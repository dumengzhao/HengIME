// heng-fcitx5 —— HengIME 的 fcitx5 外壳（M-P1 自绘候选窗版）。
//
// 职责（docs/ROUTE-P.md §2）：按键/事件转发 + 会话生命周期挂钩 + 取 UI 提交。
// 候选窗由 core 内部自绘（heng_ui_sync，Slint + override-redirect），外壳不再
// 使用 classicui 候选列表；组合串（preedit）仍走 fcitx5 原生内联显示。
//
// 会话模型：每个 InputContext 一个 heng 会话。
// 热路径：keyEvent → heng_process_key_ex 单调用取回 commit + context（v5 ABI）。

#include <fcitx/addonfactory.h>
#include <fcitx/event.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputcontextmanager.h>
#include <fcitx/inputmethodengine.h>
#include <fcitx/inputmethodentry.h>
#include <fcitx/inputpanel.h>
#include <fcitx/instance.h>
#include <fcitx/text.h>
#include <fcitx/userinterfacemanager.h>
#include <fcitx-utils/event.h>
#include <fcitx-utils/key.h>

#include <cstdio>
#include <unordered_map>

extern "C" {
#include "heng.h"
}

namespace fcitx {

namespace {

// fcitx5 KeyState -> X modifier mask（librime 沿用 X 修饰键位）：
// Shift=1<<0 Control=1<<2 Alt(Mod1)=1<<3 Super(Mod4)=1<<6
int maskFromStates(KeyStates states) {
    int mask = 0;
    if (states.test(KeyState::Shift)) mask |= 1 << 0;
    if (states.test(KeyState::Ctrl)) mask |= 1 << 2;
    if (states.test(KeyState::Alt)) mask |= 1 << 3;
    if (states.test(KeyState::Super)) mask |= 1 << 6;
    return mask;
}

// ICUUID（16 字节数组）→ 会话表键
uint64_t icKey(const ICUUID &uuid) {
    uint64_t h = 1469598103934665603ull;
    for (auto b : uuid) {
        h ^= b;
        h *= 1099511628211ull;
    }
    return h;
}

Text preeditText(const HengContext &ctx) {
    Text text(ctx.preedit ? ctx.preedit : "");
    if (ctx.preedit) {
        text.setCursor(ctx.cursor_pos);
    }
    return text;
}

} // namespace

class HengEngine : public InputMethodEngineV2 {
public:
    HengEngine(Instance *instance);
    ~HengEngine() override;

    void activate(const InputMethodEntry &entry, InputContextEvent &event) override;
    void deactivate(const InputMethodEntry &entry, InputContextEvent &event) override;
    void keyEvent(const InputMethodEntry &entry, KeyEvent &keyEvent) override;
    void reset(const InputMethodEntry &entry, InputContextEvent &event) override;

private:
    heng_session_t sessionFor(InputContext *ic);
    void endSession(InputContext *ic);
    // 取走 core 自绘候选窗点击产生的待上屏文本
    void drainUiCommit(InputContext *ic, heng_session_t session);
    // 更新组合串（内联）+ 同步自绘候选窗到光标位置
    void updateUI(InputContext *ic, heng_session_t session);

    Instance *instance_;
    std::unordered_map<uint64_t, heng_session_t> sessions_;
    std::unordered_map<heng_session_t, InputContext *> ics_;
    // Shift 独按 → 中英切换：Shift 按下标记，期间无其它按键则释放时切换
    std::unordered_map<uint64_t, bool> shift_pending_;
    int last_x_ = 200;
    int last_y_ = 500;
};

HengEngine::HengEngine(Instance *instance) : instance_(instance) {
    // 引擎初始化失败不致命：打字时 keyEvent 会给出提示路径（试用版直接在日志可见）
    if (heng_create(HENG_SHARED_DIR, HENG_USER_DIR) != 0) {
        const char *err = heng_last_error();
        std::fprintf(stderr, "[heng] heng_create 失败: %s\n", err ? err : "(null)");
    }
    instance_->watchEvent(
        EventType::InputContextDestroyed, EventWatcherPhase::PreInputMethod,
        [this](Event &event) {
            endSession(static_cast<InputContextDestroyedEvent &>(event).inputContext());
        });
    // 焦点离开任何输入上下文时隐藏自绘候选窗
    instance_->watchEvent(
        EventType::InputContextFocusOut, EventWatcherPhase::PreInputMethod,
        [](Event &) { heng_ui_hide(); });
    // 周期取走 UI 点击产生的提交（点击发生在引擎内部，无按键事件伴随）
    instance_->eventLoop().addTimeEvent(
        CLOCK_MONOTONIC, now(CLOCK_MONOTONIC) + 100000, 90000,
        [this](EventSourceTime *, uint64_t) {
            for (auto &[session, ic] : ics_) {
                drainUiCommit(ic, session);
            }
            return true; // 周期重复
        });
}

HengEngine::~HengEngine() {
    heng_ui_hide();
    for (auto &[id, session] : sessions_) {
        heng_end_session(session);
    }
    heng_destroy();
}

heng_session_t HengEngine::sessionFor(InputContext *ic) {
    auto key = icKey(ic->uuid());
    auto it = sessions_.find(key);
    if (it != sessions_.end()) {
        return it->second;
    }
    heng_session_t session = heng_start_session("fcitx5");
    if (session != 0) {
        sessions_[key] = session;
        ics_[session] = ic;
    }
    return session;
}

void HengEngine::endSession(InputContext *ic) {
    auto key = icKey(ic->uuid());
    auto it = sessions_.find(key);
    if (it != sessions_.end()) {
        heng_end_session(it->second);
        ics_.erase(it->second);
        sessions_.erase(it);
    }
    shift_pending_.erase(key);
}

void HengEngine::activate(const InputMethodEntry &, InputContextEvent &event) {
    // 拿焦点时提前建会话，首个按键零开销
    sessionFor(event.inputContext());
}

void HengEngine::deactivate(const InputMethodEntry &, InputContextEvent &event) {
    auto *ic = event.inputContext();
    if (auto it = sessions_.find(icKey(ic->uuid())); it != sessions_.end()) {
        heng_clear(it->second);
    }
    heng_ui_hide();
    ic->inputPanel().reset();
    ic->updateUserInterface(UserInterfaceComponent::InputPanel);
}

void HengEngine::reset(const InputMethodEntry &, InputContextEvent &event) {
    auto *ic = event.inputContext();
    if (auto it = sessions_.find(icKey(ic->uuid())); it != sessions_.end()) {
        heng_clear(it->second);
    }
    heng_ui_hide();
    ic->inputPanel().reset();
    ic->updateUserInterface(UserInterfaceComponent::InputPanel);
}

void HengEngine::keyEvent(const InputMethodEntry &, KeyEvent &keyEvent) {
    auto *ic = keyEvent.inputContext();
    auto key = keyEvent.key();
    auto ickey = icKey(ic->uuid());

    // Shift 独按释放 → 中英切换（ascii_mode 经 heng_set_option，含传播策略）
    if (key.isModifier()) {
        if (key.sym() == FcitxKey_Shift_L || key.sym() == FcitxKey_Shift_R) {
            if (keyEvent.isRelease()) {
                if (shift_pending_[ickey]) {
                    shift_pending_[ickey] = false;
                    heng_session_t session = sessionFor(ic);
                    if (session != 0) {
                        int cur = heng_get_option(session, "ascii_mode");
                        int next = cur == HENG_TRUE ? HENG_FALSE : HENG_TRUE;
                        heng_set_option(session, "ascii_mode", next);
                        // 切换瞬态提示（core 自绘：大字「中/A」约 1 秒）
                        heng_ui_mode_hint(next);
                        ic->updateUserInterface(UserInterfaceComponent::InputPanel);
                    }
                    keyEvent.filterAndAccept();
                }
            } else {
                shift_pending_[ickey] = true;
            }
        }
        return; // 其余修饰键不进引擎
    }

    // 非修饰键打断 Shift 独按序列
    shift_pending_[ickey] = false;
    if (keyEvent.isRelease()) {
        return;
    }

    heng_session_t session = sessionFor(ic);
    if (session == 0) {
        const char *err = heng_last_error();
        std::fprintf(stderr, "[heng] 会话创建失败: %s\n", err ? err : "(null)");
        return; // 未处理 -> fcitx5 透传给应用
    }

    // 取走候选窗点击产生的提交（点击无按键事件伴随）
    drainUiCommit(ic, session);

    HengContext ctx = {0};
    ctx.data_size = sizeof(HengContext);
    char *commit = nullptr;
    int handled = heng_process_key_ex(session, static_cast<int>(key.sym()),
                                      maskFromStates(key.states()), &commit, &ctx);
    if (!handled) {
        // 引擎不处理（数字/空组串下的退格回车/标点直通等）-> 透传给应用
        heng_free_context(&ctx);
        return;
    }
    keyEvent.filterAndAccept();

    if (commit) {
        ic->commitString(commit);
        heng_free_string(commit);
    }
    heng_free_context(&ctx);
    updateUI(ic, session);
}

void HengEngine::drainUiCommit(InputContext *ic, heng_session_t session) {
    char *text = nullptr;
    if (heng_take_ui_commit(session, &text) == HENG_TRUE && text) {
        ic->commitString(text);
        heng_free_string(text);
        ic->updateUserInterface(UserInterfaceComponent::InputPanel);
    }
}

void HengEngine::updateUI(InputContext *ic, heng_session_t session) {
    // 只喂 client preedit（应用内联显示）；不再设置 panel preedit / 候选列表，
    // classicui 就没有任何要画的内容（候选窗由 core 自绘接管）
    auto &panel = ic->inputPanel();
    panel.reset();

    HengContext ctx = {0};
    ctx.data_size = sizeof(HengContext);
    if (heng_get_context(session, &ctx) == HENG_TRUE && ctx.preedit) {
        panel.setClientPreedit(preeditText(ctx));
    }
    heng_free_context(&ctx);
    ic->updateUserInterface(UserInterfaceComponent::InputPanel);

    // 自绘候选窗跟随光标（fcitx5 上报光标区域；无有效区域用上次位置）
    auto rect = ic->cursorRect();
    if (rect.left() != 0 || rect.top() != 0 || rect.right() != 0 || rect.bottom() != 0) {
        last_x_ = rect.left();
        last_y_ = rect.bottom() + 8;
    }
    heng_ui_sync(session, last_x_, last_y_);
}

} // namespace fcitx

class HengEngineFactory : public fcitx::AddonFactory {
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override {
        return new fcitx::HengEngine(manager->instance());
    }
};

FCITX_ADDON_FACTORY(HengEngineFactory)
