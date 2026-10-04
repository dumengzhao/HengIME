# third_party —— 本地依赖（不进 Git）

本目录内容均可从公开来源重建。我们对 weasel 源码的全部修改已固化为
主仓库 `patches/` 下的补丁与配置文件——**新机器拉取主仓库后按本文步骤
即可完整重建构建环境**。

## librime

core 通过 `build.rs` 链接 `third_party/librime/dist/lib/rime.lib`，运行时加载同目录 `rime.dll`。

**获取方式**（当前版本 1.17.0）：

1. 打开 <https://github.com/rime/librime/releases>，下载对应平台的产物：
   - Windows（MSVC 工具链）：`rime-<hash>-Windows-msvc-x64.7z`
   - macOS：`rime-<hash>-macOS-universal.tar.bz2`
   - Linux：用发行版包 `librime-dev`（apt）或 `brew install librime`
2. 解压到本目录，得到 `dist/`（含 `include/`、`lib/`、`bin/`）。

Windows 下最终布局：

```
third_party/librime/
└── dist/
    ├── include/rime_api.h ...
    ├── lib/rime.dll · rime.lib
    └── bin/rime_deployer.exe ...
```

**注意**：

- 用官方现行版本（x64）。旧版曾只发 32 位 dll，无法用于本项目的 MSVC x64 工具链。
- `rime.dll` 需与可执行文件同目录（或在 PATH 中）才能加载。开发时复制到 `target/debug/`。
- 官方产物已静态打包 boost / leveldb / opencc 等依赖，无需额外 dll。

## boost 1.84（weasel 编译链依赖）

1. 下载 <https://www.boost.org/users/history/version_1_84_0.html> 的 `boost_1_84_0.7z`
2. 解压到 `third_party/src/weasel/deps/boost_1_84_0/`
3. b2 需要显式 MSVC 配置：`project-config.jam` 用 `using msvc : 14.3 : <cl路径>
   : <setup>vcvarsall路径 <archiver>lib路径`（b2 自动探测的 setup 路径是坏的；
   本仓库已修好的该文件随 weasel 改动一同保存）

## weasel 源码（M1 Windows 外壳基础）

```bash
git clone --depth 1 -b 0.17.4 https://github.com/rime/weasel third_party/src/weasel
```

克隆后按「src/weasel 树内改动清单」（见文末）重放我们的修改。

## 雾凇拼音数据（output/data，约 50MB）

下载 <https://github.com/iDvel/rime-ice/archive/refs/heads/main.zip>，
解压 `rime-ice-main/` 内容到 `third_party/src/weasel/output/data/`，
排除：`.github/`、`build/`、`README.md`、`LICENSE`、`AGENTS.md`、`.gitignore`。

## WinSparkle.dll（WeaselServer 运行时依赖，必须 x64）

官方 weasel installer 里的是 32 位（其 WeaselServer.exe 也是 32 位），**不能用**。
从 <https://github.com/vslavik/winsparkle/releases> 下载 `WinSparkle-<ver>.zip`，
提取 `x64/Release/WinSparkle.dll` 放入 `third_party/src/weasel/output/`。

## output/ 其余运行时产物

| 文件 | 来源 |
|---|---|
| WeaselServer.exe / WeaselDeployer.exe / weaselx64.dll | `build-x64.bat` 编译产物 |
| heng_core.dll | `cargo build --release`（仓库内源码） |
| rime.dll | `third_party/librime/dist/lib/rime.dll` 复制 |
| data/ | 雾凇（见上节） |

## src/weasel 改动固化（主仓库 patches/）

我们对 weasel 源码的全部修改保存在主仓库 `patches/`：

| 文件 | 内容 |
|---|---|
| `weasel-0.17.4-heng-m1.patch` | 全部源码改动（13 文件）：RimeWithWeasel 数据源换血 + v4 样式/app_options 配置通道 + v6 内置候选窗接线（ui/builtin 开关、_UpdateUI 分支、_Respond 合并 heng_take_ui_commit）+ TSF 侧 CCandidateList 永不显示（避免与 core 候选窗叠双层）、布局间距修复、独立圆角药丸、ContextUpdater 零宽保护、rc winres 替换、build-x64.bat、include/heng.h（v6） |
| `weasel.props` / `env.bat` | 编译配置（BOOST_ROOT 等，上游 gitignore 忽略） |
| `rime-default.yaml` | 雾凇 default.yaml + 左右键切换候选（上游 gitignore 忽略） |
| `rime-weasel.yaml` | 雾凇 weasel.yaml + 衡默认主题 heng_blue（白底蓝块白字配色方案）（上游 gitignore 忽略） |
| `rime-ice.schema.yaml` | 雾凇 rime_ice.schema.yaml + 模糊音全开（平翘舌/前后鼻音/n-l；2026-10-04）（上游 gitignore 忽略） |
| `boost-project-config.jam` | b2 MSVC 显式配置（修复 setup 路径 bug） |

**新机器重建步骤**（克隆 weasel 后）：

```bash
cd third_party/src/weasel
git apply --whitespace=nowarn ../../../patches/weasel-0.17.4-heng-m1.patch
cp ../../../patches/weasel.props ../../../patches/env.bat .
cp ../../../patches/rime-default.yaml output/data/default.yaml
cp ../../../patches/rime-weasel.yaml output/data/weasel.yaml
cp ../../../patches/rime-ice.schema.yaml output/data/rime_ice.schema.yaml
cp ../../../patches/boost-project-config.jam deps/boost_1_84_0/project-config.jam
```

改动明细（供 review 补丁时对照）：

- `RimeWithWeasel/RimeWithWeasel.cpp` + `include/RimeWithWeasel.h`：
  数据源整体换血（rime_api → heng-core C ABI）；v4 起样式从 weasel.yaml 加载
  （style 段 + preset_color_schemes 配色方案，硬编码 fallback 兜底）、
  app_options 段按应用生效、亮/暗配色切换恢复；v6 起 weasel.yaml `ui/builtin`
  开启 core 内置自绘候选窗（weasel 自带面板隐藏，点击选词 commit 经
  _Respond 的 heng_take_ui_commit 取回上屏）
- `WeaselUI/HorizontalLayout.cpp` / `VerticalLayout.cpp`：
  候选间距补偿（高亮块膨胀不再压相邻序号）
- `WeaselIPC/ContextUpdater.cpp`：零宽选中区间不生成 HIGHLIGHTED
- `WeaselTSF/`：rc 文件 winres.h 替换（诊断插桩已于 v4 全部移除）
- `include/heng.h`：core C ABI 头（v6，含 config API 与内置候选窗 API），与 `core/include/heng.h` 同步
- `output/data/default.yaml` key_binder：左右键切换候选

**后续维护约定**：每次修改 weasel 树内文件后，重新生成补丁并提交主仓库：

```bash
cd third_party/src/weasel
git add -u && git add <新文件>
git diff --cached --binary > ../../../patches/weasel-0.17.4-heng-m1.patch
git reset -q
```
