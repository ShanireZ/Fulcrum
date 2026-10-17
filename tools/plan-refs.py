#!/usr/bin/env python3
"""`PLAN.md` §11 待定清單的**引用門**。

# 它答哪一個問題（⛔ 只有一個，不許混著說）

**「有沒有哪一處聲稱某個 D 號還在 §11 待定清單裡，而它其實已經不在了。」**

★★★ 立它的直接原因：2026-09-03 一次體檢在**九處**抄件裡讀到這句假話，而本儲存庫內
**沒有任何門看得見一句過期的註解**。九處裡最老的一條（`§11 的 D26`）在 D26 由 G118
結案之後活了很多輪，每一輪都有人讀過那一頁。

# 兩條判據，各自答不同的東西

**判據 ①（鏡像逐字相等）** —— `docs/governance/open-questions.md` 是 §11 的導覽鏡像，
它那張表的 D 號集合必須與 §11 派生出來的**逐字相等**。
★ 它守的是**鏡像漂移**：結案時改了 §11 而忘了改鏡像，兩份**各自自洽**，
而「自洽的陳舊抄件會通過每一道門」。⚠ 它是完全機械的，永遠不會誤報。

**判據 ②（成員資格斷言必須為真）** —— 其餘文件裡凡是寫著「§11（的／裡的）Dnn」的，
那個 Dnn 必須真的還開著。⚠ 它是**句式判據**，天生有漏網，所以下面那一節寫明它漏什麼。

# ⚠ 判據 ② 答不了什麼（⛔ 不許把它讀成「D 號引用全都有門守著」）

1. **不帶 `§11` 字樣的散文。**「這一條還在登記著」這種說法它一個都逮不到。
2. **跨行的句子。** 判據按行匹配；一句話折在兩行上，`§11` 與 D 號就分家了。
3. **語義錯。**「D22 留下的下一問」——把一個還開著的問題掛在**已結案**的號下面，
   形狀上與正確寫法一模一樣。
4. ⛔ **有意不按「同一行裡有 open-questions.md 的連結」判。** 實測那樣會把
   一句**正確**的話判紅：「……它們**不在** [待定清單](…) **裡了**」。
   ★ 一個把正確寫法判紅的門，最後會被人繞過去，而不是被人滿足。

# 怎麼跑

    python tools/plan-refs.py          # 紅了結束代碼 1
    python tools/plan-refs.py --list   # 順帶列出掃到的每一處斷言

★ 與 `docs-check.py` 同為宿主機側的純文字前置檢查：不需要 Rust 工具鏈，也不需要
Linux 特有的任何東西 —— 而建置鏡像裡沒有 python3。
"""

from __future__ import annotations

import io
import os
import re
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# ⚠ ⚠ **本機控制檯是 GBK，而本門要列印的是掃到的原始碼原文** —— 裡面有 `⚠`（U+26A0）
#   這種 GBK 編不出來的字元。Python 的 `stderr` 預設 `errors='backslashreplace'`
#   而 `stdout` 預設 `strict` ⇒ ★★★ **同一份輸出走 stdout 會拋 UnicodeEncodeError
#   直接把門炸成一條 traceback，走 stderr 卻安然無恙**。實測：`--list` 當場炸，
#   而回報錯誤那一路（stderr）看起來一切正常。⇒ 兩條都釘成 backslashreplace。
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(errors="backslashreplace")
    except (AttributeError, ValueError):  # 非 TextIO（被重定向成別的東西）時不強求
        pass

# 權威與它的鏡像。⚠ 這兩份**本來就該**談 §11，判據 ② 對它們豁免；
#   而鏡像那一份由判據 ① 單獨守著，⇒ 豁免沒有打開缺口。
PLAN_REL = "PLAN.md"
MIRROR_REL = "docs/governance/open-questions.md"

# 掃哪些地方。★ 目錄是**派生**的（os.walk），⛔ 檔案名一個都不列舉 ——
#   一份列出來的清單會隨著新增檔案悄悄失效，而那正是本門要擋的那類漂移。
SCAN_DIRS = ("crates", "docs", "tests", "tools")
SCAN_FILES = ("vendor/pingora/FORK.md",)
SCAN_EXTS = (".rs", ".md", ".sh", ".py", ".yml", ".yaml", ".toml")
SKIP_DIRS = {"target", "__pycache__", ".git"}

# §11 表格的第一列。★ 只認第一列 ⇒ 正文裡順口提到的 D 號不會被當成「開著的」。
ROW_D = re.compile(r"^\|\s*\*\*(D\d+)\*\*\s*\|")

# ★ ★ ★ 「§11 **真的空了**」與「表格格式漂了、`ROW_D` 沒認出來」——
#   這兩件事在表面上**一模一樣**：兩邊都是「一個 D 號都提不出來」。
#   ⇒ 用一個**比 `ROW_D` 寬**的篩子把它們分開（與 `tests/ci/shellcheck-all.sh` 那條
#     「普查那一步的篩子必須比門禁的寬」同形）：掃**任何表格行**裡的 D 號，
#     不限它在不在第一列、有沒有加粗。
#   ⚠ ⚠ **有意只掃表格行（`^|`），⛔ 不掃整段** —— §11 的說明文字裡本來就會
#     提到剛結案的那幾個號（「`D9` 與 `D20` 同日結案」），掃整段會把它誤判成格式漂移。
#   ★ 誠實說出它守不住什麼：**把整張表改寫成列表**（`- D20: …`）這類漂移它抓不到，
#     因為那不是表格行。那一格沒有門。
ROW_ANY_D = re.compile(r"^\|.*\bD\d+\b", re.MULTILINE)

# 「§11（的／裡的／待定清單）Dnn」這一小撮**斷言成員資格**的句式。
# ⚠ 視窗只有 8 個字元，而這是承重的：把它放寬到十幾個，
#   「§11 **只列仍然開著的**，D1 早已不在裡面」這句**正確**的話就會被判紅。
CLAIM = re.compile(r"§11[^\n]{0,8}?\*{0,2}(D\d+)")


def read(rel: str) -> str:
    with open(os.path.join(REPO, *rel.split("/")), encoding="utf-8") as handle:
        return handle.read()


def section_11(plan_text: str) -> str:
    """`PLAN.md` §11 那一段的正文。找不到就回空串（呼叫方判紅）。"""
    start = re.search(r"^## 11\.", plan_text, re.MULTILINE)
    if start is None:
        return ""
    rest = plan_text[start.end():]
    end = re.search(r"^## 12\.", rest, re.MULTILINE)
    return rest[: end.start()] if end else rest


def d_rows(text: str) -> list[str]:
    """一段 Markdown 裡所有表格行的第一列 D 號，按出現順序。"""
    return [m.group(1) for line in text.splitlines() for m in [ROW_D.match(line)] if m]


def scan_files() -> list[str]:
    """要掃的文件，相對儲存庫根、`/` 分隔。⛔ 派生，不列舉。"""
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
    # ⚠ ⚠ **本檔案自己也要豁免**，而這不是偷懶：判據 ② 的自測夾具裡**必須**有幾條
    #   真的過期斷言（`§11 的 **D27**`），否則那條自測證不了這道門紅得起來。
    #   ⇒ 它們會被本門自己掃成紅的。★ 代價寫在明處：**本檔案散文裡的 D 號沒有門守著**。
    exempt = {PLAN_REL, MIRROR_REL, "tools/plan-refs.py"}
    return sorted(rel for rel in found if rel not in exempt)


def claims_in(text: str) -> list[tuple[int, str, str]]:
    """一份文件裡的成員資格斷言：(行號, D 號, 那一行)。"""
    out = []
    for number, line in enumerate(text.splitlines(), start=1):
        for match in CLAIM.finditer(line):
            out.append((number, match.group(1), line.strip()))
    return out


def problems_for(rel: str, text: str, open_set: set[str]) -> list[str]:
    """判據 ② 落到一份文件上的結果。

    ★ 單獨摳成一個函式**只為一件事**：讓自證能拿一份**真的寫在盤上的**夾具走完
    「讀文件 → 匹配 → 變成一條紅」整條路 —— 只驗字串的自證證不了這一段串接。
    """
    return [
        "%s:%d 说 %s 在 §11 里，而它已经不在了（结论应当在 §10）：%s"
        % (rel, number, d_number, line)
        for number, d_number, line in claims_in(text)
        if d_number not in open_set
    ]


# ── 自證：這兩條判據都必須能**紅得起來** ────────────────────────────────────
#   ★ 只被觀察到綠過的門，與一道根本不跑的門分不開。


def selftest() -> list[str]:
    bad: list[str] = []

    # ① 表格第一列的提取：認得出行，也不把正文裡的 D 號當成開著的。
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

    # ①' ★ ★ 「真的空了」與「表格格式漂了」必須**分得開** —— 兩者表面上都是
    #     「一個 D 號都提不出來」，而後果相反（一個放行、一個當場紅）。
    #     ⇒ 兩個方向各釘一條；⛔ 只驗一邊的自測，配一個恆真或恆假的寬篩子照樣全綠。
    empty_plan = (
        "## 11. ⏳ 待定清单\n\n"
        "> 只列仍然开着的。`D9` 与 `D20` 已于 2026-09-07 结案。\n\n"
        "| | 待定项 |\n|---|---|\n\n"
        "## 12. 入口\n"
    )
    empty_body = section_11(empty_plan)
    if d_rows(empty_body) != []:
        bad.append("真的空的 §11 不该提出任何 D 号（夹具本身写错了）")
    if ROW_ANY_D.search(empty_body):
        bad.append(
            "宽筛子把**说明文字**里的 D 号当成了表格行 —— "
            "那会把「D 号全部结案」误判成「格式漂了」，结案那天就再也过不去"
        )

    drifted_plan = (
        "## 11. ⏳ 待定清单\n\n"
        "| | 待定项 |\n|---|---|\n"
        "| D19 | 第一列少了加粗 |\n\n"
        "## 12. 入口\n"
    )
    drifted_body = section_11(drifted_plan)
    if d_rows(drifted_body) != []:
        bad.append("少了加粗的那行不该被 ROW_D 认出来（夹具本身写错了）")
    if not ROW_ANY_D.search(drifted_body):
        bad.append("宽筛子没认出格式漂了的表格行 —— 那条分辨判据是瞎的，会把漂移放成「真的空」")

    # ② 句式判據：命中得了，也漏得掉。⚠ 兩個方向都要跑 ——
    #    只驗「命中」的自測，配一個恆真的正則照樣全綠。
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
        # ★ 這一條是真事：修完之後 decision-log.md 就是這麼寫的。
        "§11 **只列仍然开着的**，D1 早已不在里面 —— 结论在 §10 的 G29。",
        "★ **§11 待定项不是需求。** 没拍板之前不得据以实现。",
        "D22 原本登记的是「把探针挂成常设的门」，而 owner",
        "✅ D27 已结案（G128，落点 fork 改动 14）",
    ]
    for line in misses:
        if claims_in(line):
            bad.append("句式判据误报了一句正确的话：%s" % line)

    # ③ 鏡像判據：差一行就要紅。
    left, right = ["D19", "D9"], ["D19"]
    if left == right:
        bad.append("镜像自测的夹具写错了")

    # ④ 枚舉器：遞歸真的進了子目錄，帶空格的路徑也收得到，副檔名篩得掉。
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

        # ⑤ ★★★ **端到端反證**：一份真的寫在盤上的檔案，走完「讀 → 匹配 → 判紅」。
        #    ⚠ 前面那些自測都只喂字串 —— 它們證不了這一段串接，
        #    而「只被觀察到綠過的門」正是本儲存庫禁止的那種門。
        probe = os.path.join(root, "probe.md")
        with open(probe, "w", encoding="utf-8") as handle:
            handle.write("一句正常的话。\n那是 §11 的 **D27**，不是遗漏。\n")
        with open(probe, encoding="utf-8") as handle:
            probe_text = handle.read()
        if len(problems_for("probe.md", probe_text, {"D19"})) != 1:
            bad.append("端到端反证：过期断言没有变成一条红")
        if problems_for("probe.md", probe_text, {"D19", "D27"}):
            bad.append("端到端反证：D27 还开着时不该红 —— 这道门判的不是它想判的东西")

    # ⑥ ★★★ **紅的那條路真的印得出來。** 本門印的是掃到的原始碼原文，裡面有 `⚠`
    #    這種 GBK 編不出來的字元 ⇒ 一個 `errors='strict'` 的 stdout 會當場拋異常。
    #    ⚠ ⚠ **判據是「真的拿那個字元去寫一次」**，⛔ 不是「檢查有沒有設某個選項」——
    #    後者在 stdout 被重定向、編碼本來就夠用的時候會誤報。
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

    body_11 = section_11(read(PLAN_REL))
    open_ds = d_rows(body_11)
    if not open_ds:
        # ★ 三種「提不出來」，後果完全不同，⛔ 不許合成一種。
        if not body_11:
            print("PLAN.md 里找不到 §11 那一节 —— 提取本身坏了", file=sys.stderr)
            return 1
        if ROW_ANY_D.search(body_11):
            print(
                "§11 的表格行里有 D 号，而第一列一个都提不出来 —— "
                "**表格格式漂了**，⛔ 不是「真的空了」",
                file=sys.stderr,
            )
            return 1
        # ⇒ 真的空：D 號全部結案（2026-09-07 `D9` / `D20` 結案後第一次出現）。
        #   ★ 下面兩條判據**照常跑**，⛔ 不提前回傳：
        #     判據 ① 此刻要求鏡像**也**是空的；
        #     判據 ② 此刻會把每一處「§11 的 Dnn」斷言都紅出來 —— 那正是結案時最容易漏改的東西。
    open_set = set(open_ds)

    # ── 判據 ①：鏡像逐字相等 ──────────────────────────────────────────────
    mirror_ds = d_rows(read(MIRROR_REL))
    if sorted(mirror_ds) != sorted(open_ds):
        only_plan = sorted(open_set - set(mirror_ds))
        only_mirror = sorted(set(mirror_ds) - open_set)
        problems.append(
            "镜像漂移：%s 与 PLAN.md §11 对不上号"
            "（只在 §11：%s；只在镜像：%s）"
            % (MIRROR_REL, only_plan or "无", only_mirror or "无")
        )

    # ── 判據 ②：成員資格斷言必須為真 ──────────────────────────────────────
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
