# third_party —— 本地依赖（不进 Git）

本目录内容均可从公开来源重建。我们对 weasel 源码的全部修改已固化为
主仓库 `patches/` 下的补丁与配置文件——**新机器拉取主仓库后按本文步骤
即可完整重建构建环境**。

## 目录

| 路径 | 内容 | 是否入库 |
|---|---|---|
| `externals/` | 运行时外部依赖（WinSparkle.dll / rime.dll / 雾凇 data） | 否 |
| `librime/dist/` | librime 发行包（编译期链接 `rime.lib`） | 否 |
| `src/weasel/` | weasel 源码 + `deps/boost_1_84_0/` | 否 |
| `src/squirrel/` | squirrel 源码（macOS 外壳） | 否 |

## externals —— 运行时依赖的归位处

`output/` 与 `output-dev/` 需要三样 msbuild 不产出的东西：

```
WeaselServer.exe --导入--> WinSparkle.dll
                 --导入--> heng_core.dll --导入--> rime.dll
外加共享数据目录 data\（雾凇词库，约 50MB）
```

缺任何一项，服务进程都会在加载阶段退出，且早于第一条日志。它们集中放在
`third_party/externals/`，使 `output/` 成为可随时清空的纯产物目录。

| 路径 | 来源 |
|---|---|
| `externals/WinSparkle.dll` | [vslavik/winsparkle releases](https://github.com/vslavik/winsparkle/releases) 的 `x64/Release/` |
| `externals/rime.dll` | `librime/dist/lib/rime.dll` 的副本 |
| `externals/data/` | [iDvel/rime-ice](https://github.com/iDvel/rime-ice) 的 `main.zip` |

`build-x64.bat` / `build-x64-dev.bat` 在 msbuild 之后调用 `sync-deps.bat`
自动铺到 `output/` / `output-dev/`，缺什么补什么，已存在则跳过。

**WinSparkle.dll 必须 x64**：官方 weasel installer 里的是 32 位
（其 WeaselServer.exe 也是 32 位），装上会在加载时因架构不符失败。

**不要用 `/MIR` 同步 data**：引擎运行时会把部署产物写进 `data/build/`，
镜像同步会在每次构建时抹掉它并触发重新部署。`sync-deps.bat` 只在
`data/cn_dicts` 缺失时才整目录拷贝。

### data/ 里属于本项目的文件

雾凇原版之外有四个文件是本项目自己的，全部在主仓库留底，
与 `data/` 下实际文件内容 md5 一致：

| 文件 | 权威位置 | 说明 |
|---|---|---|
| `default.yaml` | `patches/rime/rime-default.yaml` | 左右键切换候选 |
| `weasel.yaml` | `patches/rime/rime-weasel.yaml` | 配色方案 heng_blue |
| `rime_ice.schema.yaml` | `patches/rime/rime-ice.schema.yaml` | 模糊音全开 |
| `heng.yaml` | `config-center/shared/heng.yaml` | 衡核心配置 |

## 配置来源的分工

`runtime/rime-shared/`（gitignored）里的文件分两类，获取方式不同：

| 类别 | 内容 | 来源 | 入库 |
|---|---|---|---|
| 雾凇原版 | 词典 `cn_dicts/`、`lua/`、符号表、opencc、7 个双拼 schema、`melt_eng` / `radical_pinyin` 词典与 schema | `iDvel/rime-ice` release 的 `full.zip` | 否 |
| 本项目改动 | 上一节表里那四个文件 | 上节「权威位置」列 | 是 |

**雾凇的部分不入库**。实测 `full.zip` 里 63 个文件与本机逐文件 md5 相同，
只有 6 个不同：`default.yaml` / `weasel.yaml` / `rime_ice.schema.yaml`
（本项目改过）与三个词典（版本差异）。入库会让配置与上游脱节，
而那批文件里没有本项目自己的改动。

**词典跟随上游更新，不锁版本。** 上游一个月至少两次增改词汇与权重
（`base.dict.yaml` 头部有 `version` 字段，如 `2026-09-11`）。
锁版本会让 CI 与本机的分词行为分叉——实测只换词典版本、配置不动，
abitest 仍全部通过，说明词库更新本身不影响行为一致性。

**`lua/` 目录不可省略。** `rime_ice.schema.yaml` 挂了一批 lua 组件
（`lua_translator@*unicode`、`lua_filter@*corrector` 等），
缺目录时 librime 静默降级：不报致命错，但分词不出词条，
表现为 `preedit` 停在 `nihao`、候选数 0。

## librime

core 通过 `build.rs` 链接 `third_party/librime/dist/lib/rime.lib`。

**获取方式**（当前版本见 `librime/version-info.txt`）：

1. 打开 <https://github.com/rime/librime/releases>，下载对应平台的产物：
   - Windows（MSVC 工具链）：`rime-<hash>-Windows-msvc-x64.7z`
   - macOS：`rime-<hash>-macOS-universal.tar.bz2`
   - Linux：用发行版包 `librime-dev`（apt）或 `brew install librime`
2. 解压到 `librime/`，得到 `dist/`（含 `include/`、`lib/`、`bin/`）。
3. `copy librime/dist/lib/rime.dll externals/`

**注意**：

- 用官方现行版本（x64）。旧版曾只发 32 位 dll，无法用于本项目的 MSVC x64 工具链。
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

## squirrel 源码（macOS 外壳）

```bash
git clone --depth 1 https://github.com/rime/squirrel third_party/src/squirrel
cd third_party/src/squirrel
git fetch --depth 1 origin 6ad0e45bcf53b770474100c02cec43912f0da502
git checkout 6ad0e45bcf53b770474100c02cec43912f0da502
git apply ../../../patches/macos/squirrel-heng.patch
```

基线 commit `6ad0e45b` 含品牌图标（作者为本项目），补丁只含贴帧层相关的 6 个文件。

`librime/` 与 `lib/` 下的 dylib、`librime/include/heng.h` 是构建产物不入库，
由 `tools/squirrel-dev-install.sh` 在部署时从 `target/release/` 与
`core/include/` 同步。

编译与部署：`make debug`（产物在 `build/Build/Products/Debug/Squirrel.app`），
再跑 `tools/squirrel-dev-install.sh` 装成 dev 版。

## output/ 与 output-dev/ 的内容构成

| 文件 | 产出方 |
|---|---|
| WeaselServer.exe / WeaselDeployer.exe / weaselx64.dll 及其 .exp/.lib/.pdb | `build-x64*.bat` |
| 四个静态库的 .lib/.pdb（WeaselUI / WeaselIPC / WeaselIPCServer / RimeWithWeasel） | `build-x64*.bat` |
| heng_core.dll | `build-core.bat`（cargo build + 部署） |
| WinSparkle.dll / rime.dll / data/ | `sync-deps.bat`（从 `externals/` 铺） |

前两类可以随时删掉重编；后两类由脚本自动恢复。
`output-dev/` 多出的 `runtime-user/` 是 dev 版用户配置（`user.yaml`、
`*.custom.yaml`、`installation.yaml`），**不属于构建产物，不要清**。

## 四个构建脚本的分工

两个 `build-x64*.bat` 是唯一入口，依次调四个脚本：

| 顺序 | 脚本 | 做什么 |
|---|---|---|
| msbuild 前 | `build-core.bat <dir>` | `cargo build --release`；停掉该输出目录的 WeaselServer（它锁着 `heng_core.dll`）；部署新 dll |
| msbuild 前 | `unlock-tsf.bat <dir>` | 把 `weaselx64.dll` 改名让出链接输出路径 |
| — | `msbuild weasel.sln` | 编译全部项目 |
| msbuild 后 | `sync-deps.bat <dir>` | 从 `externals/` 补齐 `WinSparkle.dll` / `rime.dll` / `data/` |

**`weaselx64.dll` 为什么必须让位**：它是 TSF 进程内服务器，被每个激活过输入法的
宿主加载，同时作为 COM InprocServer32 注册。link.exe 写输出前先 unlink，
`DeleteFile` 对已映射为映像的文件失败，报 `LNK1104`。改名可行——宿主继续用
重命名后文件的映射，链接器发现路径已空。不需要管理员权限，`regsvr32 /u` 也不需要：
COM 注册存的是路径而非文件身份，链接产物写回同名即生效。

让位文件带随机后缀（`weaselx64.dll.parked<rand>`），每次运行顺带清扫遗留：
映射已释放的删得掉，仍被占用的删不掉并留到下次。手工删这些文件时若报
「其他程序正在使用」，说明还有宿主加载着它，关掉那些程序再删。

**改了 ABI 之后还要重建 `lib64/heng.lib`**，见文末。

## src/weasel 改动固化（主仓库 patches/windows/）

我们对 weasel 源码的全部修改保存在主仓库 `patches/windows/`：

| 文件 | 内容 |
|---|---|
| `windows/weasel-0.17.4-heng-m1.patch` | 全部源码与构建脚本改动：RimeWithWeasel 数据源换血 + 样式/app_options 配置通道 + 内置候选窗接线 + TSF 侧 CCandidateList 永不显示（避免与 core 候选窗叠双层）+ 布局间距修复 + ContextUpdater 零宽保护 + 菜单精简与托盘图标 + `build-x64.bat` / `build-x64-dev.bat` / `build-core.bat` / `sync-deps.bat` + `include/heng.h` |
| `windows/weasel.props` / `windows/env.bat` | 编译配置（BOOST_ROOT 等，上游 gitignore 忽略） |
| `windows/boost-project-config.jam` | b2 MSVC 显式配置（修复 setup 路径 bug） |
| `windows/heng.def` | core 全部导出清单，供重建 `lib64/heng.lib` 用 |

配置留底（雾凇基础上叠加本项目改动）在 `config-center/shared/` 与
`patches/rime/*.yaml`，见「externals」一节的对照表。

**新机器重建步骤**（克隆 weasel 后）：

```bash
cd third_party/src/weasel
git apply --whitespace=nowarn ../../../patches/windows/weasel-0.17.4-heng-m1.patch
cp ../../../patches/windows/weasel.props ../../../patches/windows/env.bat .
cp ../../../patches/windows/boost-project-config.jam deps/boost_1_84_0/project-config.jam

# 共享数据从 externals/ 铺到 output\，再叠加本项目改过的配置
cmd //c sync-deps.bat output
cp ../../../patches/rime/rime-default.yaml   output/data/default.yaml
cp ../../../patches/rime/rime-weasel.yaml    output/data/weasel.yaml
cp ../../../patches/rime/rime-ice.schema.yaml output/data/rime_ice.schema.yaml
cp ../../../config-center/shared/heng.yaml output/data/heng.yaml
```

之后 `build-x64.bat` 即可产出可运行的正式版；`build-x64-dev.bat` 出 dev 版，
再跑 `tools/weasel-dev-install.ps1` 注册。

改动明细（供 review 补丁时对照）：

- `RimeWithWeasel/RimeWithWeasel.cpp` + `include/RimeWithWeasel.h`：
  数据源整体换血（rime_api → heng-core C ABI）；样式从 weasel.yaml 加载
  （style 段 + preset_color_schemes 配色方案，硬编码 fallback 兜底）、
  app_options 段按应用生效、亮/暗配色切换恢复；weasel.yaml `ui/builtin`
  开启 core 内置自绘候选窗（weasel 自带面板隐藏，点击选词 commit 经
  _Respond 的 heng_take_ui_commit 取回上屏）
- `WeaselUI/HorizontalLayout.cpp` / `VerticalLayout.cpp`：
  候选间距补偿（高亮块膨胀不再压相邻序号）
- `WeaselIPC/ContextUpdater.cpp`：零宽选中区间不生成 HIGHLIGHTED
- `WeaselServer/WeaselServerApp.cpp`：托盘与 LanguageBar 菜单精简、
  服务启停改为实例方法
- `WeaselTSF/`：rc 文件 winres.h 替换 + 菜单文案
- `include/heng.h`：core C ABI 头，与 `core/include/heng.h` 同步
- `output/data/default.yaml` key_binder：左右键切换候选

**后续维护约定**：每次修改 weasel 树内文件后重新生成补丁并提交主仓库：

```bash
python .workbuddy/tools/gen_weasel_patch.py patches/windows/weasel-0.17.4-heng-m1.patch
```

生成脚本的约束（`git diff HEAD` / 新文件先 `git add -N` / `--text` 而非
`--binary` / 排除 output 产物）与验证流程见 `patches/README.md`。

## lib64/heng.lib（core 导入库，ABI 变更后必须重导）

`lib64/heng.lib` 是 `heng_core.dll` 的导入库，供 WeaselServer.exe 链接。**core 每新增
C API（`heng_*` 导出）后它不会自动更新**——链接期报 `LNK2001: 无法解析的外部符号
__imp_heng_*` 即此原因。重建步骤：

```bash
# 1. 重编 core 并部署（build-x64*.bat 里的 build-core.bat 会做同样的事）
cargo build --release -p heng-core
cp target/release/heng_core.dll third_party/src/weasel/output/

# 2. 从新 dll 导出表生成 heng.def（pefile 解析）
python -c "
import pefile
pe = pefile.PE(r'third_party/src/weasel/output/heng_core.dll')
names = [e.name.decode() for e in pe.DIRECTORY_ENTRY_EXPORT.symbols if e.name]
with open(r'third_party/src/weasel/lib64/heng.def', 'w') as f:
    f.write('LIBRARY heng_core\nEXPORTS\n')
    for n in names: f.write('    ' + n + '\n')
"

# 3. VS 环境下重建导入库
cd third_party/src/weasel/lib64
lib /def:heng.def /machine:X64 /out:heng.lib
```

`patches/windows/heng.def` 已入库留底（全部导出清单，93 项 = 60 个 `heng_*` +
31 个 Slint `*VTable` + 2 个 `slint_new_path_*`）；`lib64/heng.lib`/`heng.exp`
为本地产物不入库。导入库必须与当前 dll 同架构同版本，否则链接期报
`LNK2001 无法解析的外部符号 __imp_heng_*`。

`lib64/` 下的 `rime.lib`（librime 发行包自带）与 `WinSparkle.lib`（winsparkle
发行包自带）是**链接输入**，不是产物，同样不要清理。
