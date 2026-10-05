/*
 * heng.h —— HengIME 统一能力模块 C ABI（M0）
 *
 * 对应 core/src/capi.rs 的导出。字段顺序与 Rust 侧 #[repr(C)] 严格一致，
 * 修改任一侧必须同步另一侧并更新 heng_describe 的 abi_version。
 *
 * 内存约定：
 * - core 分配的字符串/结构体由调用方用 heng_free_string / heng_free_context 释放
 * - heng_version / heng_describe / heng_last_error 返回的指针归 core，
 *   调用方不得释放；heng_last_error 的指针在下次 heng_* 调用前有效
 *
 * 线程约定：librime 会话操作不保证线程安全，调用方须自行串行化
 * （同一会话的调用应从单一线程或外部加锁发出）。
 */
#ifndef HENG_H
#define HENG_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---- 导入/导出标注 ---- */
#if defined(_WIN32)
#  ifdef HENG_CORE_BUILD
#    define HENG_API __declspec(dllexport)
#  else
#    define HENG_API __declspec(dllimport)
#  endif
#else
#  define HENG_API __attribute__((visibility("default")))
#endif

/* 会话句柄：0 表示无效 */
typedef uint64_t heng_session_t;

/* 布尔返回值 */
#define HENG_TRUE  1
#define HENG_FALSE 0

/*
 * 组合串与候选快照。首字段 data_size 供调用方校验 ABI 版本；
 * candidates 与 comments 为平行数组，comments 中无注释的槽位为 NULL。
 * 数组与字符串内存归 core，用 heng_free_context 一次性释放。
 */
typedef struct HengContext {
    int   data_size;              /* sizeof(HengContext) */
    char* preedit;                /* 组合串（UTF-8），无则为 NULL */
    int   cursor_pos;             /* 组合串内光标位置（UTF-8 字节偏移） */
    char** candidates;            /* 候选文本数组，无候选为 NULL */
    char** comments;              /* 候选注释数组（与 candidates 平行） */
    int   candidate_count;
    int   highlighted;            /* 当前页内高亮下标（0 基） */
    int   page_no;                /* 当前页码（0 基） */
    int   is_last_page;           /* HENG_TRUE / HENG_FALSE */
    char* commit_text_preview;    /* 高亮候选对应的预上屏文本，无则为 NULL */
    /* v2：选键标签数组（与 candidates 平行，缺省槽位 NULL；调用方回退 select_keys/序号） */
    char** labels;
    /* v3：组合串高亮区间（已转换部分；sel_start == sel_end 表示无区间） */
    int   sel_start;
    int   sel_end;
    /* v3：选键序列（select_keys，如 "1234567890"；labels 缺省时的回退） */
    char* select_keys;
} HengContext;

/*
 * 会话状态快照（v3）。字符串内存归 core，用 heng_free_status 释放。
 */
typedef struct HengStatus {
    int   data_size;              /* sizeof(HengStatus) */
    char* schema_id;              /* 当前方案 ID（UTF-8），调用方不得修改 */
    char* schema_name;            /* 当前方案名（UTF-8） */
    int   is_ascii_mode;          /* HENG_TRUE / HENG_FALSE */
    int   is_composing;
    int   is_disabled;
    int   is_full_shape;
} HengStatus;

/* ---- 生命周期 ---- */

/* 初始化引擎并按需部署。传 NULL 使用默认运行时目录（环境变量 HENG_RUNTIME 或 ./runtime）。
 * 返回 0 成功，-1 失败（heng_last_error 取详情）。 */
HENG_API int heng_create(const char* shared_data_dir, const char* user_data_dir);

/* 销毁全部会话并 finalize librime。之后不得再调用任何 heng_*（M0 约束）。 */
HENG_API void heng_destroy(void);

/* 版本描述串，如 "heng-core 0.1.0 / librime 1.17.0"。 */
HENG_API const char* heng_version(void);

/* 元数据自省：返回静态 JSON（命令清单、abi_version 等）。 */
HENG_API const char* heng_describe(void);

/* ---- 会话 ---- */

/* 打开会话。app_id 仅用于登记（M0 不做按应用配置）。返回句柄，0 失败。 */
HENG_API heng_session_t heng_start_session(const char* app_id);

/* 关闭会话。 */
HENG_API void heng_end_session(heng_session_t session);

/* ---- 输入 ---- */

/* 处理一个按键。返回 HENG_TRUE=已处理，HENG_FALSE=未处理（应透传给应用）。 */
HENG_API int heng_process_key(heng_session_t session, int keysym, int mask);

/* 模拟一段按键序列（librime 语法，如 "nihao"、"nihao{space}"）。
 * 返回 HENG_TRUE=全部按键已处理。 */
HENG_API int heng_simulate_key_sequence(heng_session_t session, const char* key_sequence);

/* 取当前组合串与候选快照。返回 HENG_TRUE 后必须用 heng_free_context 释放。 */
HENG_API int heng_get_context(heng_session_t session, HengContext* out);

/* 取待上屏文本（消费语义：取后清空）。返回 HENG_TRUE 且 *out 非 NULL 表示有文本；
 * *out 为 NULL 表示本次无上屏。文本用 heng_free_string 释放。 */
HENG_API int heng_commit_text(heng_session_t session, char** out);

/* 选中当前页第 index 个候选（0 基）。返回 HENG_TRUE=已选中。 */
HENG_API int heng_select_candidate_on_current_page(heng_session_t session, int index);

/* 高亮当前页第 index 个候选（不提交；候选窗移动光标用）。 */
HENG_API int heng_highlight_candidate_on_current_page(heng_session_t session, int index);

/* 翻页。backward 非 0 表示向前翻。返回 HENG_TRUE=已翻页。 */
HENG_API int heng_change_page(heng_session_t session, int backward);

/* 绑定/改绑会话归属应用（焦点切换时调用；app_options 与开关传播的基础）。
 * 返回 0 成功，-1 失败（会话不存在）。 */
HENG_API int heng_set_session_owner(heng_session_t session, const char* app_id);

/* 清空当前组合串（保持会话与输入状态）。 */
HENG_API void heng_clear(heng_session_t session);

/* 提交当前组合串（上屏）。返回 HENG_TRUE=已产生提交。 */
HENG_API int heng_commit_composition(heng_session_t session);

/* 设置会话选项（ascii_mode / inline_preedit / vim_mode 等）。返回 HENG_TRUE=成功。 */
HENG_API int heng_set_option(heng_session_t session, const char* option, int value);

/* 读取会话选项。返回 0/1；-1 表示失败（会话不存在）。 */
HENG_API int heng_get_option(heng_session_t session, const char* option);

/* 取会话状态快照。返回 HENG_TRUE 后必须用 heng_free_status 释放。 */
HENG_API int heng_get_status(heng_session_t session, HengStatus* out);

/* 释放 heng_get_status 填充的结构体。 */
HENG_API void heng_free_status(HengStatus* out);

/* ---- 版本握手与热路径合并调用（v5；增量追加，v4 及之前全部不变） ---- */

/*
 * 握手信息。纯值结构，无堆分配，无需专配释放。
 * 调用方在任何会话操作前调用 heng_hello 一次，据 abi_version 决定
 * 能否使用更高版本新增的函数/字段（data_size 校验语义不变）。
 */
typedef struct HengHello {
    int      data_size;        /* sizeof(HengHello) */
    int      abi_version;      /* core 当前 ABI 版本（v5 = 5） */
    int      min_abi_version;  /* core 仍兼容的最低调用方 ABI */
    uint64_t reserved[2];      /* 前向预留，必须全零初始化 */
} HengHello;

/* 版本握手。client_abi_version 传调用方编译时所依的 ABI 版本，core 侧当前不校验
 * （预留：未来 core 放弃兼容旧 ABI 时据此拒绝服务）。返回 HENG_TRUE=已填充。 */
HENG_API int heng_hello(int client_abi_version, HengHello* out);

/*
 * 热路径合并调用：处理一个按键并一次性取回待上屏文本与组合串候选快照，
 * 替代 process_key → commit_text → get_context 的多次跨进程往返
 * （Windows 命名管道下每次按键由 3 次 IPC 降为 1 次）。
 * out_commit / out_ctx 均可传 NULL 表示不需要。
 * 返回 HENG_TRUE=按键已处理；out_commit 采用消费语义（取后清空），
 * 非 NULL 时用 heng_free_string 释放；out_ctx 非 NULL 时用 heng_free_context 释放。
 */
HENG_API int heng_process_key_ex(heng_session_t session, int keysym, int mask,
                                 char** out_commit, HengContext* out_ctx);

/* ---- 行为统一层（v6；增量追加，v5 及之前全部不变） ----
 *
 * 路线 P 的核心：开关传播、按应用初始选项（app_options）、跨重启选项记忆
 * 全部收进 core，外壳不再各自实现「开关记住范围」。
 * - heng_set_option：按传播策略广播到其它会话，名单内选项（ascii_mode/full_shape/
 *   simplification/ascii_punct）自动持久化到用户目录 heng_options.yaml
 * - heng_start_session：自动应用持久化选项初值
 * - heng_set_session_owner：自动应用 heng.yaml 的 app_options/<app>/ 初始选项
 *   （app 覆盖持久化值；不重放持久化，避免覆盖会话实时状态）
 * 策略初值来自 heng.yaml 的 propagation/policy（per_session | per_app | global）。
 */

#define HENG_POLICY_PER_SESSION 0
#define HENG_POLICY_PER_APP     1
#define HENG_POLICY_GLOBAL      2

/* 设置传播策略。返回 0 成功，-1 非法值；运行时设置优先于 heng.yaml 初值。 */
HENG_API int heng_set_propagation_policy(int policy);

/* 读取当前传播策略。返回 HENG_POLICY_*；-1 引擎未初始化。 */
HENG_API int heng_get_propagation_policy(void);

/* ---- 自绘候选窗（v6；运行于 core 内部 UI 线程，X11/XWayland） ----
 *
 * 路线 P 的 L2 候选窗：悬浮提示与选中语义由 core 定义（无 classicui 悬浮
 * 歧义）。点击候选由 core 直接选词，产生的上屏文本经 heng_take_ui_commit
 * 由外壳取走（外壳应在按键处理与周期轮询中调用）。
 * x11 不可用时以下调用静默失败，外壳应回退宿主候选窗。
 */

/* 同步候选窗：取会话 context，有候选则显示于屏幕 (x,y)（光标左下角）并刷新，
 * 无候选则隐藏。返回 HENG_TRUE=已同步，HENG_FALSE=UI 不可用。 */
HENG_API int heng_ui_sync(heng_session_t session, int x, int y);

/* 隐藏候选窗（焦点离开时调用）。 */
HENG_API int heng_ui_hide(void);

/* 取走 UI 点击产生的待上屏文本（消费语义）。返回 HENG_TRUE 且 *out 非 NULL
 * 表示有文本（heng_free_string 释放）；否则 *out 为 NULL。 */
HENG_API int heng_take_ui_commit(heng_session_t session, char** out);

/* ---- 配置读取（v4；librime RimeConfig 直通） ----
 *
 * 用途：外壳加载 UI 样式（weasel.yaml 的 preset_color_schemes）与
 * app_options（default.yaml）等部署配置。句柄 0/NULL 表示无效。
 * 所有调用应从同一线程发出（与 heng 会话操作一致）。
 */

/* 遍历器：前 5 字段与 librime RimeConfigIterator 二进制兼容；
 * key/path 指向 librime 内部内存，heng_config_end/next 前有效。 */
typedef struct HengConfigIterator {
    void* list;
    void* map;
    int   index;
    const char* key;
    const char* path;
    uint64_t reserved[4];   /* 前向预留，必须全零初始化 */
} HengConfigIterator;

/* 配置句柄（librime RimeConfig 内部指针直通）；NULL 表示无效。 */
typedef void* heng_config_t;

/* 打开配置（如 "weasel"、"default"）。返回句柄，NULL 失败。 */
HENG_API heng_config_t heng_config_open(const char* config_id);

/* 关闭配置。之后句柄失效。 */
HENG_API void heng_config_close(heng_config_t config);

/* 读字符串。
 * 返回 >0 且 < buf_len：已写入 buf 的字节数（不含 NUL）；
 * 返回 > buf_len：键存在但缓冲不足（未写入；返回所需长度含 NUL）；
 * 返回 0：键不存在或参数无效。 */
HENG_API int heng_config_get_string(heng_config_t config, const char* key,
                                    char* buf, int buf_len);

/* 读整数。返回 HENG_TRUE=命中（*out 已填），HENG_FALSE=未命中。 */
HENG_API int heng_config_get_int(heng_config_t config, const char* key, int* out);

/* 读布尔。返回 HENG_TRUE=命中（*out 已填 0/1），HENG_FALSE=未命中。 */
HENG_API int heng_config_get_bool(heng_config_t config, const char* key, int* out);

/* 开始遍历 key 下直属子键（map）。返回 HENG_TRUE=成功（iter 已初始化）。
 * iter 须由调用方分配并全零初始化。 */
HENG_API int heng_config_begin_map(heng_config_t config, const char* key,
                                   HengConfigIterator* iter);

/* 遍历下一项。返回 HENG_TRUE=有项（iter.key/iter.path 可读）。 */
HENG_API int heng_config_next(HengConfigIterator* iter);

/* 结束遍历，释放迭代器资源。 */
HENG_API void heng_config_end(HengConfigIterator* iter);

/* ---- Squirrel 外壳所需（v7；增量追加，v6 及之前全部不变） ---- */

/* 取当前组合串的原始输入（如拼音串，只读快照）。返回 TRUE 且 *out 非 NULL
 * 表示有输入（heng_free_string 释放）；*out 为 NULL 表示无组合。 */
HENG_API int heng_get_input(heng_session_t session, char** out);

/* 组合串内光标位置（UTF-8 字节偏移）。无组合/失败返回 0。 */
HENG_API int heng_get_caret_pos(heng_session_t session);

/* 设置组合串内光标位置（UTF-8 字节偏移）。返回 HENG_TRUE=已设置。 */
HENG_API int heng_set_caret_pos(heng_session_t session, int pos);

/* 用户数据同步（Rime sync 机制）。返回 HENG_TRUE=成功。 */
HENG_API int heng_sync_user_data(void);

/* 选项状态标签（abbreviated 非 0 取短标签；菜单栏图标/浮窗用）。
 * 返回 >0 且 < buf_len：已写入字节数（不含 NUL）；
 * 返回 > buf_len：缓冲不足（返回所需长度含 NUL）；
 * 返回 0：无标签 / 参数无效。 */
HENG_API int heng_get_state_label_abbreviated(heng_session_t session,
                                              const char* option, int state,
                                              int abbreviated, char* buf,
                                              int buf_len);

/* 打开方案的已部署配置（如 "luna_pinyin"，方案级样式覆盖）。返回句柄，NULL 失败。 */
HENG_API heng_config_t heng_config_open_schema(const char* schema_id);

/* 读浮点。返回 HENG_TRUE=命中（*out 已填），HENG_FALSE=未命中。 */
HENG_API int heng_config_get_double(heng_config_t config, const char* key,
                                    double* out);

/* ---- 内存释放 ---- */

HENG_API void heng_free_string(char* s);
HENG_API void heng_free_context(HengContext* out);

/* ---- 错误 ---- */

/* 最后一次错误的描述（UTF-8）。无错误时返回 NULL。 */
HENG_API const char* heng_last_error(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* HENG_H */
