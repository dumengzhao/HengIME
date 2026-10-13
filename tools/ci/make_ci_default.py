"""为 CI 裁剪 default.yaml 的 schema_list，只留 rime_ice。

**为什么需要裁剪**：入库的 `patches/rime-default.yaml` 的 `schema_list` 列了 9 个方案
（rime_ice + t9 + 7 个双拼），但只有 `rime_ice` 的 schema 文件在仓库里——
其余 8 个的本机副本在 `runtime/rime-shared/` 且该目录被 gitignore，从未入库。
librime 部署时遇到缺失的 schema 会以 `deploy: failure` 收场，
之后所有分词与 config 断言一起失败。

做法：把除 `rime_ice` 外的 `- schema: xxx` 行整行注释掉，保留可逆性。

用法：python make_ci_default.py <输入> <输出>
"""
import pathlib
import re
import sys

KEEP = "rime_ice"

# 只匹配 schema_list 段落内的条目：行首两空格 + "- schema: " + 标识 + 可选行尾注释
ENTRY = re.compile(r"^(  - schema: )(\S+)(.*)$", re.M)


def main():
    src = pathlib.Path(sys.argv[1])
    dst = pathlib.Path(sys.argv[2])
    text = src.read_text(encoding="utf-8")

    kept = []

    def repl(m):
        ident = m.group(2)
        if ident == KEEP:
            kept.append(ident)
            return m.group(0)
        return "#" + m.group(0)

    out = ENTRY.sub(repl, text)
    if not kept:
        raise SystemExit(
            "schema_list 一条都没保留，正则多半没匹配上。\n"
            "输入前 5 行：\n" + "\n".join(text.splitlines()[:5])
        )
    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_text(out, encoding="utf-8")
    print("schema_list 保留 %d 项: %s" % (len(kept), ", ".join(kept)))


if __name__ == "__main__":
    main()