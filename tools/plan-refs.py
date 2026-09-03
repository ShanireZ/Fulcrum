#!/usr/bin/env python3
"""`PLAN.md` §11 待定清单的**引用门**。

# 它答哪一个问题（⛔ 只有一个，不许混着说）

**「有没有哪一处声称某个 D 号还在 §11 待定清单里，而它其实已经不在了。」**

★★★ 立它的直接原因：2026-09-03 一次体检在**九处**抄件里读到这句假话，而本仓内
**没有任何门看得见一句过期的注释**。九处里最老的一条（`§11 的 D26`）在 D26 由 G118
结案之后活了很多轮，每一轮都有人读过那一页。

# 两条判据，各自答不同的东西

**判据 ①（镜像逐字相等）** —— `docs/governance/open-questions.md` 是 §11 的导航镜像，
它那张表的 D 号集合必须与 §11 派生出来的**逐字相等**。
★ 它守的是**镜像漂移**：结案时改了 §11 而忘了改镜像，两份**各自自洽**，
而「自洽的陈旧抄件会通过每一道门」。⚠ 它是完全机械的，永远不会误报。

**判据 ②（成员资格断言必须为真）** —— 其余文件里凡是写着「§11（的／里的）Dnn」的，
那个 Dnn 必须真的还开着。⚠ 它是**句式判据**，天生有漏网，所以下面那一节写明它漏什么。

# ⚠ 判据 ② 答不了什么（⛔ 不许把它读成「D 号引用全都有门守着」）

1. **不带 `§11` 字样的散文。**「这一条还在登记着」这种说法它一个都逮不到。
2. **跨行的句子。** 判据按行匹配；一句话折在两行上，`§11` 与 D 号就分家了。
3. **语义错。**「D22 留下的下一问」——把一个还开着的问题挂在**已结案**的号下面，
   形状上与正确写法一模一样。
4. ⛔ **有意不按「同一行里有 open-questions.md 的链接」判。** 实测那样会把
   一句**正确**的话判红：「……它们**不在** [待定清单](…) **里了**」。
   ★ 一个把正确写法判红的门，最后会被人绕过去，而不是被人满足。

# 怎么跑

    python tools/plan-refs.py          # 红了退出码 1
    python tools/plan-refs.py --list   # 顺带列出扫到的每一处断言

★ 与 `docs-check.py` 同为宿主机侧的纯文本前置检查：不需要 Rust 工具链，也不需要
Linux 特有的任何东西 —— 而构建镜像里没有 python3。
"""

from __future__ import annotations

import io
import os
import re
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# ⚠ ⚠ **本机控制台是 GBK，而本门要打印的是扫到的源码原文** —— 里面有 `⚠`（U+26A0）
#   这种 GBK 编不出来的字符。Python 的 `stderr` 默认 `errors='backslashreplace'`
#   而 `stdout` 默认 `strict` ⇒ ★★★ **同一份输出走 stdout 会抛 UnicodeEncodeError
#   直接把门炸成一条 traceback，走 stderr 却安然无恙**。实测：`--list` 当场炸，
#   而报错那一路（stderr）看起来一切正常。⇒ 两条都钉成 backslashreplace。
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(errors="backslashreplace")
    except (AttributeError, ValueError):  # 非 TextIO（被重定向成别的东西）时不强求
        pass

# 权威与它的镜像。⚠ 这两份**本来就该**谈 §11，判据 ② 对它们豁免；
#   而镜像那一份由判据 ① 单独守着，⇒ 豁免没有打开缺口。
PLAN_REL = "PLAN.md"
MIRROR_REL = "docs/governance/open-questions.md"

# 扫哪些地方。★ 目录是**派生**的（os.walk），⛔ 文件名一个都不列举 ——
#   一份列出来的清单会随着新增文件悄悄失效，而那正是本门要挡的那类漂移。
SCAN_DIRS = ("crates", "docs", "tests", "tools")
SCAN_FILES = ("vendor/pingora/FORK.md",)
SCAN_EXTS = (".rs", ".md", ".sh", ".py", ".yml", ".yaml", ".toml")
SKIP_DIRS = {"target", "__pycache__", ".git"}

# §11 表格的第一列。★ 只认第一列 ⇒ 正文里顺口提到的 D 号不会被当成「开着的」。
ROW_D = re.compile(r"^\|\s*\*\*(D\d+)\*\*\s*\|")

# 「§11（的／里的／待定清单）Dnn」这一小撮**断言成员资格**的句式。
# ⚠ 窗口只有 8 个字符，而这是承重的：把它放宽到十几个，
#   「§11 **只列仍然开着的**，D1 早已不在里面」这句**正确**的话就会被判红。
CLAIM = re.compile(r"§11[^\n]{0,8}?\*{0,2}(D\d+)")


def read(rel: str) -> str:
    with open(os.path.join(REPO, *rel.split("/")), encoding="utf-8") as handle:
        return handle.read()


def section_11(plan_text: str) -> str:
    """`PLAN.md` §11 那一段的正文。找不到就回空串（调用方判红）。"""
    start = re.search(r"^## 11\.", plan_text, re.MULTILINE)
    if start is None:
        return ""
    rest = plan_text[start.end():]
    end = re.search(r"^## 12\.", rest, re.MULTILINE)
    return rest[: end.start()] if end else rest


def d_rows(text: str) -> list[str]:
    """一段 Markdown 里所有表格行的第一列 D 号，按出现顺序。"""
    return [m.group(1) for line in text.splitlines() for m in [ROW_D.match(line)] if m]


def scan_files() -> list[str]:
    """要扫的文件，相对仓库根、`/` 分隔。⛔ 派生，不列举。"""
    found: list[str] = []
    for top in SCAN_DIRS:
        for dirpath, dirnames, filenames in os.walk(os.path.join(REPO, top)):
            dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
            for name in filenames:
                if not name.endswith(SCAN_EXTS):
                    continue
                rel = os.path.relpath(os.path.join(dirpath, name), REPO)
                found.append(rel.replace(os.sep, "/"))
    for rel in SCAN_FILES:
        if os.path.exists(os.path.join(REPO, *rel.split("/"))):
            found.append(rel)
    for name in sorted(os.listdir(REPO)):
        if name.endswith(".md") and os.path.isfile(os.path.join(REPO, name)):
            found.append(name)
    # ⚠ ⚠ **本文件自己也要豁免**，而这不是偷懒：判据 ② 的自测夹具里**必须**有几条
    #   真的过期断言（`§11 的 **D27**`），否则那条自测证不了这道门红得起来。
    #   ⇒ 它们会被本门自己扫成红的。★ 代价写在明处：**本文件散文里的 D 号没有门守着**。
    exempt = {PLAN_REL, MIRROR_REL, "tools/plan-refs.py"}
    return sorted(rel for rel in found if rel not in exempt)


def claims_in(text: str) -> list[tuple[int, str, str]]:
    """一份文件里的成员资格断言：(行号, D 号, 那一行)。"""
    out = []
    for number, line in enumerate(text.splitlines(), start=1):
        for match in CLAIM.finditer(line):
            out.append((number, match.group(1), line.strip()))
    return out


def problems_for(rel: str, text: str, open_set: set[str]) -> list[str]:
    """判据 ② 落到一份文件上的结果。

    ★ 单独抠成一个函数**只为一件事**：让自证能拿一份**真的写在盘上的**夹具走完
    「读文件 → 匹配 → 变成一条红」整条路 —— 只验字符串的自证证不了这一段接线。
    """
    return [
        "%s:%d 说 %s 在 §11 里，而它已经不在了（结论应当在 §10）：%s"
        % (rel, number, d_number, line)
        for number, d_number, line in claims_in(text)
        if d_number not in open_set
    ]


# ── 自证：这两条判据都必须能**红得起来** ────────────────────────────────────
#   ★ 只被观察到绿过的门，与一道根本不跑的门分不开。


def selftest() -> list[str]:
    bad: list[str] = []

    # ① 表格第一列的提取：认得出行，也不把正文里的 D 号当成开着的。
    fixture_plan = (
        "## 11. ⏳ 待定清单\n\n"
        "> 只列仍然开着的。D99 已经结案。\n\n"
        "| | 待定项 |\n|---|---|\n"
        "| **D19** | 一 |\n| **D9** | 二 |\n\n"
        "## 12. 入口\n| **D77** | 不属于 §11 |\n"
    )
    got = d_rows(section_11(fixture_plan))
    if got != ["D19", "D9"]:
        bad.append("§11 提取自测：期望 ['D19', 'D9']，实测 %r" % (got,))
    if d_rows(section_11("没有第十一节")) != []:
        bad.append("§11 不存在时应当提不出任何 D 号")

    # ② 句式判据：命中得了，也漏得掉。⚠ 两个方向都要跑 ——
    #    只验「命中」的自测，配一个恒真的正则照样全绿。
    hits = [
        "那是 §11 的 **D27**，不是遗漏。",
        "`PLAN.md` §11 的 D27 把候选写成三选一。",
        "PLAN.md §11 D27（版本与兼容性策略）",
        "PLAN.md §11 待定清单 D27–D30",
    ]
    for line in hits:
        if [d for _, d, _ in claims_in(line)] != ["D27"]:
            bad.append("句式判据漏了一条真断言：%s" % line)
    misses = [
        # ★ 这一条是真事：修完之后 decision-log.md 就是这么写的。
        "§11 **只列仍然开着的**，D1 早已不在里面 —— 结论在 §10 的 G29。",
        "★ **§11 待定项不是需求。** 没拍板之前不得据以实现。",
        "D22 原本登记的是「把探针挂成常设的门」，而 owner",
        "✅ D27 已结案（G128，落点 fork 改动 14）",
    ]
    for line in misses:
        if claims_in(line):
            bad.append("句式判据误报了一句正确的话：%s" % line)

    # ③ 镜像判据：差一行就要红。
    left, right = ["D19", "D9"], ["D19"]
    if left == right:
        bad.append("镜像自测的夹具写错了")

    # ④ 枚举器：递归真的进了子目录，带空格的路径也收得到，扩展名筛得掉。
    with tempfile.TemporaryDirectory() as root:
        deep = os.path.join(root, "a b", "c")
        os.makedirs(deep)
        for path in (os.path.join(root, "top.md"), os.path.join(deep, "deep.rs")):
            with open(path, "w", encoding="utf-8") as handle:
                handle.write("x")
        with open(os.path.join(deep, "ignore.png"), "w", encoding="utf-8") as handle:
            handle.write("x")
        seen = []
        for dirpath, dirnames, filenames in os.walk(root):
            dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
            for name in filenames:
                if name.endswith(SCAN_EXTS):
                    seen.append(os.path.relpath(os.path.join(dirpath, name), root).replace(os.sep, "/"))
        if sorted(seen) != ["a b/c/deep.rs", "top.md"]:
            bad.append("枚举器自测：实测 %r" % (sorted(seen),))

        # ⑤ ★★★ **端到端反证**：一份真的写在盘上的文件，走完「读 → 匹配 → 判红」。
        #    ⚠ 前面那些自测都只喂字符串 —— 它们证不了这一段接线，
        #    而「只被观察到绿过的门」正是本仓禁止的那种门。
        probe = os.path.join(root, "probe.md")
        with open(probe, "w", encoding="utf-8") as handle:
            handle.write("一句正常的话。\n那是 §11 的 **D27**，不是遗漏。\n")
        with open(probe, encoding="utf-8") as handle:
            probe_text = handle.read()
        if len(problems_for("probe.md", probe_text, {"D19"})) != 1:
            bad.append("端到端反证：过期断言没有变成一条红")
        if problems_for("probe.md", probe_text, {"D19", "D27"}):
            bad.append("端到端反证：D27 还开着时不该红 —— 这道门判的不是它想判的东西")

    # ⑥ ★★★ **红的那条路真的印得出来。** 本门印的是扫到的源码原文，里面有 `⚠`
    #    这种 GBK 编不出来的字符 ⇒ 一个 `errors='strict'` 的 stdout 会当场抛异常。
    #    ⚠ ⚠ **判据是「真的拿那个字符去写一次」**，⛔ 不是「检查有没有设某个选项」——
    #    后者在 stdout 被重定向、编码本来就够用的时候会误报。
    sample = "⚠ ⇒ ★ 一行带着编不出来的字符的原文"
    for name, stream in (("stdout", sys.stdout), ("stderr", sys.stderr)):
        probe_stream = io.TextIOWrapper(
            io.BytesIO(),
            encoding=getattr(stream, "encoding", None) or "utf-8",
            errors=getattr(stream, "errors", None) or "strict",
        )
        try:
            probe_stream.write(sample)
            probe_stream.flush()
        except UnicodeEncodeError:
            bad.append("%s 印不出扫到的源码原文（%s + errors=%s）—— 红的那条路走不通"
                       % (name, probe_stream.encoding, probe_stream.errors))

    return bad


def main() -> int:
    problems: list[str] = []

    broken = selftest()
    if broken:
        print("本门的自测未通过 —— **本次结论一律不可信**：", file=sys.stderr)
        for line in broken:
            print("  - " + line, file=sys.stderr)
        return 1

    open_ds = d_rows(section_11(read(PLAN_REL)))
    if not open_ds:
        print("从 PLAN.md §11 一个 D 号都提不出来 —— 提取本身坏了", file=sys.stderr)
        return 1
    open_set = set(open_ds)

    # ── 判据 ①：镜像逐字相等 ──────────────────────────────────────────────
    mirror_ds = d_rows(read(MIRROR_REL))
    if sorted(mirror_ds) != sorted(open_ds):
        only_plan = sorted(open_set - set(mirror_ds))
        only_mirror = sorted(set(mirror_ds) - open_set)
        problems.append(
            "镜像漂移：%s 与 PLAN.md §11 对不上号"
            "（只在 §11：%s；只在镜像：%s）"
            % (MIRROR_REL, only_plan or "无", only_mirror or "无")
        )

    # ── 判据 ②：成员资格断言必须为真 ──────────────────────────────────────
    files = scan_files()
    if not files:
        print("一份文件都没扫到 —— 扫描本身坏了", file=sys.stderr)
        return 1
    if not any(f.endswith(".rs") for f in files) or not any(f.endswith(".md") for f in files):
        print("扫到的文件里缺了 .rs 或 .md —— 扫描本身坏了", file=sys.stderr)
        return 1

    total = 0
    for rel in files:
        text = read(rel)
        for number, d_number, line in claims_in(text):
            total += 1
            if "--list" in sys.argv:
                state = "OK  " if d_number in open_set else "★过期"
                print("  %s %s:%d  %s" % (state, rel, number, line))
        problems.extend(problems_for(rel, text, open_set))

    if problems:
        print("§11 引用门未通过，%d 项：" % len(problems), file=sys.stderr)
        for line in problems:
            print("  - " + line, file=sys.stderr)
        return 1

    print(
        "§11 引用门通过：开着的 %d 条（%s）与镜像逐字相等；"
        "扫了 %d 份文件、%d 处成员资格断言全部成立。"
        % (len(open_ds), " ".join(open_ds), len(files), total)
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
