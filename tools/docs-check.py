#!/usr/bin/env python3
"""docs/ 這個 OKF v0.2 bundle 的結構門。

要攔的是兩類漂移：

  1. 新增一份文件卻忘了寫 frontmatter —— 它對 agent 就是一份無類型的散文，
     漸進披露失效；
  2. 新增文件卻沒進 `index.md` —— agent 從入口讀目錄時看不見它，等於沒寫。

★★★ 這道門是補的，樞衡此前**一份文件門都沒有**。補它的直接原因是
第 2 類漂移真的發生了：owner 08-21 把 `docs/agents/` 六份共享約定鋪進來，同批還有
五個子目錄的 `index.md`，**從 `docs/index.md` 出發一份都到不了**——而 `docker-run.sh`
的全部場景照樣綠，因為沒有任何一場看這件事。

★★ 「被 index 列出」不等於「從入口走得到」：子目錄有自己的 `index.md` 時，父 index
必須鏈到那個子 index，**否則整棵子樹都是孤兒**。判據因此取從入口出發的**傳遞閉包**。

★ 為什麼在宿主機跑而不進容器：這是純文字檢查，不需要 Rust 工具鏈，也不需要 Linux
特有的任何東西（那才是 Docker 存在的理由）。而且建置鏡像裡**沒有 python3**——
真要進容器就得先改鏡像，為一道幾十毫秒的門不值得。

用法：
    python tools/docs-check.py           # 檢查，紅了結束代碼 1
    python tools/docs-check.py --list    # 順帶列出全部文件與它們的可達狀態
"""

from __future__ import annotations

import os
import re
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOCS = os.path.join(REPO, "docs")

# OKF 保留檔案名，不是概念文件，不要求 type。
# ★ 按 basename 判定：`governance/index.md` 這類子目錄入口同樣是保留檔案，
#   但它仍然必須可達 —— 那正是 08-21 斷掉的那五份。
RESERVED = {"index.md", "log.md"}

OKF_VERSION = '"0.2"'

MARKDOWN_LINK = re.compile(r"\]\(([^)\s]+)\)")
FRONTMATTER = re.compile(r"\A---\r?\n(.*?)\r?\n---\r?\n", re.DOTALL)
TYPE_LINE = re.compile(r"^type:[ \t]*(\S.*)$", re.MULTILINE)
OKF_LINE = re.compile(r"^okf_version:[ \t]*(\S.*)$", re.MULTILINE)


def normalize(path: str) -> str:
    """統一成 / 分隔並解掉 . 與 ..。

    ★★ 消不掉的 `..` 必須留著 —— 它是「這條連結指到儲存庫外面去了」的唯一證據。
    把它摺疊掉，門會以為那是本儲存庫根下的文件，從而報一條假斷鏈。
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
    """遞歸收集 bundle 裡的全部 Markdown，鍵是相對 docs/ 的 / 分隔路徑。"""
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
    """一份文件裡指向 Markdown 的連結，按它自己所在的目錄解析。"""
    directory = rel.rsplit("/", 1)[0] if "/" in rel else ""
    out = []
    for raw in MARKDOWN_LINK.findall(source_of(rel)):
        href = raw.split("#")[0]
        if not href or not href.endswith(".md") or "://" in href:
            continue
        # bundle 內連結寫成相對 bundle 根的絕對路徑（/governance/index.md）；
        # ../PLAN.md 這類出 bundle 的按文件自身位置解析到儲存庫根。
        if href.startswith("/"):
            from_repo = normalize("docs/" + href)
        else:
            from_repo = normalize("docs/" + directory + "/" + href)
        inside = from_repo.startswith("docs/")
        out.append((from_repo[len("docs/"):] if inside else from_repo, inside))
    return out


def reachable_from_index(files: list[str]) -> set[str]:
    """從入口出發的傳遞閉包 —— 「被列出」不夠，要走得到。"""
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

    # ★ 掃描沒瞎：遞歸確實進了子目錄。退化回「只有頂層」時，可達性會變成空斷言。
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
                # ★★★ 跨儲存庫引用（消不掉的 ..）不歸本門管：它指向 workspace 裡的兄弟
                #   儲存庫，本儲存庫既無權也無力保證它在，而克隆本儲存庫的人那裡它根本不存在。
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
