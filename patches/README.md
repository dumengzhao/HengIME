# 外壳补丁

本目录按平台与用途分三组，固化两端第三方外壳的改动与共享配置：

| 子目录 | 内容 |
|---|---|
| `windows/` | `weasel-0.17.4-heng-m1.patch`（weasel 源码与构建脚本）+ `heng.def`（core 导出清单）+ `weasel.props` / `env.bat` / `boost-project-config.jam`（编译配置） |
| `macos/` | `squirrel-heng.patch` |
| `rime/` | `rime-default.yaml` / `rime-weasel.yaml` / `rime-ice.schema.yaml`（雾凇基础上叠加的本项目改动，跨平台共用） |

两端补丁形态不同：weasel 已在主仓库跑通重新生成与验证脚本，squirrel 目前是一次性
产出，重生成需在 mac 上手工执行（见文末）。

## 为什么需要补丁

`third_party/src/weasel/` 与 `third_party/src/squirrel/` 都是 gitignored 的上游源码树。
改动只存在于本地工作树，不入库就等于不存在——换机器重拉会全部丢失。

Linux 外壳不走这条路径：`shells/linux/fcitx5/` 是自研代码，直接入库。

## weasel 基线

| 项 | 值 |
|---|---|
| 上游 | https://github.com/rime/weasel |
| 基线 commit | `d73f6295e8252ed2f7b9c12bae32e9001b1afdaa` |
| 该commit 说明 | fix(WeaselServer): avoid tray refresh blocking IPC pipe（在 0.17.4 之后） |

**不是 0.17.4 tag**。tag `0.17.4` 指向 `9cc96e2`，用它做基线会导致补丁 context
对不上、`git apply` 失败。

## squirrel 基线

| 项 | 值 |
|---|---|
| 上游 | https://github.com/rime/squirrel |
| 基线 commit | `6ad0e45bcf53b770474100c02cec43912f0da502`（2026-10-08） |
| 该 commit 说明 | style: 品牌图标定型——印章风 |

该commit 由本项目作者推送到上游，内容只有 `Rime.icon/Assets/logo.svg` 一个文件。
因此基线里已含品牌图标，**补丁不必重复固化它**。

## 重新生成（weasel）

```bash
python .workbuddy/tools/gen_weasel_patch.py patches/windows/weasel-0.17.4-heng-m1.patch
```

当前补丁 47 个文件 / 452 KB。

脚本在 `.workbuddy/`（gitignored），逻辑要点：

- **用 `git diff HEAD`**，不是 `git diff`。工作树里有一部分文件是已 staged 的
  （状态 `M ` 而非 ` M`），`git diff` 只比工作区与暂存区，会漏掉这批。
- **新增文件要先 `git add -N`**。`git diff` 系列不输出未跟踪文件，新建文件
  （如 `build-core.bat`）不进补丁。`git add -N`（intent-to-add）让它进入
  diff 而不落入 index 暂存内容。
- **保留名单里的文件必须真实存在于工作树**。`output/data/weasel.yaml` 在
  `KEEP_OUT` 里，但清空 `output/` 后它不在了；`git diff` 遇到不存在的路径
  会整体失败并返回 128、stdout 为空——脚本却只看到「47 个文件」就继续，
  写出 176 字节的空补丁。脚本现在跳过缺失文件并在输出里点数，
  且 `git diff` 结果小于 1000 字节时直接中止。
- **用 `--text`**。两份 `.rc` 是 UTF-16LE，git 默认当二进制，不加这个参数
  只会输出 `Binary files differ`，补丁无法重放。
- **不能加 `--binary`**。与 `--text` 同时使用时 git 仍会把 UTF-16 的 NUL
  写成字面量，补丁里出现 `^@`，重放时匹配失败。
- **排除 `output/` 与 `output-dev/`、`msbuild-dev/`**。那里是构建产物
  （安装脚本、curl、预览图）。例外：`output/data/weasel.yaml` 是真实配置，保留。
- **`.bat` 必须是 CRLF**，且注释全用英文。中文注释在 cmd 控制台会乱码，
  LF 行尾则与上游同名文件不一致。

## 改 UTF-16LE 的 .rc

`.rc` 是 UTF-16LE 且**混合行尾**（`WeaselServer.rc` 里 CRLF 与裸 LF 并存），
按行号批量替换极容易吃掉替换区以外的内容，症状是 `RC1020 unexpected '#endif'`。

用 `.workbuddy/tools/edit_rc.py`（按 `\n` 切分并剥掉行尾 `\r`，写回统一 LF），
改完用 `.workbuddy/tools/check_rc_struct.py` 验结构（`#if`/`BEGIN` 配平、
BOM、行尾），不必等 msbuild 报错。

## 编码漂移

用文本编辑器改 UTF-16LE 文件时，BOM 容易被丢掉。`WeaselServer.rc` 的
BOM 丢过一次，后果是 rc.exe 按 ANSI 代码页读整个 UTF-16 文件，中文语言块
损坏，系统静默回退到英文块——**菜单文字全变英文，编译零报错**，没有任何
诊断信息。

改动 `.rc` 后跑一次字节级比对：

```bash
python .workbuddy/tools/check_encoding_drift.py
python .workbuddy/tools/check_rc_struct.py <改过的.rc>
```

前者覆盖 BOM 差异与新增文件行尾，返回码非 0 表示有问题；
后者查条件编译与资源块是否配平，能在 msbuild 之前发现结构损坏。

## 验证

补丁必须能在干净基线上重放且结果一致：

```bash
git clone --no-checkout https://github.com/rime/weasel.git /tmp/wv
cd /tmp/wv && git checkout d73f629
git apply --check <补丁>      # 应为 0 失败
git apply <补丁>
python .workbuddy/tools/verify_patch_result.py /tmp/wv <工作树> <文件清单>
```

比对用**文件内容 md5**，不要用 `git status` —— 两棵树的 index 状态不同
（本地有一部分 staged），同一内容可能显示成 `M ` 或 ` M`，不可比。

`git apply` 对 UTF-16 的 `.rc` 会输出若干 whitespace 警告，属正常，
只要退出码为 0 且 md5 全部一致即可。

## 行尾

Windows 检出为 CRLF。新增文件若写成 LF，git 会当成新文件，补丁应用后
与上游同名文件不一致。发现内容差异但 `diff` 看不出逻辑差别时，先查行尾：

```bash
python -c "import pathlib;b=pathlib.Path('X').read_bytes();print('CRLF',b.count(b'\r\n'),'LF',b.count(b'\n')-b.count(b'\r\n'))"
```

## squirrel 补丁

`squirrel-heng.patch` 是 macOS 外壳的改动固化，6 个文件 / 56739 字节：

| 文件 | 内容 |
|---|---|
| `sources/HengWindowRenderer.swift` | 新增。NSPanel 贴帧层（29533 字节），core 只出帧、窗口操作在主线程执行 |
| `sources/SquirrelApplicationDelegate.swift` | 接线贴帧层 |
| `sources/SquirrelInputController.swift` | 按键/触摸事件转 core 调用 |
| `sources/BridgingFunctions.swift` | C 桥接结构与回调 |
| `Squirrel.xcodeproj/project.pbxproj` | 注册新文件、HEADER_SEARCH_PATHS、链接 `libheng_core.dylib` |
| `resources/Localizable.xcstrings` | 三语言文案 |

贴帧层调用的 core ABI：`heng_set_window_handler`、`heng_set_commit_handler`、
`heng_set_screen_info`、`heng_set_work_area`、`heng_push_pointer_event` ——
全部在 `core/include/heng.h` 中有声明。

### 不入补丁的文件

squirrel 的 `.gitignore` 已排除以下路径，它们是构建产物或运行时数据，
`tools/squirrel-dev-install.sh` 每次运行会重新同步：

| 路径 | 来源 |
|---|---|
| `lib/libheng_core.dylib` | `target/release/libheng_core.dylib` |
| `librime/include/heng.h` | `core/include/heng.h` |
| `Frameworks/`、`build/`、`data/opencc/`、`data/plum/`、`package/`、`download/` | 构建与打包中间物 |

### 重新生成（mac）

改动只在 mac 那台机器上，Windows 侧的 `third_party/src/squirrel` 是干净的上游树。
在 mac 上执行：

```bash
cd <衡仓库>/third_party/src/squirrel

# 新增文件先标intent-to-add，否则不进 diff
git add -N <新增文件>

# --text 必需（.strings/.plist/.xcassets 会被判二进制）；不能加 --binary
git diff HEAD --text > /tmp/squirrel-heng.patch

# 自检
grep -c '^diff --git' /tmp/squirrel-heng.patch   # 文件数
grep -n '\^@' /tmp/squirrel-heng.patch           # 必须无输出
```

验证方式与 weasel 相同：在 `6ad0e45b` 的干净基线树上 `git apply`，再比对文件内容
md5。mac 工作树不在 Windows 侧，无法用 `verify_patch_result.py` 直接比对，
需在 mac 上按文件逐个 `md5 -q` 对照。