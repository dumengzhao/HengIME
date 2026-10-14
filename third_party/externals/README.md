# externals —— 外部二进制依赖

`output/` 与 `output-dev/` 里除编译产物外的全部运行时依赖。集中存放是为了让
`output/` 成为可随时清空的纯产物目录，也让「换机器要准备什么」有明确答案。

## 内容

| 路径 | 来源 | 许可 |
|---|---|---|
| `WinSparkle.dll` | <https://github.com/vslavik/winsparkle/releases> 的 `x64/Release/` | MIT |
| `rime.dll` | librime 发行包（本仓库 `third_party/librime/dist/lib/rime.dll` 的副本） | GPLv3 |
| `data/` | 雾凇拼音 <https://github.com/iDvel/rime-ice> | LGPL |

## 为什么 rime.dll 要放副本

librime 发行包会随版本变动，而 weasel 编译期链接的是 `lib64/rime.lib`，
运行时加载的必须是同一次编译产出的 dll。两者版本不一致会在 `RimeSetup`
阶段以「导出符号找不到」的形式失败。放在这里可以让「编译用的 .lib」与
「运行用的 .dll」版本对应关系可见。

## 为什么不入库

三个目录/文件的总代价：

- `WinSparkle.dll` 2.8 MB — 单文件可入库，但它是第三方项目的产物，
  入库后每次上游发版都要手动同步，且与本仓库代码无关
- `rime.dll` 3.6 MB — 同上，且 librime 已有独立 dist 副本，重复
- `data/` 50 MB — 其中 `cn_dicts/` 45 MB 是雾凇的词库二进制，
  由上游维护，入库会与上游长期分叉

`third_party/` 整体 gitignore（见根 `.gitignore`），本目录随之不入库。

## 装配

`tools/sync-runtime-deps.ps1` 把本目录的内容复制到 `output/` 与 `output-dev/`，
两个 build 脚本在 msbuild 之后自动调用。详见 `tools/README` 的说明或脚本头部注释。

## data/ 里属于本项目的文件

雾凇原版之外有四个文件是本项目自己的，全部在主仓库留底：

| 文件 | 权威位置 | 说明 |
|---|---|---|
| `default.yaml` | `patches/rime/rime-default.yaml` | 左右键切换候选 |
| `weasel.yaml` | `patches/rime/rime-weasel.yaml` | 配色方案 heng_blue |
| `rime_ice.schema.yaml` | `patches/rime/rime-ice.schema.yaml` | 模糊音全开 |
| `heng.yaml` | `config-center/shared/heng.yaml` | 衡自己的核心配置 |

四份留底与 `data/` 下的实际文件内容 md5 一致。`data/` 本身仍需从雾凇
下载（45 MB 词库无法入库），但**改过的部分不会丢**。