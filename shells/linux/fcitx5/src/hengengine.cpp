// heng-fcitx5 —— HengIME 的 fcitx5 外壳（试用版）。
//
// 定位（docs/ARCHITECTURE.md M7/L1 路线）：fcitx5 做「按键转发 + 候选窗渲染」，
// 全部上层逻辑来自 heng-core（同进程 C ABI 直链，Linux 上候选窗与引擎同进程无妨）。
//
// 会话模型：每个 InputContext 一个 heng 会话（对齐 fcitx5-rime 的会话池语义）。
// 热路径：keyEvent → heng_process_key_ex 单调用取回 commit + context（v5 ABI）。

#include <fcitx/addonfactory.h>
#include <fcitx/event.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputcontextmanager.h>
#include <fcitx/inputmethodengine.h>
#include <fcitx/inputmethodentry.h>
#include <fcitx/inputpanel.h>
#include <fcitx/instance.h>
#include <fcitx/candidatelist.h>
#include <fcitx/text.h>
#include <fcitx/userinterfacemanager.h>
#include <fcitx-utils/key.h>

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

class HengEngine;

// 点击候选 -> core 选词 -> 刷新 UI
class HengCandidateWord : public CandidateWord {
public:
    HengCandidateWord(Text text, heng_session_t session, int index, HengEngine *engine)
        : CandidateWord(std::move(text)), session_(session), index_(index),
          engine_(engine) {}
    void select(InputContext *ic) const override;

private:
    heng_session_t session_;
    int index_;
    HengEngine *engine_;
};

class HengEngine : public InputMethodEngineV2 {
public:
    HengEngine(Instance *instance);
    ~HengEngine() override;

    void activate(const InputMethodEntry &entry, InputContextEvent &event) override;
    void deactivate(const InputMethodEntry &entry, InputContextEvent &event) override;
    void keyEvent(const InputMethodEntry &entry, KeyEvent &keyEvent) override;
    void reset(const InputMethodEntry &entry, InputContextEvent &event) override;

    void updateUI(InputContext *ic);

private:
    heng_session_t sessionFor(InputContext *ic);
    void endSession(InputContext *ic);

    Instance *instance_;
    std::unordered_map<uint64_t, heng_session_t> sessions_;
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
}

HengEngine::~HengEngine() {
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
    }
    return session;
}

void HengEngine::endSession(InputContext *ic) {
    auto key = icKey(ic->uuid());
    auto it = sessions_.find(key);
    if (it != sessions_.end()) {
        heng_end_session(it->second);
        sessions_.erase(it);
    }
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
    ic->inputPanel().reset();
    ic->updateUserInterface(UserInterfaceComponent::InputPanel);
}

void HengEngine::reset(const InputMethodEntry &, InputContextEvent &event) {
    auto *ic = event.inputContext();
    if (auto it = sessions_.find(icKey(ic->uuid())); it != sessions_.end()) {
        heng_clear(it->second);
    }
    updateUI(ic);
}

void HengEngine::keyEvent(const InputMethodEntry &, KeyEvent &keyEvent) {
    auto *ic = keyEvent.inputContext();
    auto key = keyEvent.key();

    // 修饰键单独按/释放不进引擎
    if (key.isModifier() || keyEvent.isRelease()) {
        return;
    }

    heng_session_t session = sessionFor(ic);
    if (session == 0) {
        const char *err = heng_last_error();
        std::fprintf(stderr, "[heng] 会话创建失败: %s\n", err ? err : "(null)");
        return; // 未处理 -> fcitx5 透传给应用
    }

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
    updateUI(ic);
    heng_free_context(&ctx);
    (void)handled; // v5 返回值与透传决策：试用版接受全部按键，由 commit/preedit 呈现
}

void HengEngine::updateUI(InputContext *ic) {
    auto &panel = ic->inputPanel();
    panel.reset();

    heng_session_t session = sessionFor(ic);
    if (session == 0) {
        return;
    }
    HengContext ctx = {0};
    ctx.data_size = sizeof(HengContext);
    if (heng_get_context(session, &ctx) != HENG_TRUE) {
        heng_free_context(&ctx);
        ic->updateUserInterface(UserInterfaceComponent::InputPanel);
        return;
    }

    if (ctx.preedit) {
        panel.setClientPreedit(preeditText(ctx));
        panel.setPreedit(preeditText(ctx));
    }

    if (ctx.candidate_count > 0 && ctx.candidates) {
        auto *list = new CommonCandidateList;
        if (ctx.select_keys) {
            list->setSelectionKey(Key::keyListFromString(ctx.select_keys));
        }
        list->setPageSize(ctx.candidate_count);
        for (int i = 0; i < ctx.candidate_count; i++) {
            Text text(ctx.candidates[i] ? ctx.candidates[i] : "");
            list->append<HengCandidateWord>(std::move(text), session, i, this);
        }
        list->setGlobalCursorIndex(ctx.highlighted);
        panel.setCandidateList(std::unique_ptr<CandidateList>(list));
    }
    heng_free_context(&ctx);
    ic->updateUserInterface(UserInterfaceComponent::InputPanel);
}

void HengCandidateWord::select(InputContext *ic) const {
    heng_select_candidate_on_current_page(session_, index_);
    engine_->updateUI(ic);
}

} // namespace fcitx

class HengEngineFactory : public fcitx::AddonFactory {
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override {
        return new fcitx::HengEngine(manager->instance());
    }
};

FCITX_ADDON_FACTORY(HengEngineFactory)
