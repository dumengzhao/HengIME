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
