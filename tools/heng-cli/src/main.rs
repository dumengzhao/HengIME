//! heng-cli —— HengIME 命令行校验器（M0）。
//!
//! 结构对照 librime 自带的 `tools/rime_api_console.cc` 与社区 rsime 的做法：
//! 用 `simulate_key_sequence` 驱动引擎，验证部署、候选、上屏与热路径延迟。
//!
//! 用法（在仓库根目录运行）：
//!   heng-cli version              打印 librime 版本与已装方案
//!   heng-cli cand <seq>           模拟按键，打印候选（不上屏）
//!   heng-cli commit <seq>         模拟按键 + 空格，打印上屏文本
//!   heng-cli bench <seq> [n]      热路径延迟基线（默认 1000 次）
//!   heng-cli serve [port]         启动本机 HTTP 服务（默认 127.0.0.1:9371）
//!   heng-cli abitest              直接调 C ABI（heng_* 导出）做冒烟测试
//!
//! 运行时目录：`HENG_RUNTIME` 环境变量可覆盖，默认 `./runtime`。

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use heng_core::{Engine, EngineConfig};

const USAGE: &str = "用法: heng-cli <version|cand|commit|bench|serve|abitest> [参数]";
const DEFAULT_PORT: u16 = 9371;

fn runtime_dir() -> PathBuf {
    std::env::var_os("HENG_RUNTIME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("runtime"))
}

fn make_engine() -> Result<Engine, Box<dyn std::error::Error>> {
    let base = runtime_dir();
    let config = EngineConfig {
        shared_data_dir: Some(base.join("rime-shared")),
        user_data_dir: base.join("rime-user"),
        // 0=INFO 1=WARNING 2=ERROR 3=FATAL，M0 只关心错误
        min_log_level: 2,
    };
    Ok(Engine::new(config)?)
}

fn print_context(ctx: &heng_core::ContextSnapshot) {
    println!("preedit : {}", ctx.preedit);
    println!(
        "page    : 第 {} 页 / 高亮 {} / 末页 {}",
        ctx.page_no + 1,
        ctx.highlighted,
        ctx.is_last_page
    );
    let keys: Vec<char> = ctx.select_keys.chars().collect();
    for (i, c) in ctx.candidates.iter().enumerate() {
        let key = keys.get(i).map_or(' ', |&k| k);
        match &c.comment {
            Some(comment) if !comment.is_empty() => {
                println!("  [{key}] {}   ({comment})", c.text)
            }
            _ => println!("  [{key}] {}", c.text),
        }
    }
    if let Some(preview) = &ctx.commit_text_preview {
        println!("preview : {preview}");
    }
}

fn cmd_version(engine: &Engine) {
    println!("librime : {}", engine.version().unwrap_or_else(|| "?".into()));
    println!("已装方案:");
    for (id, name) in engine.schema_list() {
        println!("  {id:<28} {name}");
    }
}

fn cmd_cand(engine: &Engine, seq: &str) -> Result<(), Box<dyn std::error::Error>> {
    let session = engine.create_session()?;
    if !session.simulate(seq) {
        return Err(format!("按键序列 {seq:?} 存在未被处理的按键").into());
    }
    match session.context() {
        Some(ctx) => print_context(&ctx),
        None => println!("（无组合串状态）"),
    }
    Ok(())
}

fn cmd_commit(engine: &Engine, seq: &str) -> Result<(), Box<dyn std::error::Error>> {
    let session = engine.create_session()?;
    let full = format!("{seq}{{space}}");
    if !session.simulate(&full) {
        return Err(format!("按键序列 {full:?} 存在未被处理的按键").into());
    }
    match session.commit_text() {
        Some(text) => println!("上屏: {text}"),
        None => return Err("没有产生上屏文本".into()),
    }
    Ok(())
}

fn cmd_bench(engine: &Engine, seq: &str, n: usize) -> Result<(), Box<dyn std::error::Error>> {
    let session = engine.create_session()?;

    // 预热一次：首次按键会触发词典加载等惰性初始化
    session.simulate(seq);
    session.clear();

    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        session.clear();
        let t0 = Instant::now();
        session.simulate(seq);
        let _ = session.context();
        samples.push(t0.elapsed());
    }
    samples.sort_unstable();

    let avg: std::time::Duration = samples.iter().sum::<std::time::Duration>() / n as u32;
    let min = samples[0];
    let max = samples[n - 1];
    let p95 = samples[(n as f64 * 0.95) as usize % n];

    println!("序列    : {seq:?}");
    println!("次数    : {n}");
    println!("平均    : {:.2?} µs", avg.as_nanos() as f64 / 1000.0);
    println!("最小    : {:.2?} µs", min.as_nanos() as f64 / 1000.0);
    println!("p95     : {:.2?} µs", p95.as_nanos() as f64 / 1000.0);
    println!("最大    : {:.2?} µs", max.as_nanos() as f64 / 1000.0);
    Ok(())
}

fn cmd_serve(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(heng_core::server::serve(port))?;
    Ok(())
}

/// C ABI 冒烟测试：直接以 `extern "C"` 声明调用 heng_* 导出符号，
/// 验证 cdylib/rlib 导出面与 include/heng.h 的一致性。
mod abi {
    use std::os::raw::{c_char, c_int};

    pub type HengSession = u64;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct HengContext {
        pub data_size: c_int,
        pub preedit: *mut c_char,
        pub cursor_pos: c_int,
        pub candidates: *mut *mut c_char,
        pub comments: *mut *mut c_char,
        pub candidate_count: c_int,
        pub highlighted: c_int,
        pub page_no: c_int,
        pub is_last_page: c_int,
        pub commit_text_preview: *mut c_char,
        pub labels: *mut *mut c_char,
        pub sel_start: c_int,
        pub sel_end: c_int,
        pub select_keys: *mut c_char,
    }

    impl HengContext {
        pub const fn zeroed() -> Self {
            Self {
                data_size: 0,
                preedit: std::ptr::null_mut(),
                cursor_pos: 0,
                candidates: std::ptr::null_mut(),
                comments: std::ptr::null_mut(),
                candidate_count: 0,
                highlighted: 0,
                page_no: 0,
                is_last_page: 0,
                commit_text_preview: std::ptr::null_mut(),
                labels: std::ptr::null_mut(),
                sel_start: 0,
                sel_end: 0,
                select_keys: std::ptr::null_mut(),
            }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct HengStatus {
        pub data_size: c_int,
        pub schema_id: *mut c_char,
        pub schema_name: *mut c_char,
        pub is_ascii_mode: c_int,
        pub is_composing: c_int,
        pub is_disabled: c_int,
        pub is_full_shape: c_int,
    }

    impl HengStatus {
        pub const fn zeroed() -> Self {
            Self {
                data_size: 0,
                schema_id: std::ptr::null_mut(),
                schema_name: std::ptr::null_mut(),
                is_ascii_mode: 0,
                is_composing: 0,
                is_disabled: 0,
                is_full_shape: 0,
            }
        }
    }

    extern "C" {
        pub fn heng_create(shared: *const c_char, user: *const c_char) -> c_int;
        pub fn heng_destroy();
        pub fn heng_version() -> *const c_char;
        pub fn heng_describe() -> *const c_char;
        pub fn heng_start_session(app_id: *const c_char) -> HengSession;
        pub fn heng_end_session(session: HengSession);
        pub fn heng_process_key(session: HengSession, keysym: c_int, mask: c_int) -> c_int;
        pub fn heng_simulate_key_sequence(session: HengSession, seq: *const c_char) -> c_int;
        pub fn heng_get_context(session: HengSession, out: *mut HengContext) -> c_int;
        pub fn heng_commit_text(session: HengSession, out: *mut *mut c_char) -> c_int;
        pub fn heng_select_candidate_on_current_page(session: HengSession, index: c_int) -> c_int;
        pub fn heng_highlight_candidate_on_current_page(
            session: HengSession,
            index: c_int,
        ) -> c_int;
        pub fn heng_change_page(session: HengSession, backward: c_int) -> c_int;
        pub fn heng_set_session_owner(session: HengSession, app_id: *const c_char) -> c_int;
        pub fn heng_clear(session: HengSession);
        pub fn heng_commit_composition(session: HengSession) -> c_int;
        pub fn heng_set_option(session: HengSession, option: *const c_char, value: c_int) -> c_int;
        pub fn heng_get_option(session: HengSession, option: *const c_char) -> c_int;
        pub fn heng_get_status(session: HengSession, out: *mut HengStatus) -> c_int;
        pub fn heng_free_status(out: *mut HengStatus);
        pub fn heng_free_string(s: *mut c_char);
        pub fn heng_free_context(out: *mut HengContext);
        pub fn heng_last_error() -> *const c_char;
    }
}

fn cmd_abitest() -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::{CStr, CString};
    use std::os::raw::c_char;

    use abi::*;

    unsafe fn cstr(p: *const c_char) -> String {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }

    let mut failures = 0usize;
    macro_rules! check {
        ($name:expr, $cond:expr) => {
            if $cond {
                println!("  PASS  {}", $name);
            } else {
                println!("  FAIL  {}", $name);
                failures += 1;
            }
        };
    }

    unsafe {
        // 1. 创建引擎
        let rc = heng_create(std::ptr::null(), std::ptr::null());
        check!("heng_create(NULL,NULL) == 0", rc == 0);

        // 2. 版本与自省
        let ver = cstr(heng_version());
        println!("        version   = {ver}");
        check!("heng_version 非空", !ver.is_empty());
        let desc = cstr(heng_describe());
        check!("heng_describe 含 heng-core", desc.contains("heng-core"));

        // 3. 会话 + 模拟按键 + 取候选
        let session = heng_start_session(std::ptr::null());
        check!("heng_start_session != 0", session != 0);

        let seq = CString::new("nihao").unwrap();
        let handled = heng_simulate_key_sequence(session, seq.as_ptr());
        check!("simulate(\"nihao\") 已处理", handled == 1);

        let mut ctx = abi::HengContext::zeroed();
        let rc = heng_get_context(session, &mut ctx);
        check!("heng_get_context 成功", rc == 1);
        if rc == 1 {
            let preedit = if ctx.preedit.is_null() {
                String::new()
            } else {
                cstr(ctx.preedit)
            };
            println!("        preedit   = {preedit}");
            println!("        candidates= {}", ctx.candidate_count);
            check!("preedit == \"ni hao\"", preedit == "ni hao");
            check!("候选数 >= 1", ctx.candidate_count >= 1);
            if ctx.candidate_count >= 1 {
                let first = cstr(*ctx.candidates);
                println!("        首候选    = {first}");
                if !ctx.labels.is_null() {
                    let l = *ctx.labels;
                    if !l.is_null() {
                        println!("        首标签    = {}", cstr(l));
                    }
                }
            }
            heng_free_context(&mut ctx);
        }

        // 4. 选候选 + 取上屏（主流程，先于新 API 冒烟，避免状态污染）
        let selected = heng_select_candidate_on_current_page(session, 0);
        check!("select_candidate(0) 已选中", selected == 1);
        let mut out: *mut c_char = std::ptr::null_mut();
        let rc = heng_commit_text(session, &mut out);
        check!("heng_commit_text 成功", rc == 1);
        if !out.is_null() {
            let text = cstr(out);
            println!("        上屏      = {text}");
            check!("上屏文本非空", !text.is_empty());
            heng_free_string(out);
        } else {
            check!("上屏文本非空", false);
        }

        // 4.5 新 API 冒烟：重新输入后高亮 + 翻页 + 归属绑定 + 状态/选项，clear 收尾不留状态
        let seq = CString::new("nihao").unwrap();
        heng_simulate_key_sequence(session, seq.as_ptr());
        let hl = heng_highlight_candidate_on_current_page(session, 1);
        check!("highlight_candidate(1) 返回值合法", hl == 0 || hl == 1);
        let moved = heng_change_page(session, 0);
        check!("change_page(向后) 返回值合法", moved == 0 || moved == 1);
        let app = CString::new("abitest.exe").unwrap();
        let ascii_name = CString::new("ascii_mode").unwrap();
        let bound = heng_set_session_owner(session, app.as_ptr());
        check!("set_session_owner 绑定成功", bound == 0);

        // v3：状态快照
        let mut st = abi::HengStatus::zeroed();
        let rc = heng_get_status(session, &mut st);
        check!("heng_get_status 成功", rc == 1);
        if rc == 1 {
            let schema_id = if st.schema_id.is_null() {
                String::new()
            } else {
                cstr(st.schema_id)
            };
            println!("        schema_id = {schema_id}");
            check!("schema_id 非空", !schema_id.is_empty());
            heng_free_status(&mut st);
        }

        // v3：选项读写（用 ascii_mode 做往返，随后恢复）
        let set1 = heng_set_option(session, ascii_name.as_ptr(), 1);
        check!("set_option(ascii_mode,1) 成功", set1 == 1);
        let got = heng_get_option(session, ascii_name.as_ptr());
        check!("get_option(ascii_mode) == 1", got == 1);
        heng_set_option(session, ascii_name.as_ptr(), 0);
        let got2 = heng_get_option(session, ascii_name.as_ptr());
        check!("get_option 恢复为 0", got2 == 0);

        // v3：组合串高亮区间（nihao 全转换时 sel_start=0, sel_end>0）
        let mut ctx3 = abi::HengContext::zeroed();
        if heng_get_context(session, &mut ctx3) == 1 {
            println!(
                "        sel range = [{}, {})",
                ctx3.sel_start, ctx3.sel_end
            );
            check!("sel_end >= sel_start", ctx3.sel_end >= ctx3.sel_start);
            heng_free_context(&mut ctx3);
        }

        // v3：commit_composition（有组合串时应产生提交）
        let committed = heng_commit_composition(session);
        check!("commit_composition 已提交", committed == 1);
        let mut out2: *mut c_char = std::ptr::null_mut();
        if heng_commit_text(session, &mut out2) == 1 && !out2.is_null() {
            heng_free_string(out2);
        }
        heng_clear(session);

        // 5. 单键路径（XK_space=0x0020）：空串状态下按空格应上屏空格
        let space = heng_process_key(session, 0x0020, 0);
        check!("process_key(space) 返回值合法", space == 0 || space == 1);

        // 6. 清组合串
        let seq = CString::new("abc").unwrap();
        heng_simulate_key_sequence(session, seq.as_ptr());
        heng_clear(session);
        let mut ctx2 = abi::HengContext::zeroed();
        if heng_get_context(session, &mut ctx2) == 1 {
            let preedit = if ctx2.preedit.is_null() {
                String::new()
            } else {
                cstr(ctx2.preedit)
            };
            check!("clear 后 preedit 为空", preedit.is_empty());
            heng_free_context(&mut ctx2);
        }

        // 7. 关会话 + 错误路径
        heng_end_session(session);
        let ghost = heng_simulate_key_sequence(session, seq.as_ptr());
        check!("已关会话操作返回 FALSE", ghost == 0);
        let err = heng_last_error();
        check!("heng_last_error 非空", !err.is_null());

        // 8. 销毁
        heng_destroy();
        println!("        heng_destroy 完成");
    }

    if failures == 0 {
        println!("abitest 全部通过");
        Ok(())
    } else {
        Err(format!("{failures} 项失败").into())
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    // serve / abitest 走 global 层（自带引擎单例），不能用 make_engine 重复初始化 librime
    if args[0] == "serve" || args[0] == "abitest" {
        let result: Result<(), Box<dyn std::error::Error>> = match args[0].as_str() {
            "serve" => {
                let port: u16 = args
                    .get(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(DEFAULT_PORT);
                cmd_serve(port)
            }
            _ => cmd_abitest(),
        };
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("错误: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let engine = match make_engine() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("引擎初始化失败: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result: Result<(), Box<dyn std::error::Error>> = match args[0].as_str() {
        "version" => {
            cmd_version(&engine);
            Ok(())
        }
        "cand" => match args.get(1) {
            Some(seq) => cmd_cand(&engine, seq),
            None => Err("cand 需要按键序列参数，如 heng-cli cand nihao".into()),
        },
        "commit" => match args.get(1) {
            Some(seq) => cmd_commit(&engine, seq),
            None => Err("commit 需要按键序列参数，如 heng-cli commit nihao".into()),
        },
        "bench" => {
            let seq = args.get(1).map(String::as_str).unwrap_or("nihao");
            let n: usize = args
                .get(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(1000);
            cmd_bench(&engine, seq, n)
        }
        other => Err(format!("未知命令 {other:?}\n{USAGE}").into()),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("错误: {e}");
            ExitCode::FAILURE
        }
    }
}
