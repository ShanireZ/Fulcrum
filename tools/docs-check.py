#!/usr/bin/env python3
"""docs/ 这个 OKF v0.2 bundle 的结构门。

要拦的是两类漂移：

  1. 新增一份文档却忘了写 frontmatter —— 它对 agent 就是一份无类型的散文，
     渐进披露失效；
  2. 新增文档却没进 `index.md` —— agent 从入口读目录时看不见它，等于没写。

★★★ 这道门是补的，枢衡此前**一份文档门都没有**。补它的直接原因是
第 2 类漂移真的发生了：owner 08-21 把 `docs/agents/` 六份共享约定铺进来，同批还有
五个子目录的 `index.md`，**从 `docs/index.md` 出发一份都到不了**——而 `docker-run.sh`
的全部场景照样绿，因为没有任何一场看这件事。

★★ 「被 index 列出」不等于「从入口走得到」：子目录有自己的 `index.md` 时，父 index
必须链到那个子 index，**否则整棵子树都是孤儿**。判据因此取从入口出发的**传递闭包**。

★ 为什么在宿主机跑而不进容器：这是纯文本检查，不需要 Rust 工具链，也不需要 Linux
特有的任何东西（那才是 Docker 存在的理由）。而且构建镜像里**没有 python3**——
真要进容器就得先改镜像，为一道几十毫秒的门不值得。

用法：
    python tools/docs-check.py           # 检查，红了退出码 1
    python tools/docs-check.py --list    # 顺带列出全部文档与它们的可达状态
"""

from __future__ import annotations

import os
import re
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOCS = os.path.join(REPO, "docs")

# OKF 保留文件名，不是概念文档，不要求 type。
# ★ 按 basename 判定：`governance/index.md` 这类子目录入口同样是保留文件，
#   但它仍然必须可达 —— 那正是 08-21 断掉的那五份。
RESERVED = {"index.md", "log.md"}

OKF_VERSION = '"0.2"'

MARKDOWN_LINK = re.compile(r"\]\(([^)\s]+)\)")
FRONTMATTER = re.compile(r"\A---\r?\n(.*?)\r?\n---\r?\n", re.DOTALL)
TYPE_LINE = re.compile(r"^type:[ \t]*(\S.*)$", re.MULTILINE)
OKF_LINE = re.compile(r"^okf_version:[ \t]*(\S.*)$", re.MULTILINE)


def normalize(path: str) -> str:
    """统一成 / 分隔并解掉 . 与 ..。

    ★★ 消不掉的 `..` 必须留着 —— 它是「这条链接指到仓库外面去了」的唯一证据。
    把它折叠掉，门会以为那是本仓根下的文件，从而报一条假断链。
    """
    parts: list[str] = []
    for part in path.replace("\\", "/").split("/"):
        if part in ("", "."):
            continue
        if part != "..":
            parts.append(part)
        elif parts and parts[-1] != "..":
            parts.pop()
        else:
            parts.append("..")
    return "/".join(parts)


def walk() -> list[str]:
    """递归收集 bundle 里的全部 Markdown，键是相对 docs/ 的 / 分隔路径。"""
    found = []
    for dirpath, _dirnames, filenames in os.walk(DOCS):
        for name in filenames:
            if name.endswith(".md"):
                rel = os.path.relpath(os.path.join(dirpath, name), DOCS)
                found.append(rel.replace(os.sep, "/"))
    return sorted(found)


def source_of(rel: str) -> str:
    with open(os.path.join(DOCS, *rel.split("/")), encoding="utf-8") as handle:
        return handle.read()


def links_of(rel: str) -> list[tuple[str, bool]]:
    """一份文档里指向 Markdown 的链接，按它自己所在的目录解析。"""
    directory = rel.rsplit("/", 1)[0] if "/" in rel else ""
    out = []
    for raw in MARKDOWN_LINK.findall(source_of(rel)):
        href = raw.split("#")[0]
        if not href or not href.endswith(".md") or "://" in href:
            continue
        # bundle 内链接写成相对 bundle 根的绝对路径（/governance/index.md）；
        # ../PLAN.md 这类出 bundle 的按文档自身位置解析到仓库根。
        if href.startswith("/"):
            from_repo = normalize("docs/" + href)
        else:
            from_repo = normalize("docs/" + directory + "/" + href)
        inside = from_repo.startswith("docs/")
        out.append((from_repo[len("docs/"):] if inside else from_repo, inside))
    return out


def reachable_from_index(files: list[str]) -> set[str]:
    """从入口出发的传递闭包 —— 「被列出」不够，要走得到。"""
    known = set(files)
    seen = {"index.md"}
    queue = ["index.md"]
    while queue:
        current = queue.pop()
        if current not in known:
            continue
        for target, inside in links_of(current):
            if not inside or target in seen:
                continue
            seen.add(target)
            queue.append(target)
    return seen


def main() -> int:
    files = walk()
    problems: list[str] = []

    # ★ 扫描没瞎：递归确实进了子目录。退化回「只有顶层」时，可达性会变成空断言。
    if not files:
        print("docs/ 里一份 Markdown 都没有 —— 扫描本身坏了", file=sys.stderr)
        return 1
    if not any("/" in name for name in files):
        print("递归没进任何子目录 —— 扫描本身坏了", file=sys.stderr)
        return 1

    index_front = FRONTMATTER.search(source_of("index.md"))
    declared = OKF_LINE.search(index_front.group(1)) if index_front else None
    if declared is None or declared.group(1).strip() != OKF_VERSION:
        problems.append(
            "index.md 没有声明 okf_version: %s（实测 %s）"
            % (OKF_VERSION, declared.group(1).strip() if declared else "缺失")
        )

    reachable = reachable_from_index(files)
    for name in files:
        if name not in reachable:
            problems.append("从 index.md 走不到：%s —— 到不了就等于没写" % name)

    for name in files:
        if name.rsplit("/", 1)[-1] in RESERVED:
            continue
        block = FRONTMATTER.search(source_of(name))
        if block is None:
            problems.append("没有 frontmatter：%s" % name)
        elif TYPE_LINE.search(block.group(1)) is None:
            problems.append("frontmatter 缺非空 type：%s" % name)

    for name in files:
        for target, inside in links_of(name):
            if inside:
                if target not in set(files):
                    problems.append("断链：%s -> /%s" % (name, target))
            elif not target.startswith(".."):
                # ★★★ 跨仓引用（消不掉的 ..）不归本门管：它指向 workspace 里的兄弟
                #   仓库，本仓既无权也无力保证它在，而克隆本仓的人那里它根本不存在。
                if not os.path.exists(os.path.join(REPO, *target.split("/"))):
                    problems.append("断链：%s -> %s" % (name, target))

    if "--list" in sys.argv:
        for name in files:
            print("%s %s" % ("  OK " if name in reachable else "  ★孤儿", name))

    if problems:
        print("docs/ bundle 门未通过，%d 项：" % len(problems), file=sys.stderr)
        for line in problems:
            print("  - " + line, file=sys.stderr)
        return 1

    print("docs/ bundle 门通过：%d 份文档，全部从 index.md 可达。" % len(files))
    return 0


if __name__ == "__main__":
    sys.exit(main())
