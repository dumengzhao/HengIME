//! 设置中心数据层（S0-S3，docs/SETTINGS-PLAN.md）。
//!
//! 原则：只写 `*.custom.yaml` patch（自用约定：`key_binder/bindings` 等
//! 整键拥有，patch 语义 = 覆盖），写入后由调用方触发 `engine.redeploy()`。
//! 读走 librime staging（config API，已合并 patch 的生效值）。

use crate::engine::Engine;
use serde_yaml::{Mapping, Value};
use std::path::{Path, PathBuf};

pub const PATCH_DEFAULT: &str = "default.custom.yaml";
pub const PATCH_WEASEL: &str = "weasel.custom.yaml";

pub struct Settings {
    user_dir: PathBuf,
}

impl Settings {
    pub fn new(engine: &Engine) -> Self {
        Self {
            user_dir: engine.user_data_dir().to_path_buf(),
        }
    }

    /// 诊断/测试用：直接指定目录
    pub fn with_dir(dir: PathBuf) -> Self {
        Self { user_dir: dir }
    }

    pub fn user_dir(&self) -> &Path {
        &self.user_dir
    }

    fn patch_path(&self, file: &str) -> PathBuf {
        self.user_dir.join(file)
    }

    /// 读 patch 文件为 Mapping；不存在/解析失败 → 空（失败静默 = 回退默认）
    fn load_patch(&self, file: &str) -> Mapping {
        match std::fs::read_to_string(self.patch_path(file)) {
            Ok(text) => serde_yaml::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.as_mapping().cloned())
                .unwrap_or_default(),
            Err(_) => Mapping::new(),
        }
    }

    fn save_patch(&self, file: &str, map: &Mapping) -> std::io::Result<()> {
        let text = serde_yaml::to_string(&Value::Mapping(map.clone()))
            .map_err(std::io::Error::other)?;
        std::fs::write(self.patch_path(file), text)
    }

    /// 写单键路径（"a/b/c"），保留 patch 内其余键
    pub fn set_path(&self, file: &str, path: &str, value: Value) -> std::io::Result<()> {
        let mut map = self.load_patch(file);
        merge_at(&mut map, &path.split('/').collect::<Vec<_>>(), value);
        self.save_patch(file, &map)
    }

    /// 删除键路径（回退上游默认）
    pub fn remove_path(&self, file: &str, path: &str) -> std::io::Result<()> {
        let mut map = self.load_patch(file);
        remove_at(&mut map, &path.split('/').collect::<Vec<_>>());
        self.save_patch(file, &map)
    }

    pub fn get_path(&self, file: &str, path: &str) -> Option<Value> {
        let mut node = Value::Mapping(self.load_patch(file));
        for seg in path.split('/') {
            node = node.get(seg)?.clone();
        }
        Some(node)
    }

    // ---------- 语义化 setter（S1 样式 / S2 方案快捷键） ----------

    /// 配色方案（weasel.custom.yaml style/color_scheme）
    pub fn set_color_scheme(&self, id: &str) -> std::io::Result<()> {
        self.set_path(PATCH_WEASEL, "style/color_scheme", Value::from(id))
    }

    /// 内嵌编码（inline preedit）
    pub fn set_inline_preedit(&self, on: bool) -> std::io::Result<()> {
        self.set_path(PATCH_WEASEL, "style/inline_preedit", Value::from(on))
    }

    /// 候选字号
    pub fn set_font_point_size(&self, n: i64) -> std::io::Result<()> {
        self.set_path(PATCH_WEASEL, "style/font_point_size", Value::from(n))
    }

    /// 启用方案列表（default.custom.yaml schema_list；第一个 = 默认方案）
    pub fn set_schema_list(&self, ids: &[String]) -> std::io::Result<()> {
        let list: Vec<Value> = ids
            .iter()
            .map(|id| {
                let mut m = Mapping::new();
                m.insert(Value::from("schema"), Value::from(id.clone()));
                Value::Mapping(m)
            })
            .collect();
        self.set_path(PATCH_DEFAULT, "schema_list", Value::from(list))
    }

    /// key_binder/bindings（自用约定：整键拥有，见 SETTINGS-PLAN.md §2）
    pub fn set_bindings(&self, bindings: Value) -> std::io::Result<()> {
        self.set_path(PATCH_DEFAULT, "key_binder/bindings", bindings)
    }

    /// 读整键 bindings（当前生效值经 config API；patch 内容经此文件）
    pub fn get_bindings(&self) -> Option<Value> {
        self.get_path(PATCH_DEFAULT, "key_binder/bindings")
    }

    /// 用户目录下文件名列表（词库页展示）
    pub fn list_user_files(&self) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.user_dir) {
            for e in rd.flatten() {
                if let Ok(md) = e.metadata() {
                    if md.is_file() {
                        let name = e.file_name().to_string_lossy().into_owned();
                        // 词库页只展示方案/词库/配置类文件
                        if !(name.ends_with(".yaml") || name.ends_with(".txt")) {
                            continue;
                        }
                        out.push((name, md.len()));
                    }
                }
            }
        }
        out.sort();
        out
    }
}

/// 沿路径创建/合并 Mapping 节点，末段赋值（切片递归，避免迭代器链类型爆炸）
fn merge_at(map: &mut Mapping, segs: &[&str], value: Value) {
    let Some(first) = segs.first() else { return };
    let key = Value::from(*first);
    if segs.len() == 1 {
        map.insert(key, value);
        return;
    }
    let child = map
        .entry(key)
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    if !child.is_mapping() {
        *child = Value::Mapping(Mapping::new());
    }
    if let Value::Mapping(m) = child {
        merge_at(m, &segs[1..], value);
    }
}

/// 沿路径删除节点（父节点变空即连锁删除，保持 patch 干净）
fn remove_at(map: &mut Mapping, segs: &[&str]) {
    let Some(first) = segs.first() else { return };
    let key = Value::from(*first);
    if segs.len() == 1 {
        map.remove(&key);
        return;
    }
    if let Some(Value::Mapping(m)) = map.get_mut(&key) {
        remove_at(m, &segs[1..]);
        if m.is_empty() {
            map.remove(&key);
        }
    }
}

// ---------- 快捷键预设（S2；整键拥有 bindings，见 SETTINGS-PLAN.md §2） ----------

/// 翻页键预设：[(显示名, accept 上一页, accept 下一页)]
pub const PAGING_PRESETS: &[(&str, &str, &str)] = &[
    ("减号 等号（- / =）", "Minus", "Equal"),
    ("逗号 句号（, / .）", "comma", "period"),
    ("方括号（[ / ]）", "bracketleft", "bracketright"),
];
/// 左右键行为预设：[(显示名, 是否启用 has_menu Left→Up/Right→Down)]
pub const LR_PRESETS: &[(&str, bool)] = &[
    ("左右键切换候选", true),
    ("左右键移动光标（默认）", false),
];
/// 简繁切换键预设：[(显示名, accept 或 空=禁用)]
pub const SIMP_PRESETS: &[(&str, &str)] = &[
    ("Ctrl+Shift+4（默认）", "Control+Shift+4"),
    ("Ctrl+Shift+F", "Control+Shift+F"),
    ("禁用", ""),
];
/// 标点切换键预设：[(显示名, accept 或 空=禁用)]
pub const PUNCT_PRESETS: &[(&str, &str)] = &[
    ("Ctrl+句号（默认）", "Control+period"),
    ("Ctrl+Shift+P", "Control+Shift+P"),
    ("禁用", ""),
];
/// 全半角切换键预设：[(显示名, accept 或 空=禁用)]
pub const FULL_PRESETS: &[(&str, &str)] = &[
    ("Shift+空格（默认）", "Shift+space"),
    ("禁用", ""),
];

/// 组装整键 bindings：
/// 基础 = 左右键切候选（lr_on 时）+ 翻页 + 简繁/标点/全半角切换键。
/// 现实对照：这些是自用输入法的全部自定义键位，覆盖式 patch 可预期。
pub fn build_bindings(
    lr_on: bool,
    paging: usize,
    simp: usize,
    punct: usize,
    full: usize,
) -> Value {
    let mut out: Vec<Value> = Vec::new();
    fn push_toggle(out: &mut Vec<Value>, accept: &str, option: &str) {
        if accept.is_empty() {
            return;
        }
        let mut m = Mapping::new();
        m.insert(Value::from("when"), Value::from("always"));
        m.insert(Value::from("accept"), Value::from(accept));
        m.insert(Value::from("toggle"), Value::from(option));
        out.push(Value::Mapping(m));
    }
    if lr_on {
        for (accept, send) in [("Left", "Up"), ("Right", "Down")] {
            let mut m = Mapping::new();
            m.insert(Value::from("when"), Value::from("has_menu"));
            m.insert(Value::from("accept"), Value::from(accept));
            m.insert(Value::from("send"), Value::from(send));
            out.push(Value::Mapping(m));
        }
    }
    if let Some((_, up, down)) = PAGING_PRESETS.get(paging) {
        for (accept, send) in [(*up, "Page_Up"), (*down, "Page_Down")] {
            let mut m = Mapping::new();
            m.insert(Value::from("when"), Value::from("has_menu"));
            m.insert(Value::from("accept"), Value::from(accept));
            m.insert(Value::from("send"), Value::from(send));
            out.push(Value::Mapping(m));
        }
    }
    if let Some((_, accept)) = SIMP_PRESETS.get(simp) {
        push_toggle(&mut out, accept, "simplification");
    }
    if let Some((_, accept)) = PUNCT_PRESETS.get(punct) {
        push_toggle(&mut out, accept, "ascii_punct");
    }
    if let Some((_, accept)) = FULL_PRESETS.get(full) {
        push_toggle(&mut out, accept, "full_shape");
    }
    Value::from(out)
}

/// 从整键 bindings 反推各预设索引（设置页初始态）。
/// bindings 键存在时我们总是写全四个 toggle，缺失 = 禁用；
/// 键不存在 = 从未配置过 → 全部取默认（索引 0）。
pub fn parse_bindings(v: Option<&Value>) -> (bool, usize, usize, usize, usize) {
    let Some(Value::Sequence(seq)) = v else {
        return (false, 0, 0, 0, 0);
    };
    let disabled = |t: &[(&str, &str)]| {
        t.iter().position(|(_, a)| a.is_empty()).unwrap_or(0)
    };
    let mut simp = disabled(SIMP_PRESETS);
    let mut punct = disabled(PUNCT_PRESETS);
    let mut full = disabled(FULL_PRESETS);
    let mut paging = 0;
    let mut lr_on = false;
    for item in seq {
        let Value::Mapping(m) = item else { continue };
        let get = |k: &str| {
            m.get(Value::from(k)).and_then(|v| v.as_str()).unwrap_or("")
        };
        let (accept, send, toggle) = (get("accept"), get("send"), get("toggle"));
        if toggle == "simplification" {
            simp = SIMP_PRESETS
                .iter()
                .position(|(_, a)| !a.is_empty() && *a == accept)
                .unwrap_or(0);
        } else if toggle == "ascii_punct" {
            punct = PUNCT_PRESETS
                .iter()
                .position(|(_, a)| !a.is_empty() && *a == accept)
                .unwrap_or(0);
        } else if toggle == "full_shape" {
            full = FULL_PRESETS
                .iter()
                .position(|(_, a)| !a.is_empty() && *a == accept)
                .unwrap_or(0);
        } else if send == "Page_Up" {
            paging = PAGING_PRESETS
                .iter()
                .position(|(_, a, _)| *a == accept)
                .unwrap_or(0);
        } else if send == "Up" && accept == "Left" {
            lr_on = true;
        }
    }
    (lr_on, paging, simp, punct, full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_roundtrip() {
        let dir = std::env::temp_dir().join("heng-set-test");
        std::fs::create_dir_all(&dir).unwrap();
        let s = Settings {
            user_dir: dir.clone(),
        };
        s.set_path(PATCH_DEFAULT, "menu/page_size", Value::from(9))
            .unwrap();
        s.set_path(PATCH_DEFAULT, "style/font_point_size", Value::from(16))
            .unwrap();
        assert_eq!(
            s.get_path(PATCH_DEFAULT, "menu/page_size").unwrap().as_i64(),
            Some(9)
        );
        assert_eq!(
            s.get_path(PATCH_DEFAULT, "style/font_point_size")
                .unwrap()
                .as_i64(),
            Some(16)
        );
        s.remove_path(PATCH_DEFAULT, "menu/page_size").unwrap();
        assert!(s.get_path(PATCH_DEFAULT, "menu/page_size").is_none());
        assert!(s.get_path(PATCH_DEFAULT, "style/font_point_size").is_some());
        std::fs::remove_file(dir.join(PATCH_DEFAULT)).unwrap();
    }

    #[test]
    fn bindings_roundtrip() {
        let v = build_bindings(true, 0, 1, 0, 0);
        let (lr, paging, simp, punct, full) = parse_bindings(Some(&v));
        assert!(lr && paging == 0 && simp == 1 && punct == 0 && full == 0);
        let v = build_bindings(false, 2, 0, 2, 1);
        let (lr, paging, simp, punct, full) = parse_bindings(Some(&v));
        assert!(!lr && paging == 2 && simp == 0 && punct == 2 && full == 1);
    }
}
