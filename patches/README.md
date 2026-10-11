# weasel 补丁

`weasel-0.17.4-heng-m1.patch` 是 Windows 外壳的改动固化。

## 为什么需要补丁

`third_party/src/weasel/` 是 gitignored 的上游源码树（ime/weasel 0.17.4 + 一个
托盘刷新修复 commit `d73f629`）。改动只存在于本地工作树，不入库就等于不存在 ——
换机器重拉会全部丢失。

## 基线

| 项 | 值 |
|---|---|
| 上游 | https://github.com/rime/weasel |
| 基线 commit | `d73f6295e8252ed2f7b9c12bae32e9001b1afdaa` |
| 该commit 说明 | fix(WeaselServer): avoid tray refresh blocking IPC pipe（在 0.17.4 之后） |

**不是 0.17.4 tag**。tag `0.17.4` 指向 `9cc96e2`，用它做基线会导致补丁 context
对不上、`git apply` 失败。

## 重新生成

```bash
python .workbuddy/tools/gen_weasel_patch.py patches/weasel-0.17.4-heng-m1.patch
```

当前补丁 47 个文件 / 446 KB。

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