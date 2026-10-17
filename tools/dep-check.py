#!/usr/bin/env python3
"""每週相依檢查 —— 實作 PLAN.md G29 的「追新 + 24 小時安全懷疑期」。

G29 的口徑是：`Cargo.toml` 用 `>=` 下界、不設上界，追最新（**包括破壞性大版本**），
但**任何版本必須已發佈滿 24 小時才允許採納**。後半句 Cargo 表達不了，只能由本指令碼執行。

這道懷疑期擋的是一類具體攻擊：投毒版本發佈後數小時內被發現並 yank。盲目 `cargo update`
會正好撞上那個視窗；等滿 24 小時再取，絕大多數這類事件已經暴露。

做法：
  1. 用 `cargo update --dry-run` 問出「如果現在更新，會動哪些包」——只有這批才需要查；
     全量查 Cargo.lock 裡幾百個包純屬浪費。★ 根清單與 vendor 清單各問一次，vendor 那份只收
     根鎖裡沒有的版本（見 `vendor_only`）。
  2. 對每個候選版本查 crates.io 的發佈時間與 yanked 狀態。
  3. 發佈不足 N 小時、或已被 yank 的，一律擋下並說明原因。
  4. `--apply` 時只對**通過懷疑期**的候選做 `cargo update -p <name> --precise <ver>`；
     成敗看跑完之後的兩把鎖，不看 cargo 的結束代碼（同族包會被前一項順帶帶走，見 `adopt_root`）。
     ★ 順帶帶進來的版本同樣要過懷疑期：兩把鎖採納前後逐包比，有一個過不了就**整批作廢、
     兩把鎖逐位元組還原**（見 `lock_intruders`）。

★ 另有一件 `cargo update` 結構上做不到的事：**盯住被 fork 的 pingora-core 的上游**。

G30 把 `pingora-core` 用 `[patch.crates-io]` 指向了 `vendor/pingora/`，於是它**徹底脫離了
crates.io 的更新流**——`cargo update` 再也不會為它查任何東西。上游發了 0.9 也不會有任何提示，
而 fork 的全部價值（抬上界、修掉兩條公告）恰恰要靠跟進上游才不會隨時間腐化。
所以本指令碼單獨查一次上游最新版，與 fork 的基線比對。

★ 第三件同樣在 `cargo update` 視野之外的事：**建置鏡像的編譯器**（G36 / D13 結案）。

`docker/Dockerfile.build` 的基礎鏡像已釘到 digest，好處是可重現，代價是**不會自己跟進**。
G29 的精神是追新，所以這個釘子要定期拔——但拔的判據不是「digest 變沒變」：

★ **建置鏡像不是執行環境鏡像。** G13 定的散布是 musl 靜態二進制、§6.2 明確不做官方容器鏡像，
所以鏡像裡的 Debian 包（cmake / clang / glibc）**一個都不進產物**，只影響編譯過程。
真正影響產物的變數只有 **rustc 版本**。因此本檢查只在 rustc 變了時才影響結束代碼；
Debian 側的 digest 漂移只打一行提示。

★ 與 fork rebase 一樣，它**只報告、拒絕自動應用**（`--apply` 也不動）：換編譯器必須重跑
三場景，不該無人值守——一次靜默的編譯器切換正是修掉的那個洞。

用法：
    python tools/dep-check.py                 # 只報告
    python tools/dep-check.py --apply         # 採納通過懷疑期的更新
    python tools/dep-check.py --hours 48      # 換一個懷疑期長度
    python tools/dep-check.py --skip-fork     # 跳過 fork 上游檢查
    python tools/dep-check.py --skip-image    # 跳過建置鏡像檢查

結束代碼（可疊加）：
    0   沒有任何要處理的
    10  有可採納的 cargo 更新（--apply 模式下表示已採納）
    20  ★ fork 那條線要人管：上游有新版需要人工 rebase / **本次沒能查證**
    40  ★ 建置鏡像要人管：rustc 有新版 / 釘子掉了 / **pebble 有新版**（G64）/ **本次沒能查證**
    80  ★ systemd 測試宿主鏡像要人管（G39）：大版本變了 / **本次沒能查證**
   160  ★ 有未登記的安全公告 / **本次沒能查證**（新增，見下）
   255  ★ 上面幾檔同時亮到裝不下了（見 `finish()`）——**不是第六檔**，是「說不清楚」
    1   出錯；`--apply` 時也表示採納沒成（含順帶帶進沒過懷疑期的版本、整批作廢，見 `lock_intruders`）

★ 20 / 40 / 80 / 160 都把「沒能查證」算作**要人管**，不算作「無事」。這是本專案複審反覆抓到的
  那個形狀：把失敗當成成功的回傳值。它們各自只發一兩個請求，失敗多半是真有問題，重跑也便宜。

    （各位可疊加，所以 30、50、170、250 之類都是合法組合。
      ⚠ 但**加起來不能超過 255**：五檔全亮是 310，POSIX 截斷成 54，
      而 54 按本編碼解出來是「10+40+4」——一個不存在、而且看起來更輕的組合。
      `finish()` 負責在越界時改報 255 並逐條列出真正亮著的那幾檔。）

★ ★ 160 那一項為什麼在這裡而不是另起一個指令碼：`supply-audit.py` 預設**只審根鎖**，
  而且**沒有任何東西自動呼叫它**——它一直是一件「記得的時候跑一下」的事。
  代價兌現了一次：`h2` 的 RUSTSEC-2026-0258 是**手動跑一次 vendor 鎖才撞見的**。
  ⚠ 更要緊的是**產品的真實相依住在 vendor 鎖裡**（`[patch.crates-io]` 把 `pingora-core`
  指向 `vendor/pingora`），而根鎖連一個 rustls 相關的包都沒有。**最該被掃的那把鎖，恰恰沒人掃。**
  並進本指令碼而不另設節奏，理由同 G36：**沒人記得住第二件事**。
"""

from __future__ import annotations

import argparse
import contextlib
import io
import json
import re
import subprocess
import sys
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

# Windows 控制檯預設不是 UTF-8，中文輸出會變亂碼。強制一下，免得每週看到的都是問號。
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8")
    except (AttributeError, ValueError):
        pass

REPO = Path(__file__).resolve().parent.parent
UA = "fulcrum-dep-check (https://github.com/ShanireZ/Fulcrum)"

# G30 的 fork。基線版本直接讀 fork 裡 pingora-core 的 `version`，
# 不另設一份需要手動同步的記錄——那種記錄遲早與事實分家。
FORK_CRATE = "pingora-core"
FORK_MANIFEST = REPO / "vendor" / "pingora" / FORK_CRATE / "Cargo.toml"
FORK_DOC = "vendor/pingora/FORK.md"
FORK_UPSTREAM_REPO = "cloudflare/pingora"

# ★ 只看發版是不夠的。上游 main 上的相依上界改動在發版前就有價值——
#   nix 0.24→0.31 那條就是先落在 main、我們直接照抄省下一整輪遷移的。
#   所以除了比對 crates.io 的版本，還看一眼 main 領先基線 tag 多少提交。
DEP_BUMP_HINT = re.compile(r"\b(bump|upgrade|update|deps?|dependenc)", re.I)

PKG_VERSION = re.compile(r'^\s*version\s*=\s*"([^"]+)"', re.M)

# ── 建置鏡像（G36 / D13）─────────────────────────────────────────────────────
# ★ 釘定的 tag 與 digest **只從 Dockerfile 讀**，不另存一份——同 fork 基線的道理：
#   第二份記錄遲早與事實分家（這個專案已經栽過一次：文件說 trixie，鏡像裡是 bookworm）。
BUILD_DOCKERFILE = REPO / "docker" / "Dockerfile.build"
BUILD_DOC = "docs/platform/build-and-test.md"
# 上游用來「追新」的那個浮動 tag。釘子就是從它身上取下來的。
BASE_FLOATING_TAG = "1-trixie"
BASE_HUB_REPO = "library/rust"
# FROM rust:1.97.1-trixie@sha256:3382bd…
FROM_LINE = re.compile(
    r"^FROM\s+(?P<repo>[^\s:@]+):(?P<tag>[^\s@]+?)(?:@(?P<digest>sha256:[0-9a-f]{64}))?\s*$",
    re.M,
)
# 從 `1.97.1-trixie` 裡取出 `1.97.1`
TAG_VERSION = re.compile(r"^(\d+(?:\.\d+)*)-")

# ── pebble（G64）：門禁裡那個本機 ACME CA ─────────────────────────────────────
# ★ 它與 rustc 釘在**同一個檔案**裡，處置也一樣（改檔案 → 重建鏡像 → 重跑門禁），
#   所以並進 40 那一檔，不新開一位——結束代碼只有 8 位，現有五檔相加已經 310。
PEBBLE_REPO = "letsencrypt/pebble"
PEBBLE_PIN = re.compile(r"^ARG\s+PEBBLE_VERSION=(\S+)", re.MULTILINE)
PEBBLE_SHA_ARGS = (
    "PEBBLE_SHA256_AMD64",
    "PEBBLE_SHA256_ARM64",
    "CHALLTESTSRV_SHA256_AMD64",
    "CHALLTESTSRV_SHA256_ARM64",
)

# ── systemd 測試宿主鏡像（G39 / ）───────────────────────────────────
# ★ 與 G36 的建置鏡像**同構但不同判據**：那邊判 rustc，這邊判 **systemd 大版本**。
#   理由：M1 spike #1 的結論（alien MainPID 之下 systemctl stop 不再等排空）是
#   **對某個 systemd 版本成立**的，版本本身就是結論的一部分。浮動 tag 會讓結論某天
#   悄悄失效，而沒有任何一行輸出會說出來——這正是建置鏡像那次栽過的形狀。
SYSTEMD_DOCKERFILE = REPO / "docker" / "Dockerfile.systemd"
SYSTEMD_HUB_REPO = "library/debian"
SYSTEMD_DOC = "docs/verification/m1-systemd.md"
# ★ 釘定的 systemd 版本寫在 Dockerfile 裡一行**機器可讀**的註解上，與 FROM 行同處一個檔案。
#   不另存第二份——同 fork 基線與建置鏡像的道理（第二份記錄遲早與事實分家）。
PINNED_SYSTEMD = re.compile(r"^#\s*fulcrum-pinned-systemd:\s*(?P<ver>\S+)\s*$", re.M)
# Debian 的原始碼 API：一個很小的 JSON，直接給出每個 suite 當前的版本。純 HTTP。
DEB_SOURCES_API = "https://sources.debian.org/api/src/systemd/"
# 從 `257.13-1~deb13u1` 裡取出大版本 `257`
SYSTEMD_MAJOR = re.compile(r"^(\d+)")

# cargo 的 dry-run 輸出形如：
#     Updating pingora-core v0.8.1 -> v0.9.0
#       Adding some-new-crate v1.2.3
#
# ★ 行尾可能再掛一個括號註記，**必須認**：不認，這一行整條被丟 —— 不報、不過懷疑期、不採納，一點聲響都沒有。
#   2026-10-16 在 cargo 1.98.0（fulcrum-build:local）裡實測到的兩種：
#     Updating zstd v0.12.4 -> v0.13.3 (available: v0.14.2)                      有更新、但跨了大版本，`cargo update` 夠不到
#     Updating clap v4.0.0 -> v4.5.61 (available: v4.6.7, requires Rust 1.85)    更新的那版要更高的 Rust（resolver 3 的 MSRV 退回）
#   `Adding` 同樣帶。另有三種照 cargo 原始碼（797e8a9bc `ops/cargo_update.rs` 的 `report_*`）才知道的：
#   `(requires Rust X)`（選中的版本比 rust-version 還新）、`(published 3 hours ago, minimum age 7 days)` 與
#   `(available: vX, published …)`（後兩種要 -Zmin-publish-age，穩定版打不出來）。
#   ⇒ 不枚舉內容，認「行尾恰好一個不含括號的括號註記」：將來再冒出新種類，也不至於重蹈覆轍。
#   `Downgrading` / `Removing` / `Unchanged` 不是採納候選，不解析（`Downgrading` 一樣帶註記，見 [`_self_check_dry_run`]）。
_NOTE = r"(?:\s+\([^()]*\))?"
UPDATING = re.compile(r"^\s*Updating\s+(\S+)\s+v(\S+)\s+->\s+v(\S+)" + _NOTE + r"\s*$")
ADDING = re.compile(r"^\s*Adding\s+(\S+)\s+v(\S+)" + _NOTE + r"\s*$")


def rel(p: Path) -> str:
    """相對儲存庫根的顯示名；路徑不在儲存庫裡時退回絕對路徑，**不拋異常**。

    ★ 這不是潔癖：`relative_to` 在路徑不在 REPO 下時會 raise，而它出現在**錯誤分支**的
      列印語句裡——也就是說，唯一會走到它的場合恰恰是「已經出問題了」的時候，
      再拋一個 ValueError 只會把真正的診斷蓋掉。
    """
    return str(p.relative_to(REPO)) if p.is_relative_to(REPO) else str(p)


def run(cmd: list[str], *, cwd: Path = REPO) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)


def cargo_candidates(cargo: list[str], manifest: str | None = None) -> list[tuple[str, str | None, str]]:
    """回傳 [(crate, 當前版本 or None, 候選版本)]；`manifest` 給了就問那份清單（vendor），否則問根。"""
    proc = run([*cargo, "update", "--dry-run", *(["--manifest-path", manifest] if manifest else [])])
    out = (proc.stdout or "") + (proc.stderr or "")
    if proc.returncode != 0:
        print(out, file=sys.stderr)
        raise SystemExit(f"cargo update --dry-run 失败（退出码 {proc.returncode}）")

    return parse_dry_run(out)


def parse_dry_run(out: str) -> list[tuple[str, str | None, str]]:
    """`cargo update --dry-run` 的輸出 → [(crate, 當前版本 or None, 候選版本)]。純文字進、列表出 —— `--self-check` 直接餵它。

    只認 `Updating` 與 `Adding`（含行尾括號註記，見 [`UPDATING`] 上的說明）；`Downgrading` / `Removing` / `Unchanged`、
    `Locking …` 之類的表頭、`warning:` 與 `note:` 都不是採納候選。來源不是 crates.io 的包，行裡舊版本後面跟著 `(来源)`，
    本來就匹配不上 —— 它們沒有 crates.io 的發佈時間可判懷疑期。
    """
    found: list[tuple[str, str | None, str]] = []
    for line in out.splitlines():
        m = UPDATING.match(line)
        if m:
            found.append((m.group(1), m.group(2), m.group(3)))
            continue
        m = ADDING.match(line)
        if m:
            found.append((m.group(1), None, m.group(2)))
    return found


def vendor_only(candidates, root_lock: dict[str, set[str]]) -> list[tuple[str, str | None, str]]:
    """vendor 清單 dry-run 的候選裡，**根那條路夠不到**的那些：`名字@旧版本` 根鎖裡沒有。

    # ★ 為什麼要有這一步

    候選原先只來自根清單的 `cargo update --dry-run`，`--apply` 再把它們鏡像進 vendor 鎖。
    ⇒ 只在 vendor 鎖裡的版本（`pingora-core` 的 dev-dep、vendor 成員才開的可選相依拖進來的）**永遠不是候選**：
      2026-10-16 vendor 的 dry-run 動 29 項，25 項是這一類，其中 `quinn-proto` 0.11.16 帶五條公告
      （RUSTSEC-2026-0338…0342），而本指令碼一條都不報。

    ★ `名字@旧版本` 根鎖也有的**不收**：它歸根那條路決定、`_align_vendor_lock` 照舊鏡像 ——
      兩邊各自決定，兩把鎖就會對不上（`tests/vendor/run.sh` [2/5]）。同名、但舊版本只在 vendor 的
      （vendor 合法地多一個老大版本，如 `rand` 0.8）照收。新增包（舊版本為空）看的是新版本根鎖有沒有。
    """
    return [(name, cur, new) for name, cur, new in candidates
            if (cur or new) not in root_lock.get(name, set())]


def crate_versions(name: str) -> dict[str, dict]:
    url = f"https://crates.io/api/v1/crates/{name}"
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=30) as resp:
        data = json.load(resp)
    return {v["num"]: v for v in data.get("versions", [])}


def parse_ts(raw: str) -> datetime:
    # crates.io 給的是 RFC3339，尾巴可能是 Z
    return datetime.fromisoformat(raw.replace("Z", "+00:00"))


def quarantine_verdict(name: str, version: str, cutoff: datetime) -> tuple[str | None, str]:
    """一個具體版本過沒過懷疑期：`(None, 说明)` = 可採納，`(原因, 说明)` = 擋下。

    ★ 候選清單與採納之後的闖入者核對（[`lock_intruders`]）共用這一個判據 —— 各寫一份遲早分家。
      查不到、已 yank 同樣擋下：沒能查證不算通過。
    """
    try:
        versions = crate_versions(name)
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError) as e:
        return "查不到", f"crates.io 查询失败：{e}"

    meta = versions.get(version)
    if meta is None:
        return "查不到", "crates.io 上没有这个版本（本地索引比线上新？）"
    if meta.get("yanked"):
        return "已 yank", "该版本已被撤回"

    published = parse_ts(meta["created_at"])
    age_h = (datetime.now(timezone.utc) - published).total_seconds() / 3600
    if published > cutoff:
        return "怀疑期内", f"发布于 {published:%Y-%m-%d %H:%M UTC}，仅 {age_h:.1f} 小时"
    return None, f"发布于 {published:%Y-%m-%d %H:%M UTC}，{age_h:.0f} 小时前"


def screen(candidates, vet) -> list[tuple[str, str | None, str]]:
    """候選逐個過 `vet`（同 [`quarantine_verdict`]），打出 ✓ / ✗，回傳過了的那些。根與 vendor 獨有的兩批共用。"""
    eligible, blocked = [], []
    for name, cur, new in candidates:
        label = f"{name} {cur or '(新增)'} -> {new}"
        why, detail = vet(name, new)
        if why:
            blocked.append((label, why, detail))
        else:
            eligible.append((name, cur, new))
            print(f"  ✓ {label}    （{detail}）")
    if blocked:
        print("\n被挡下的：")
        for label, why, detail in blocked:
            print(f"  ✗ {label}    [{why}] {detail}")
    return eligible


def version_key(v: str) -> tuple:
    """粗略的 semver 排序鍵；只用於「上游是不是比基線新」這一個判斷。"""
    core = re.split(r"[-+]", v, maxsplit=1)[0]
    return tuple(int(x) if x.isdigit() else 0 for x in core.split("."))


# ★ 本儲存庫有**兩把鎖**，而 `tests/vendor/run.sh` 要求它們對同一個包解出同一組版本。
#   ⚠ 定義在這裡而不是各處寫字面值，是因為之前它只寫在
#     `ADVISORY_LOCKS` 裡，而 `--apply` 那條路**完全不知道第二把鎖的存在**。
VENDOR_LOCK = "vendor/pingora/Cargo.lock"
VENDOR_MANIFEST = "vendor/pingora/Cargo.toml"


def fork_baseline() -> str | None:
    """fork 當前對齊的上游版本 = vendor 裡 pingora-core 的 `version`。"""
    if not FORK_MANIFEST.exists():
        return None
    # 只看 [package] 段，避免撞上相依項裡的 version
    text = FORK_MANIFEST.read_text(encoding="utf-8").split("[dependencies]")[0]
    m = PKG_VERSION.search(text)
    return m.group(1) if m else None


def check_upstream_main(baseline: str) -> None:
    """★ 看一眼上游 main 相對基線 tag 領先多少，並挑出像相依改動的提交。

    只報告、不影響結束代碼——main 上的東西還沒發版，是「值得提前撈」而不是「必須跟進」。
    """
    url = f"https://api.github.com/repos/{FORK_UPSTREAM_REPO}/compare/{baseline}...main"
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "application/vnd.github+json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.load(resp)
    except urllib.error.HTTPError as e:
        if e.code == 403:
            print("  （GitHub API 限流，跳过 main 分支检查——未认证时每小时 60 次）")
        elif e.code == 404:
            print(f"  （比不了 {baseline}...main：上游没有这个 tag？）")
        else:
            print(f"  （GitHub 查询失败 {e.code}，跳过 main 分支检查）")
        return
    except (urllib.error.URLError, TimeoutError) as e:
        print(f"  （GitHub 查询失败：{e}，跳过 main 分支检查）")
        return

    ahead = data.get("ahead_by", 0)
    behind = data.get("behind_by", 0)
    status = data.get("status", "?")
    if not ahead and not behind:
        print(f"  上游 main 与 tag {baseline} 齐平，没有未发版的改动。")
        return

    print(f"  ★ 上游 main 相对 tag {baseline}：**{status}**，领先 {ahead} 个 / 落后 {behind} 个提交")

    # ★ ★ ★ 「落後」這一半必須先說，而且要說得比「領先」那一半響。
    #   實測：main 與 0.8.1 是 **diverged**（共同祖先是 0.8.0），
    #   而 main **落後 8 個提交**，那 8 個裡有 `RUSTSEC-2026-0098/0099 fixes` 與
    #   `Bound default HTTP/2 server limits to mitigate memory exhaustion`。
    #   ⇒ 本函式原先唯讀 `ahead_by`，於是它說的是「main 領先 190 個提交」——
    #     一句會被讀成「main ⊇ tag + 190」的話，而那是假的。
    #   ★ 那句話當天真的把一個決定引到了「從 main 撈改動」上。
    if behind:
        print(f"    ⚠ ⚠ ★ **main 上没有 tag {baseline} 的那 {behind} 个提交** —— 它们不是「旧的」，")
        print("      是**只存在于发布线上的**。⇒ 把 fork 挪到 main、或按 main 重做 vendor，")
        print("      会**静默丢掉**它们；下面这几条尤其要自己看一眼是不是安全修复：")
        for msg in _missing_on_main(baseline):
            print(f"      · {msg[:100]}")

    interesting = [
        c["commit"]["message"].splitlines()[0]
        for c in data.get("commits", [])
        if DEP_BUMP_HINT.search(c["commit"]["message"].splitlines()[0])
    ]
    if interesting:
        verb = "定向捞**单条**" if behind else "提前捞"
        print(f"    其中 {len(interesting)} 条看起来与依赖有关——★ 可以{verb}过来，省掉一整轮迁移：")
        for msg in interesting[-8:]:
            print(f"      · {msg[:100]}")
        if behind:
            print("      ⚠ **只捞单条，不要整体对齐 main** —— 理由见上面那一段。")
    print(f"    （只是提示，不影响退出码。要捞的话见 {FORK_DOC}）")


def _missing_on_main(baseline: str) -> list[str]:
    """反向比一次，列出「只在 tag 上、不在 main 上」的提交標題。

    ★ 單獨一次請求，因為 `compare/A...B` **只給 A→B 那一個方向的 commits 列表**：
    `behind_by` 是個數字，而那些提交的**內容**要反著問一次才拿得到。
    ⚠ 拿不到就說「拿不到」，不回傳空列表冒充「沒有」——
    本儲存庫為「某處沒有記錄 ≠ 那件事沒發生」付過賬。
    """
    url = f"https://api.github.com/repos/{FORK_UPSTREAM_REPO}/compare/main...{baseline}"
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "application/vnd.github+json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.load(resp)
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError) as e:
        return [f"（这一份没查成：{e} —— 不是「没有」，是「没问到」）"]
    return [c["commit"]["message"].splitlines()[0] for c in data.get("commits", [])]


def check_fork_upstream(cutoff: datetime) -> bool:
    """★ 被 patch 的 crate 脫離了 cargo 的更新流，必須單獨查。回傳 True 表示**這條線要人管**。

    ★ ★ 「要人管」包含「沒能查證」，不只是「上游有新版」。
    原先這兩種網路失敗都 `return False`（= 無事），於是 crates.io 一抖，每週檢查就會
    輸出「fork 基線仍是上游最新發布版」——**它並不知道，卻把不知道說成了知道**。
    這與同文件建置鏡像檢查、`supply-audit.py` 的查證覆蓋率是同一條紀律。
    """
    baseline = fork_baseline()
    if baseline is None:
        print(f"★ 没找到 {rel(FORK_MANIFEST)} —— fork 基线无从读起，这条线**没有查证**。")
        print("  （不是「无事」：G30 的 fork 是本项目最重要的那个依赖，读不到它的基线就等于没检查。）")
        return True

    try:
        versions = crate_versions(FORK_CRATE)
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, json.JSONDecodeError) as e:
        print(f"★ 没能向 crates.io 查证 {FORK_CRATE} 的上游版本：{e}")
        print("  ★ 这**不等于**上游没有新版——本次 fork 那一栏没有结论，换网络重跑。")
        return True

    live = [v for v in versions.values() if not v.get("yanked")]
    newer = [v for v in live if version_key(v["num"]) > version_key(baseline)]
    if not newer:
        print(f"fork 基线 {FORK_CRATE} {baseline} 仍是上游最新发布版，无需 rebase。")
        check_upstream_main(baseline)
        return False

    newest = max(newer, key=lambda v: version_key(v["num"]))
    published = parse_ts(newest["created_at"])
    age_h = (datetime.now(timezone.utc) - published).total_seconds() / 3600
    quarantined = published > cutoff

    print(f"★ {FORK_CRATE} 上游已到 {newest['num']}，而 fork 基线是 {baseline}"
          f"（发布于 {published:%Y-%m-%d %H:%M UTC}，{age_h:.0f} 小时前）"
          f"{'  ——【仍在怀疑期内】' if quarantined else ''}")
    if len(newer) > 1:
        print(f"    期间共发布 {len(newer)} 个版本：{', '.join(sorted(v['num'] for v in newer))}")
    print(f"""
    ⚠ 这一条 **不能自动采纳** —— fork 改的是 pingora 各 Cargo.toml 的版本上界，
      要在新基线上重做一遍，再修随之而来的调用点。步骤见 {FORK_DOC}。
      ★ 先看上游有没有已经自己抬了某几条（nix 那次就是白捡的）。
      ★ 做完照例跑 `bash tests/m0/docker-run.sh` —— fork 有一处改动正落在 transfer_fd 上。""")
    check_upstream_main(baseline)
    return not quarantined


def hub_tag_digests(repo: str, name_filter: str) -> dict[str, str]:
    """Docker Hub 上某儲存庫里名字含 `name_filter` 的 tag → digest。不需要 Docker，也不拉鏡像。

    ★ 兩處檢查（建置鏡像 / systemd 測試宿主）共用這一份翻頁邏輯，不各寫一份——
      本儲存庫已經在「兩份收尾指令碼分頭長歪」上吃過一次虧。
    """
    tags: dict[str, str] = {}
    url = (
        f"https://hub.docker.com/v2/repositories/{repo}/tags"
        f"?page_size=100&name={name_filter}"
    )
    for _ in range(5):  # 有界翻頁，別寫成無限循環
        req = urllib.request.Request(url, headers={"User-Agent": UA})
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.load(resp)
        for t in data.get("results", []):
            if t.get("digest"):
                tags[t["name"]] = t["digest"]
        url = data.get("next")
        if not url:
            break
    return tags


def hub_trixie_tags() -> dict[str, str]:
    """Docker Hub 上所有含 `trixie` 的 rust tag → digest。"""
    return hub_tag_digests(BASE_HUB_REPO, BASE_FLOATING_TAG.split("-")[-1])


def debian_suite_systemd(suite: str) -> str | None:
    """Debian 某個 suite 當前的 systemd 版本（如 trixie → `257.13-1~deb13u1`）。"""
    req = urllib.request.Request(DEB_SOURCES_API, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=30) as resp:
        data = json.load(resp)
    for v in data.get("versions", []):
        if suite in v.get("suites", []):
            return v.get("version")
    return None


def check_build_image() -> bool:
    """★ 建置鏡像的編譯器有沒有落後（G36 / D13）。回傳 True = 需要人管。

    判據只有一個：**rustc 版本**。Debian 側的 digest 漂移只提示，不判紅——
    那些包不進產物（G13 是 musl 靜態二進制），把它算作失敗會讓這道門幾乎常年亮著，
    而**永遠亮著的告警等於沒有告警**。
    """
    if not BUILD_DOCKERFILE.exists():
        print(f"★ 找不到 {rel(BUILD_DOCKERFILE)} —— 构建镜像无从查证。")
        return True

    m = FROM_LINE.search(BUILD_DOCKERFILE.read_text(encoding="utf-8"))
    if m is None:
        print(f"★ {rel(BUILD_DOCKERFILE)} 里没找到可解析的 FROM 行。")
        return True

    pinned_tag, pinned_digest = m.group("tag"), m.group("digest")
    # ★ 這一條同時守著自己的修復：釘子被人拿掉了，本檢查就該紅。
    if not pinned_digest:
        print(f"★ 基础镜像 `{m.group('repo')}:{pinned_tag}` **没有钉 digest**——浮动 tag 与 §8"
              f"「对拍环境可复现」直接冲突。见 {BUILD_DOC}。")
        return True

    ver_m = TAG_VERSION.match(pinned_tag)
    if ver_m is None:
        print(f"★ 钉定的 tag `{pinned_tag}` 里读不出精确版本号（应形如 `1.97.1-trixie`）。")
        return True
    pinned_ver = ver_m.group(1)

    try:
        tags = hub_trixie_tags()
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, json.JSONDecodeError) as e:
        # ★ 查不到 ≠ 沒落後。**「沒能檢查」不許當成「檢查通過」**——這是本專案複審
        #   兩輪反覆抓到的形狀。只有一個請求，失敗多半是真有問題，重跑也便宜。
        print(f"★ 没能向 Docker Hub 查证构建镜像：{e}")
        print("  ★ 这**不等于**镜像是最新的——本次构建镜像那一栏没有结论，换网络重跑。")
        return True

    live_digest = tags.get(BASE_FLOATING_TAG)
    if live_digest is None:
        print(f"★ Docker Hub 上没查到 `{BASE_HUB_REPO}:{BASE_FLOATING_TAG}`——tag 改名了？")
        return True

    if live_digest == pinned_digest:
        print(f"构建镜像已是 `{BASE_FLOATING_TAG}` 当前指向的那一个（rustc {pinned_ver}），无需拔钉子。")
        return False

    # digest 變了。★ 只有 rustc 也變了才判紅。
    # 找出與浮動 tag 同 digest 的那個精確版本 tag，例如 `1.97.2-trixie`。
    precise = sorted(
        (n for n, d in tags.items() if d == live_digest and TAG_VERSION.match(n)),
        key=lambda n: version_key(TAG_VERSION.match(n).group(1)),
        reverse=True,
    )
    live_ver = TAG_VERSION.match(precise[0]).group(1) if precise else None

    if live_ver is None:
        print(f"★ `{BASE_FLOATING_TAG}` 的 digest 变了，但配不出它对应的精确版本 tag——需要人工看一眼。")
        return True

    if version_key(live_ver) <= version_key(pinned_ver):
        # 同一個 rustc，只是 Debian 側重打了包。按 G36 只提示。
        print(f"构建镜像的 rustc 仍是 {pinned_ver}，无需拔钉子。")
        print(f"  （`{BASE_FLOATING_TAG}` 的 digest 已重打为 {live_digest[:19]}…，Debian 侧的更新；"
              f"这些包不进产物，按 G36 只提示不判红）")
        return False

    print(f"★ 构建镜像的 rustc 已到 **{live_ver}**，而 {rel(BUILD_DOCKERFILE)} 钉的是 {pinned_ver}。")
    print(f"""
    ⚠ 这一条 **不能自动采纳**（G36）—— 换编译器必须重跑三场景，不该无人值守。
      步骤（{BUILD_DOC} 的「怎么升」一节）：
        1. 把 FROM 换成 rust:{live_ver}-trixie@{live_digest}
        2. bash tests/m0/docker-run.sh    # 镜像会因内容哈希变化自动重建，三场景必须全绿
        3. 把新的 rustc 版本更新到 {BUILD_DOC}
      ★ M3 对拍期间**冻结**，不要在采集数据的中途换编译器（§8 要求环境可复现）。""")
    return True


def check_pebble() -> bool:
    """★ 門禁裡那個本機 CA（pebble，G64）有沒有落後。回傳 True = 需要人管。

    ★ **為什麼並進建置鏡像那一檔（40）而不是新開一檔**：它就釘在
    `docker/Dockerfile.build` 裡，與 rustc 同一個文件、同一種處置（改文件 → 重建鏡像 →
    重跑門禁）。★ 而更硬的理由是結束代碼只有 8 位：現有五檔相加已經是 310，
    再加一檔必然溢出（見 `finish()`）。**一個會被截斷的位，比沒有這個位更糟。**

    ⚠ 判據是「上游有沒有發新版」，不是「雜湊對不對」——雜湊對不對由 `docker build`
    自己在下載時驗（`sha256sum -c`），裝不上就當場失敗，不需要這裡再查一遍。
    """
    if not BUILD_DOCKERFILE.exists():
        print(f"★ 找不到 {rel(BUILD_DOCKERFILE)} —— pebble 无从查证。")
        return True

    text = BUILD_DOCKERFILE.read_text(encoding="utf-8")
    m = PEBBLE_PIN.search(text)
    if m is None:
        # ★ 這一條同時守著自己：有人把 pebble 那段刪了或改了寫法，本檢查就該紅，
        #   而不是安靜地什麼都不查（「沒能檢查」當成「檢查通過」是本專案栽過的形狀）。
        print(f"★ {rel(BUILD_DOCKERFILE)} 里读不出 `ARG PEBBLE_VERSION=` —— "
              f"pebble 那一段被改过写法？本次 pebble 没有结论。")
        return True
    pinned = m.group(1)

    # ★ 四個雜湊也要在。少一個架構的雜湊，那個架構上的門禁會在建置時才炸，
    #   而那時的回報錯誤離「誰把它刪了」已經很遠。
    missing = [k for k in PEBBLE_SHA_ARGS if f"ARG {k}=" not in text]
    if missing:
        print(f"★ {rel(BUILD_DOCKERFILE)} 缺这几个哈希：{', '.join(missing)} —— "
              f"按 sha256 钉死是 G64 的一半，缺了等于没钉。")
        return True

    url = f"https://api.github.com/repos/{PEBBLE_REPO}/releases/latest"
    req = urllib.request.Request(
        url, headers={"User-Agent": UA, "Accept": "application/vnd.github+json"}
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            latest = json.load(resp).get("tag_name")
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, json.JSONDecodeError) as e:
        print(f"★ 没能向 GitHub 查证 pebble 的最新 release：{e}")
        print("  ★ 这**不等于** pebble 是最新的——本次 pebble 那一栏没有结论，换网络重跑。")
        return True

    if not latest:
        print("★ GitHub 没给出 pebble 的 latest release tag —— 本次 pebble 那一栏没有结论。")
        return True

    if version_key(latest.lstrip("v")) <= version_key(pinned.lstrip("v")):
        print(f"pebble 已是最新（钉的是 {pinned}，上游最新 {latest}）。")
        return False

    print(f"★ pebble 上游已到 **{latest}**，而 {rel(BUILD_DOCKERFILE)} 钉的是 {pinned}。")
    print(f"""
    ⚠ 这一条 **不能自动采纳**：换 CA 要重跑 ACME 门禁。手工做：
      1. 到 https://github.com/{PEBBLE_REPO}/releases/tag/{latest} 取
         pebble / pebble-challtestsrv 的 **linux-amd64 与 linux-arm64 四个 sha256**
         ★ **四个一起换，别只换本机那个架构的**——另一个架构会在别人机器上才炸
      2. 改 {rel(BUILD_DOCKERFILE)} 里的 ARG（版本 + 四个哈希）
      3. ACME_ONLY=1 bash tests/m0/docker-run.sh   # 镜像按内容哈希自动重建，必须绿""")
    return True


def check_systemd_image() -> bool:
    """★ M1 的 systemd 測試宿主鏡像有沒有落後（G39）。回傳 True = 需要人管。

    判據是 **systemd 大版本**，與 G36 判 rustc 同構，理由卻不同：
    G36 那邊「Debian 包不進產物」所以只盯 rustc；這邊**被測物件本身就是 systemd**——
    M1 spike #1 的結論（alien MainPID 之下 `systemctl stop` 不再等排空）是對某個
    systemd 版本成立的，★ **版本是結論的一部分**。

    ★ 一條實測出來的邊界，值得寫在這裡：Debian stable 在一個 suite 內**凍結大版本**，
    所以「大版本變了」實際只會在換 suite（trixie → 下一個）時發生。這道門因此不是
    「每週都可能紅」的那種，而是**換基底時必然紅**的那種——它守的正是那一刻。
    """
    if not SYSTEMD_DOCKERFILE.exists():
        print(f"★ 找不到 {rel(SYSTEMD_DOCKERFILE)} —— systemd 测试宿主无从查证。")
        return True

    text = SYSTEMD_DOCKERFILE.read_text(encoding="utf-8")
    m = FROM_LINE.search(text)
    if m is None:
        print(f"★ {rel(SYSTEMD_DOCKERFILE)} 里没找到可解析的 FROM 行。")
        return True

    pinned_tag, pinned_digest = m.group("tag"), m.group("digest")
    # ★ 與 G36 那條一樣，這一句同時守著自己的修復：釘子被人拿掉了，本檢查就該紅。
    if not pinned_digest:
        print(f"★ 测试宿主镜像 `{m.group('repo')}:{pinned_tag}` **没有钉 digest**——"
              f"而 systemd 的版本是 M1 结论的一部分。见 {SYSTEMD_DOC}。")
        return True

    pin_m = PINNED_SYSTEMD.search(text)
    if pin_m is None:
        print(f"★ {rel(SYSTEMD_DOCKERFILE)} 里没有 `# fulcrum-pinned-systemd: <版本>` 这一行——"
              f"没有它就无从判断 systemd 是否漂了。")
        return True
    pinned_sd = pin_m.group("ver")
    pinned_major_m = SYSTEMD_MAJOR.match(pinned_sd)
    if pinned_major_m is None:
        print(f"★ 钉定的 systemd 版本 `{pinned_sd}` 里读不出大版本号。")
        return True
    pinned_major = pinned_major_m.group(1)

    # `trixie-slim` → suite 名 `trixie`
    suite = pinned_tag.split("-", 1)[0]

    try:
        tags = hub_tag_digests(SYSTEMD_HUB_REPO, suite)
        live_sd = debian_suite_systemd(suite)
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, json.JSONDecodeError) as e:
        # ★ 查不到 ≠ 沒落後。**「沒能檢查」不許當成「檢查通過」**（本專案已抓到過五種面目）。
        print(f"★ 没能查证 systemd 测试宿主镜像：{e}")
        print("  ★ 这**不等于**它是最新的——本次这一栏没有结论，换网络重跑。")
        return True

    if live_sd is None:
        print(f"★ Debian 源码 API 里查不到 suite `{suite}` 的 systemd 版本——suite 改名了？")
        return True
    live_major_m = SYSTEMD_MAJOR.match(live_sd)
    if live_major_m is None:
        print(f"★ 线上的 systemd 版本 `{live_sd}` 里读不出大版本号。")
        return True
    live_major = live_major_m.group(1)

    live_digest = tags.get(pinned_tag)
    if live_digest is None:
        print(f"★ Docker Hub 上没查到 `{SYSTEMD_HUB_REPO}:{pinned_tag}`——tag 改名了？")
        return True

    if live_major != pinned_major:
        print(f"★ Debian `{suite}` 的 systemd 已到 **{live_sd}**（大版本 {live_major}），"
              f"而 {rel(SYSTEMD_DOCKERFILE)} 钉的是 {pinned_sd}（大版本 {pinned_major}）。")
        print(f"""
    ⚠ 这一条 **不能自动采纳**（同 G36）—— 换 systemd 必须重跑 M1 的三个场景，不该无人值守。
      ★ 换大版本时**尤其**要看 `tests/m1/mainpid-handover.sh`：它钉住的正是
        「alien MainPID 之下停机不等排空」这条行为，而那是**随 systemd 版本而变**的。
        它若变绿，说明 systemd 改了行为，D14 的取舍要重新评估，而不是把断言改掉。
      步骤：
        1. docker pull {SYSTEMD_HUB_REPO.split('/')[-1]}:{pinned_tag}
        2. 把 FROM 的 digest 与 `# fulcrum-pinned-systemd:` 一起换成新值
        3. bash tests/m1/systemd-run.sh   # 镜像会因内容哈希变化自动重建，三场景必须全绿
        4. 把新的 systemd 版本更新到 {SYSTEMD_DOC}""")
        return True

    if live_digest != pinned_digest or live_sd != pinned_sd:
        # 同一個大版本，只是 Debian 側重打了包或出了小版本。按 G36 的口徑只提示。
        print(f"systemd 测试宿主的大版本仍是 {pinned_major}，无需拔钉子。")
        if live_sd != pinned_sd:
            print(f"  （suite `{suite}` 的 systemd 已是 {live_sd}，钉的是 {pinned_sd}；"
                  f"同一大版本，按 G39 只提示不判红）")
        if live_digest != pinned_digest:
            print(f"  （`{pinned_tag}` 的 digest 已重打为 {live_digest[:19]}…）")
        return False

    print(f"systemd 测试宿主已是 `{pinned_tag}` 当前指向的那一个（systemd {pinned_sd}），无需拔钉子。")
    return False

# ── 安全公告（第五項）───────────────────────────────────────────
# 要掃的鎖。★ 順序有意義：vendor 那把是**產品的真實相依**，放前面。
ADVISORY_LOCKS = (VENDOR_LOCK, "Cargo.lock")


def _load_supply_audit():
    """按路徑載入 `supply-audit.py`（名字帶連字元，不能直接 import）。

    ★ 只為一件事：**重複使用它的 `ACCEPTED` 與 `osv_batch`，而不是複製一份**。
      本儲存庫已經吃過「同一份清單兩處副本」的虧（`tests/vendor/run.sh` 那張 16 項
      手寫 BUMPED 名單，是 FORK.md 上界表的人工鏡像，才拆掉）。
      兩份豁免名單分家的表現是：這邊判紅、那邊判綠，而兩邊都說自己是權威。
    """
    import importlib.util

    path = REPO / "tools" / "supply-audit.py"
    spec = importlib.util.spec_from_file_location("fulcrum_supply_audit", path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"没能加载 {rel(path)}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def check_advisories() -> bool:
    """第五項：對**兩把鎖**掃安全公告。回傳 True 表示要人管。

    ★ 只做公告這一半：OSV 是批量介面（`querybatch`），幾個請求就夠。
      `supply-audit.py` 慢的是逐包查 crates.io 陳舊度那半，那半仍留給手動深審。
    """
    sa = _load_supply_audit()

    pkgs: dict[tuple[str, str], list[str]] = {}
    for rel_lock in ADVISORY_LOCKS:
        p = REPO / rel_lock
        if not p.exists():
            print(f"  ★ 找不到 {rel_lock} —— 判红，因为「没能查证」不等于「没问题」")
            return True
        for nv in sa.parse_lock(p):
            pkgs.setdefault(nv, []).append(rel_lock)
    print(f"两把锁去重后共 {len(pkgs)} 个 registry 包（{' + '.join(ADVISORY_LOCKS)}）")

    hits, unqueried = sa.osv_batch(sorted(pkgs))

    if unqueried:
        print(f"  ★ 有 {len(unqueried)} 个包**本次没查成**——判红（「没查成」不是「干净」）：")
        for (n, v), why in unqueried[:5]:
            print(f"      {n} {v}  —— {why}")
        if len(unqueried) > 5:
            print(f"      …… 另有 {len(unqueried) - 5} 个")
        return True

    unregistered: list[tuple[str, str, str, list[str]]] = []
    registered = 0
    seen_accepted: set[str] = set()
    for (n, v), ids in sorted(hits.items()):
        for aid in ids:
            if aid in sa.ACCEPTED:
                registered += 1
                seen_accepted.add(aid)
                continue
            unregistered.append((n, v, aid, pkgs[(n, v)]))

    for n, v, aid, locks in unregistered:
        print(f"  ★ 未登记：{n} {v}  {aid}   （出现在 {', '.join(locks)}）")
    if registered:
        print(f"  ○ 已登记并接受 {registered} 条（理由见 supply-audit.py 的 ACCEPTED）")

    # ★ 反向檢核：ACCEPTED 裡有條目在兩把鎖上都不再命中 → 它該被刪掉，否則名單會爛。
    #   ⚠ 只在**確實查成了**的時候才做——`unqueried` 非空時上面已經 return，所以這裡安全。
    dead = sorted(set(sa.ACCEPTED) - seen_accepted)
    if dead:
        print(f"  ★ ACCEPTED 里有 {len(dead)} 条本次没有命中任何包，应当删掉：{', '.join(dead)}")
        return True

    if unregistered:
        print(f"  → {len(unregistered)} 条未登记：要么修，要么写进 supply-audit.py 的 ACCEPTED 并说明理由。")
        return True

    print("  没有未登记的安全公告。")
    return False


EXIT_BITS: tuple[tuple[int, str], ...] = (
    (10, "有可采纳的 cargo 更新"),
    (20, "fork 上游要人管"),
    (40, "构建镜像要人管（rustc / pebble）"),
    (80, "systemd 测试宿主镜像要人管"),
    (160, "有未登记的安全公告"),
)


def finish(code: int) -> int:
    """把疊加出來的結束代碼交出去；**溢出時改成 255 並把真相打出來**。

    ★ ★ POSIX 的結束代碼只有 8 位。五檔全亮是 10+20+40+80+160 = **310**，
    而呼叫方看到的是 `310 & 0xFF` = **54**——按本指令碼自己的編碼解出來是
    「10 + 40 + 4」，其中 4 根本不是一檔。也就是說**最嚴重的那次報告，
    解碼出來是一個不存在的組合**，而且看起來比實際輕。
    （`20+80+160` = 260 → **4** 更糟：三檔全亮，讀出來像「幾乎什麼都沒亮」。）

    ⚠ 這不是理論：這幾檔是一檔一檔加上來的，加到第五檔時就已經越界了，
    而沒有任何東西會說出來——**點陣圖式的結束代碼天生缺一個「裝不下了」的表示**。
    處置：超出就退 255（慣例上的「說不清楚」），並把**亮著的那幾檔逐條列出來**。
    人讀得懂的清單，比一個被截斷的點陣圖有用。
    """
    if code <= 255:
        return code
    # ⚠ ⚠ **這幾檔不是二進位位**，是十進位的 10 / 20 / 40 / 80 / 160。
    #   ⚠ 寫成 `code & bit == bit` 是錯的 —— 文件裡雖然管它們叫「一位」，
    #   我就當真按位與去解了。結果 310 解出來**只有「fork 上游要人管」一條**：
    #   一份讀起來很像結論的錯報告，而且比真相輕得多。
    #   ★ 它們都是 10 的倍數、逐檔翻倍，所以先除以 10 才是真正的點陣圖。
    #   （這條與本專案反覆記的是同一件事：**判據要照定義寫，不照它長得像什麼寫**。）
    mask = code // 10
    lit = [label for bit, label in EXIT_BITS if mask & (bit // 10)]
    print()
    print(f"★ ★ 退出码 {code} 超出 8 位，直接报会被截断成 {code & 0xFF}（那是个不存在的组合）。")
    print("  本次改用 255。真正亮着的是：")
    for label in lit:
        print(f"    · {label}")
    return 255


def main() -> int:
    ap = argparse.ArgumentParser(description="Fulcrum 依赖检查（G29：追新 + 安全怀疑期）")
    ap.add_argument("--hours", type=int, default=24, help="安全怀疑期小时数（默认 24）")
    ap.add_argument("--apply", action="store_true", help="采纳通过怀疑期的更新")
    ap.add_argument(
        "--cargo",
        default="cargo",
        help="cargo 命令；在容器里跑时可传 'docker run ... cargo' 之类的完整前缀（用空格分隔）",
    )
    ap.add_argument("--skip-fork", action="store_true", help="跳过 fork 上游检查")
    ap.add_argument("--skip-image", action="store_true", help="跳过构建镜像检查（G36）")
    ap.add_argument(
        "--skip-systemd-image", action="store_true", help="跳过 systemd 测试宿主镜像检查（G39）"
    )
    ap.add_argument(
        "--skip-advisories", action="store_true", help="跳过安全公告检查（两把锁）"
    )
    ap.add_argument(
        "--self-check", action="store_true", help="只跑内置自测（不出网、不跑 cargo），门禁 lint 那一格调它"
    )
    args = ap.parse_args()
    if args.self_check:
        return self_check()

    cargo = args.cargo.split()
    cutoff = datetime.now(timezone.utc) - timedelta(hours=args.hours)

    # ── fork 的上游先查。它與 cargo 那條路完全獨立：patch 過去的 crate
    #    不在 `cargo update` 的視野裡，而它恰恰是本專案最重要的那個相依。
    fork_behind = False
    if not args.skip_fork:
        print("── fork 上游检查 ──")
        fork_behind = check_fork_upstream(cutoff)
        print()
    fork_code = 20 if fork_behind else 0

    # ── 建置鏡像（G36 / D13）。同樣在 `cargo update` 的視野之外。
    image_behind = False
    if not args.skip_image:
        print("── 构建镜像检查 ──")
        image_behind = check_build_image()
        # ★ pebble 並進同一檔（40）。⚠ 兩個都要跑完再取或，**不能寫成
        #   `check_build_image() or check_pebble()`**：`or` 短路，rustc 一紅
        #   pebble 那條就整輪不查了——而它恰恰是「沒能檢查算作檢查通過」的近親。
        print()
        print("── 门禁本地 CA（pebble，G64）检查 ──")
        pebble_behind = check_pebble()
        image_behind = image_behind or pebble_behind
        print()
    image_code = 40 if image_behind else 0

    # ── systemd 測試宿主鏡像（G39）。同樣在 `cargo update` 的視野之外，
    #    而且它比建置鏡像更需要盯著：**systemd 的版本是 M1 結論的一部分**。
    systemd_behind = False
    if not args.skip_systemd_image:
        print("── systemd 测试宿主镜像检查 ──")
        systemd_behind = check_systemd_image()
        print()
    systemd_code = 80 if systemd_behind else 0

    # ── 安全公告。★ 它與上面三條一樣在 `cargo update` 的視野之外：
    #    `cargo update` 只管「有沒有新版」，管不了「現有版本有沒有公告」。
    advisories_bad = False
    if not args.skip_advisories:
        print("── 安全公告检查（两把锁）──")
        advisories_bad = check_advisories()
        print()
    advisory_code = 160 if advisories_bad else 0

    print("── crates.io 更新检查 ──")
    candidates = cargo_candidates(cargo)
    # ★ vendor 清單再問一次：只在 vendor 鎖裡的版本根那條路看不見，見 [`vendor_only`]。
    vendor_candidates = vendor_only(cargo_candidates(cargo, VENDOR_MANIFEST), _lock_versions("Cargo.lock"))
    if not candidates and not vendor_candidates:
        print("没有可用更新，依赖已是最新。")
        print("★ 注意：这句话只覆盖 `cargo update` 能动的东西——"
              "被上游清单上界卡住的依赖它一个都不会报。全量审计见 docs/platform/supply-chain.md。")
        return finish(fork_code + image_code + systemd_code + advisory_code)

    # ★ ★ 第二格是**當前版本**，而它是補上的，理由是一次真的失敗：
    #   `syn` 在圖裡有兩個大版本（2.0.119 與 3.0.3），於是
    #   `cargo update -p syn --precise 3.0.4` 報 `specification 'syn' is ambiguous`，
    #   ⚠ 而**另外八項全成功了** —— 一次「大部分綠」的採納，最容易被讀成全綠。
    #   ⇒ 用 `name@当前版本` 這個 pkgid spec 去指名道姓（cargo 自己在那條回報錯誤裡
    #     給出的正是這種寫法）。
    print(f"候选更新 {len(candidates)} 项，怀疑期 {args.hours} 小时（早于 {cutoff:%Y-%m-%d %H:%M UTC} 发布的才算数）\n")

    # 判過的版本記下來：採納之後的闖入者核對多半撞上候選清單裡被擋的那幾個，不必再問 crates.io。
    verdicts: dict[tuple[str, str], tuple[str | None, str]] = {}

    def vet(name: str, version: str) -> tuple[str | None, str]:
        if (name, version) not in verdicts:
            verdicts[(name, version)] = quarantine_verdict(name, version, cutoff)
        return verdicts[(name, version)]

    eligible = screen(candidates, vet)
    print(f"\n── vendor 独有的更新（{VENDOR_LOCK}；根清单看不见）──")
    print(f"候选 {len(vendor_candidates)} 项，同一个怀疑期\n")
    vendor_eligible = screen(vendor_candidates, vet)

    if not eligible and not vendor_eligible:
        print("\n没有通过怀疑期的更新，本次什么都不做。")
        return finish(fork_code + image_code + systemd_code + advisory_code)

    if not args.apply:
        print(f"\n{len(eligible)} + {len(vendor_eligible)}（vendor 独有）项可采纳。加 --apply 执行。")
        return finish(10 + fork_code + image_code + systemd_code + advisory_code)

    def update_root(spec: str, version: str) -> tuple[int, str]:
        proc = run([*cargo, "update", "-p", spec, "--precise", version])
        return proc.returncode, proc.stderr or proc.stdout

    def update_vendor(spec: str, version: str) -> tuple[int, str]:
        proc = run([*cargo, "update", "-p", spec, "--precise", version, "--manifest-path", VENDOR_MANIFEST])
        return proc.returncode, proc.stderr or proc.stdout

    def group(read_lock, lock: str, manifest: list[str]):
        """`adopt_root` 的 `update_group`：一次 `cargo update -p a -p b …`（不帶 `--precise`）。
        ★ `blaming` 在**呼叫時**才包：它記的是包上那一刻之後新出現的版本，早包會把逐項那一輪的也算到這一次頭上。"""
        def run_group(specs: list[str]) -> tuple[int, str]:
            def update(spec_line: str, _version: str) -> tuple[int, str]:
                proc = run([*cargo, "update", *(a for s in spec_line.split() for a in ("-p", s)), *manifest])
                return proc.returncode, proc.stderr or proc.stdout
            return blaming(update, read_lock, lock, blame)(" ".join(specs), "各自最新的兼容版（一起采纳）")
        return run_group

    # ★ 採納前給兩把鎖拍快照（逐位元組）：順帶帶進沒過懷疑期的版本時整批還原，見 [`lock_intruders`]。
    locks = ("Cargo.lock", VENDOR_LOCK)
    snapshot = {lock: (REPO / lock).read_bytes() for lock in locks}
    before = {lock: _lock_versions(lock) for lock in locks}
    blame: dict[tuple[str, str, str], str] = {}

    def read_root() -> dict[str, set[str]]:
        return _lock_versions("Cargo.lock")

    def read_vendor() -> dict[str, set[str]]:
        return _lock_versions(VENDOR_LOCK)

    failed = 0
    if eligible:
        print("\n开始采纳：")
        failed += len(adopt_root(eligible, blaming(update_root, read_root, "Cargo.lock", blame), read_root,
                                 update_group=group(read_root, "Cargo.lock", [])))
        failed += _align_vendor_lock(cargo, eligible, blame)

    if vendor_eligible:
        # ★ 根那一批之後再做：它們的目標裡不該有根那一批會鏡像過來的版本（[`vendor_only`] 已排除）。
        print(f"\n采纳 vendor 独有的（{VENDOR_LOCK}）：")
        prior = read_vendor()
        failed += len(adopt_root(vendor_eligible, blaming(update_vendor, read_vendor, VENDOR_LOCK, blame),
                                 read_vendor, label=f"vendor 锁（{VENDOR_LOCK}）",
                                 update_group=group(read_vendor, VENDOR_LOCK, ["--manifest-path", VENDOR_MANIFEST])))
        # ⚠ vendor 獨有的一項也可能把**兩把鎖共有**的包拉走（新版本要更新的相依）⇒ 動過的名字都再對一次齊。
        moved = sorted(n for n, vs in read_vendor().items() if vs != prior.get(n, set()))
        moved += sorted(set(prior) - set(read_vendor()))
        failed += _report_alignment(cargo, moved)

    found = lock_intruders(before, {lock: _lock_versions(lock) for lock in locks},
                           eligible + vendor_eligible, vet)
    if report_intruders(found, blame):
        for lock, data in snapshot.items():
            (REPO / lock).write_bytes(data)
        print("\n⛔ 整批作废（G29）：采纳会顺带带进没过怀疑期的版本。"
              "两把锁已逐字节还原成采纳前的样子，本轮什么都没采。")
        print("  等上面 ✗ 的那几个过了怀疑期再重跑；已 yank／查不到的要人看。")
        return 1

    print(
        "\n★ 记住 G29 的后半句：升级之后必须跑完当前里程碑的全部验证"
        "（M0 是 tests/m0/run.sh；M3 之后是全量对拍）。"
    )
    return 1 if failed else finish(10 + fork_code + image_code + systemd_code + advisory_code)


def adopt_root(eligible, update, read_lock, label: str = "根锁（Cargo.lock）",
               update_group=None) -> list[tuple[str, str]]:
    """根鎖那一側的採納：逐項 `cargo update -p 名字@旧版本 --precise 新版本`，**按結果鎖判成敗**。

    回傳真失敗的那幾項 `[(名字, 说明)]`，空表 = 根鎖全部到位。`update(spec, 版本)` 回傳
    `(退出码, 输出)`，`read_lock()` 回傳 `{名字: {版本}}` —— 都由呼叫方給，自測裡換成樁。
    vendor 獨有的那一批（[`vendor_only`]）走同一個函式，`update` / `read_lock` 換成 vendor 清單與鎖、`label` 跟著換。

    # ⚠ 結束代碼不能當判據（與 `_align_vendor_lock` 同一條）

    同族包會被前一項**順帶帶到目標版本**：`boring-sys` 跟 `boring`、`jiff-static` 跟 `jiff`、
    `zerocopy-derive` 跟 `zerocopy`、`zstd-safe` / `zstd-sys` 跟 `zstd`。輪到它自己時
    `名字@旧版本` 已經不在鎖裡，cargo 報 `did not match any packages` 並非零退出。
    ⇒ 2026-10-16 那次 37 項裡 9 項是這個形狀，`--apply` 因此退 1，而鎖一項不差 ——
      一個綠的結果被報成紅，比漏報更傷：讀的人要麼誤判，要麼學會無視紅。
    ★ 判據取**結果**：全部跑完之後讀根鎖，見 [`root_lock_problems`]。反過來也成立：
      cargo 退 0 而鎖不對（後一項把前一項挪走、舊版本仍被別的包釘著）同樣判失敗。

    # ★ 互相 `=` 釘死的一族：逐項永遠升不動，一起升（`update_group`）

    `wasm-bindgen` 0.2.127 / `-macro` / `-macro-support` / `-shared` / `-futures` 與 `js-sys` / `web-sys` 彼此要 `=` 版本
    ⇒ 任何一項單獨 `--precise` 都撞上另一項的 `=` 釘子（cargo 1.98.0：`failed to select a version for the
    requirement js-sys = "=0.3.104"`），2026-10-16 vendor 獨有的那批 25 項裡 7 項是這個形狀，逐項採納永遠紅。
    ⇒ 給了 `update_group(specs)` 時，逐項跑完還沒到位、舊版本還在鎖裡的那幾項**一起**交給它
    （`cargo update -p a@旧 -p b@旧 …`，不帶 `--precise`：一次解開一族，落到各自最新的相容版，實測正是 dry-run 的目標），
    然後**照舊按結果鎖判**。⚠ 不帶 `--precise` 可能落到候選清單之後才發的版本 ⇒ 那不是目標，這裡判沒到位，
    採納後的懷疑期核對（[`lock_intruders`]）也會現查它 —— 判據不放鬆。
    """
    for name, cur, version in eligible:
        # ⚠ 同名多版本時裸 `name` 是**歧義的**，而 cargo 會因此整條失敗。
        #   帶上當前版本就唯一了；`cur` 為空（新增包）時只能裸著來。
        spec = f"{name}@{cur}" if cur else name
        rc, out = update(spec, version)
        if rc == 0:
            print(f"  ✓ {spec} -> {version}")
        else:
            print(f"  ? {spec} -> {version}    （cargo 退出码 {rc}；成败以下面的{label}核对为准）")
            print("\n".join(f"      {line}" for line in out.strip().splitlines()))

    problems = root_lock_problems(read_lock(), eligible)
    if problems and update_group:
        lock = read_lock()
        stuck = {name for name, _line in problems}
        specs = [f"{name}@{cur}" for name, cur, _new in eligible
                 if name in stuck and cur and cur in lock.get(name, set())]
        if specs:
            print(f"\n  逐项没到位的 {len(specs)} 项一起升（互相 `=` 钉死的一族逐项永远升不动）：{' '.join(specs)}")
            rc, out = update_group(specs)
            if rc:
                print(f"  ? cargo 退出码 {rc}；成败以下面的{label}核对为准")
                print("\n".join(f"      {line}" for line in out.strip().splitlines()))
            problems = root_lock_problems(read_lock(), eligible)
    print(f"\n核对{label}：")
    for _name, line in problems:
        print(f"  ✗ {line}")
    if problems:
        print(f"  ⚠ {len(problems)} / {len(eligible)} 项没落到目标版本。")
    else:
        print(f"  ✓ {len(eligible)} 项全在目标版本，旧版本一个不剩。")
    return problems


def root_lock_problems(lock: dict[str, set[str]], eligible) -> list[tuple[str, str]]:
    """逐項核對根鎖：目標版本在、舊版本不在。回傳沒做到的那幾項 `[(名字, 说明)]`。

    ★ 同一個名字在圖裡可以有好幾個版本，每一項只對自己那一對（舊 → 新）負責：
      舊版本恰好是同名另一項的目標時（一個相依方 0.3 → 0.4、另一個 0.4 → 0.5），它留著是對的。
      新增包（舊版本為空）只看目標在不在。
    """
    targets: dict[str, set[str]] = {}
    for name, _cur, new in eligible:
        targets.setdefault(name, set()).add(new)

    out = []
    for name, cur, new in eligible:
        have = lock.get(name, set())
        why = []
        if new not in have:
            why.append(f"目标 {new} 不在锁里")
        if cur and cur in have and cur not in targets[name]:
            why.append(f"旧版本 {cur} 还在")
        if why:
            out.append((name, f"{name} {cur or '(新增)'} -> {new}：{'，'.join(why)}"
                              f"（锁里是：{' '.join(sorted(have)) or '没有这个包'}）"))
    return out


def lock_intruders(before, after, eligible, vet) -> list[tuple[str, str, str, str | None, str]]:
    """採納前後逐把鎖比：新出現、卻**不是任何一項採納目標**的 `(名字, 版本)`，各自過一遍懷疑期。

    回傳 `[(锁, 名字, 版本, 原因 or None, 说明)]`，原因非空的就是闖入者 —— G29 被繞過了。
    `before` / `after` 是 `{锁: {名字: {版本}}}`；`vet(名字, 版本)` 同 [`quarantine_verdict`]。

    # ★ ★ 為什麼要有這一步

    `cargo update -p A@旧 --precise 新` 是保守更新，但 A 的新版本要求的相依**鎖裡滿足不了時照樣會動**，
    而且被拉到「滿足要求的最新版」，**不看發佈時間**。cargo 1.98.0 實測：`zstd` 0.12.4 → 0.13.0
    把 `zstd-safe` 6.0.6 拉到 7.3.0，而 `^7.0.0` 有 7.0.0 就夠了。
    ⇒ 同族包的最新版還在懷疑期裡（候選清單裡它正被擋著），或 A 的新版本新增了一個剛發版的相依，
      `--apply` 都會把它**悄悄**採納 —— [`root_lock_problems`] 只核「每一項到沒到目標」，不核「別的動沒動」。

    ★ 採納目標在候選清單裡已過過懷疑期，不再查；其餘新出現的版本**一律現查**，不只認候選清單裡被擋的：
      cargo 每跑一次都會刷新索引，候選清單之後才發的版本同樣可能被帶進來；而被帶到候選清單之外、
      早已過了懷疑期的老版本（上面的 7.3.0 就是這種）也不該判紅。
    ★ 消失的版本（新版本甩掉的相依）不在此列：移走東西不是採納。
    """
    targets = {(name, new) for name, _cur, new in eligible}
    out = []
    for lock, now in after.items():
        old = before.get(lock, {})
        for name in sorted(now):
            for ver in sorted(now[name] - old.get(name, set())):
                if (name, ver) not in targets:
                    out.append((lock, name, ver, *vet(name, ver)))
    return out


def report_intruders(found, blame) -> int:
    """打出 [`lock_intruders`] 的結果，回傳闖入者個數（> 0 即整批作廢）。"""
    print("\n核对怀疑期（两把锁采纳前后逐包比，只看不是采纳目标的新版本）：")
    if not found:
        print("  ✓ 新出现的版本全是采纳目标。")
        return 0
    bad = 0
    for lock, name, ver, why, detail in found:
        by = blame.get((lock, name, ver), "说不清是哪一项")
        if why:
            bad += 1
            print(f"  ✗ {name} {ver}（{lock}，由 {by} 带进来）[{why}] {detail}")
        else:
            print(f"  ○ {name} {ver}（{lock}，由 {by} 带进来）不在候选清单里，已过怀疑期：{detail}")
    return bad


def blaming(update, read_lock, lock: str, blame: dict):
    """給 `update(spec, 版本)` 包一層：每跑完一項讀一次鎖，新出現的 `(锁, 名字, 版本)` 記進 `blame`，
    值是帶它進來的那一項。只為報告 —— 闖入者要說得出是誰帶進來的，人才知道在等哪一個。
    """
    prev = read_lock()

    def wrapped(spec, version):
        nonlocal prev
        result = update(spec, version)
        now = read_lock()
        for name, vers in now.items():
            for ver in vers - prev.get(name, set()):
                blame[(lock, name, ver)] = f"{spec} -> {version}"
        prev = now
        return result

    return wrapped


def _lock_versions(rel_lock: str) -> dict[str, set[str]]:
    """某把鎖裡每個 registry 包 → 它解出來的版本集合。"""
    out: dict[str, set[str]] = {}
    for n, v in _load_supply_audit().parse_lock(REPO / rel_lock):
        out.setdefault(n, set()).add(v)
    return out


def _align_vendor_lock(cargo: list[str], eligible: list[tuple[str, str | None, str]], blame: dict) -> int:
    """把同一批更新也落進 **vendor/pingora/Cargo.lock**，並**逐個包核對兩把鎖一致**。

    # ★ ★ ★ 為什麼要有這一步（§10）

    本儲存庫有**兩把鎖**，而 [`tests/vendor/run.sh`] 第 [2/5] 步要求它們對同一個包
    解出同一組版本，否則**拒絕跑回歸網** —— 理由是「vendor 測試跑的不是產物裡那套組合，
    結果不可採信」。⚠ 而本函式出現之前，`--apply` **只更新根那一把**。

    ⇒ 那 9 項更新完成實作之後，迴歸網當場紅，並逐條列出 **8 個**對不上的包。
    ★ ★ 諷刺的是本指令碼**早就知道有兩把鎖** —— 安全公告那一節讀的正是兩把
    （`ADVISORY_LOCKS`）。**「知道有兩把」與「兩把都更新」是兩件事。**

    # ⚠ 結束代碼不能當判據

    對 vendor 清單跑 `cargo update` 有**兩種良性失敗**，都會讓它非零：

    · 那個包根本不在 vendor 鎖裡（實測：`combine`）；
    · 它已被同族的另一條更新順帶帶過去了（實測：`ref-cast-impl` 跟著 `ref-cast` 走）。

    ⇒ 判據取的是**結果**不是**過程**：兩把鎖對這個名字解出來的版本集合一不一致。
    ★ 而「vendor 鎖里根本沒有這個名字」算一致 —— 它沒有可對不上的東西。
    """
    print(f"\n对齐第二把锁（{VENDOR_LOCK}）：")
    update = blaming(
        lambda spec, version: run([*cargo, "update", "-p", spec, "--precise", version,
                                   "--manifest-path", VENDOR_MANIFEST]),
        lambda: _lock_versions(VENDOR_LOCK), VENDOR_LOCK, blame,
    )
    for name, cur, version in eligible:
        update(f"{name}@{cur}" if cur else name, version)

    return _report_alignment(cargo, [n for n, _c, _v in eligible])


def _report_alignment(cargo: list[str], names: list[str]) -> int:
    """讀兩把鎖，對 `names` 逐個跑 [`lock_alignment`] 並打出來，回傳對不上的個數。"""
    root = _lock_versions("Cargo.lock")
    vendor = _lock_versions(VENDOR_LOCK)

    marks = {"ok": "✓", "absent": "○", "vendor-only": "○", "root-only-path": "·", "mismatch": "✗"}
    bad = 0
    for name, verdict, why in lock_alignment(root, vendor, names, lambda n, v: _pingora_users(cargo, n, v)):
        print(f"  {marks[verdict]} {name:<20} {why}")
        bad += verdict == "mismatch"
    if bad:
        print(f"  ⚠ {bad} 个包两把锁对不上 —— tests/vendor/run.sh 会拒绝跑回归网。")
    return bad


def _pingora_users(cargo: list[str], name: str, version: str) -> list[str] | None:
    """產物裡有哪些 pingora crate 用到 `name@version`；問不出來回傳 None。

    ★ 與 `tests/vendor/run.sh` [2/5] 的 `who_uses` 問的是同一句：根 workspace 的反向樹，
      `-e normal,build`（產物裡的圖）、`--target all`（寧可多算路徑）。
    """
    proc = run([*cargo, "tree", "--locked", "-i", f"{name}@{version}",
                "-e", "normal,build", "--target", "all", "--prefix", "none"])
    if proc.returncode != 0:
        return None
    return sorted({m.group(0) for line in proc.stdout.splitlines()
                   if (m := re.match(r"pingora-[a-z-]+", line))})


def lock_alignment(root, vendor, names, pingora_users):
    """逐個包判定兩把鎖對不對得上，回傳 [(名字, 判定, 說明)] —— 與 [2/5] 同一口徑。

    判定 ∈ "ok" / "absent"（vendor 鎖裡沒有這個名字）/ "vendor-only"（根鎖裡沒有這個名字）/
    "root-only-path"（根側多出的版本沒有任何 pingora crate 在用）/ "mismatch"。`pingora_users(名字, 版本)`
    回傳用到它的 pingora crate 列表，問不出來回傳 None（⛔ 問不出來 ≠ 沒人用，一律判 mismatch）。
    ★ vendor 側**多**出來的版本合法（dev-dep／未啟用的可選相依拖進來的老大版本），不判。
    """
    out = []
    for name in names:
        rv, vv = root.get(name, set()), vendor.get(name)
        if vv is None:
            out.append((name, "absent", "vendor 锁里没有这个包 —— 无从对不上"))
            continue
        if not rv:
            out.append((name, "vendor-only", f"根锁里没有这个包 —— 只在 vendor 锁里：{' '.join(sorted(vv))}"))
            continue
        missing = sorted(rv - vv)
        if not missing:
            extra = "（vendor 另有旧版本，不进产物）" if vv - rv else ""
            out.append((name, "ok", f"根侧的版本 vendor 侧都有：{' '.join(sorted(rv))}{extra}"))
            continue
        problems, root_only = [], []
        for ver in missing:
            users = pingora_users(name, ver)
            if users is None:
                problems.append(f"{ver} 在 vendor 侧缺席，且问不出产物里谁在用它（cargo tree 失败）")
            elif users:
                problems.append(f"{ver} 在 vendor 侧缺席，而 {' '.join(users)} 在用它")
            else:
                root_only.append(ver)
        if problems:
            out.append((name, "mismatch", "；".join(problems) + f"（vendor 有：{' '.join(sorted(vv))}）"))
        else:
            out.append((name, "root-only-path",
                        f"根侧的 {' '.join(root_only)} 没有任何 pingora crate 在用 —— vendor 测不到也不必测"))
    return out


def _self_check_adopt_root() -> tuple[list[str], int]:
    """釘住 [`adopt_root`]：根鎖那一側的成敗看**結果鎖**，不看 cargo 的結束代碼。兩個方向都要有。

    每條：(說明, 候選, 非零退出的 spec → 結束代碼, 採納後的根鎖, 應判失敗的名字)。版本號是合成的。
    """
    cases = [
        ("同族已被前一项带到位（2026-10-16 的形状）：cargo 退非零，锁是对的",
         [("boring", "5.2.0", "5.3.0"), ("boring-sys", "5.2.0", "5.3.0"),
          ("zstd", "0.14.0", "0.14.1"), ("zstd-safe", "8.0.0", "8.0.1"), ("zstd-sys", "2.1.0", "2.1.1")],
         {"boring-sys@5.2.0": 101, "zstd-safe@8.0.0": 101, "zstd-sys@2.1.0": 101},
         {"boring": {"5.3.0"}, "boring-sys": {"5.3.0"},
          "zstd": {"0.14.1"}, "zstd-safe": {"8.0.1"}, "zstd-sys": {"2.1.1"}},
         set()),
        ("同名两个大版本各升一格 · 一个依赖方的目标正是另一个的旧版本 · 新增包轮到时还不在锁里",
         [("syn", "2.0.119", "2.0.120"), ("syn", "3.0.3", "3.0.4"),
          ("foo", "0.3.0", "0.4.0"), ("foo", "0.4.0", "0.5.0"), ("aaa-new", None, "0.1.0")],
         {"aaa-new": 101},
         {"syn": {"2.0.120", "3.0.4"}, "foo": {"0.4.0", "0.5.0"}, "aaa-new": {"0.1.0"}},
         set()),
        ("真失败：cargo 退非零，锁也没动",
         [("rand", "0.10.3", "0.10.4"), ("ring", "0.17.14", "0.17.15")],
         {"ring@0.17.14": 101},
         {"rand": {"0.10.4"}, "ring": {"0.17.14"}},
         {"ring"}),
        ("cargo 退 0 而锁不对：旧版本仍被别的包钉着 · 目标被后一项挪走 · 新增包没加上",
         [("hyper", "1.11.0", "1.12.0"), ("h2", "0.4.19", "0.4.20"), ("bbb-new", None, "1.0.0")],
         {},
         {"hyper": {"1.11.0", "1.12.0"}, "h2": {"0.4.18"}},
         {"hyper", "h2", "bbb-new"}),
    ]
    bad = []
    for label, eligible, nonzero, lock, want in cases:
        def update(spec, _version, nonzero=nonzero):
            rc = nonzero.get(spec, 0)
            return rc, f"error: package ID specification `{spec}` did not match any packages" if rc else ""

        with contextlib.redirect_stdout(io.StringIO()):
            got = {n for n, _why in adopt_root(eligible, update, lambda lock=lock: lock)}
        if got != want:
            bad.append(f"根锁采纳「{label}」：应判失败 {sorted(want) or '无'}，实际判了 {sorted(got) or '无'}")
    return bad, len(cases)


def _self_check_intruders() -> tuple[list[str], int]:
    """釘住 [`lock_intruders`] 與 [`blaming`]：順帶帶進來的版本逐個過懷疑期，判紅的要點名是哪一項帶進來的。

    每條：(說明, 候選, 採納前的兩把鎖, 每一項在每把鎖裡的改動, 懷疑期判定, 應判紅 → 帶進來的那一項,
    應放行的)。改動是 `(名字, 移走的版本 or None, 加进的版本 or None)`，由 `update` 樁寫進假鎖，
    逐項走一遍 `blaming` 再比前後 —— 與 `--apply` 同一條路。版本號與判定都是合成的。
    ★ 判定表之外的版本被拿去查，同樣算失敗：採納目標和沒動的版本不該再問 crates.io。
    """
    root, vendor = "Cargo.lock", VENDOR_LOCK
    cases = [
        ("同族被拉到候选清单里正被挡着的版本 · 新增依赖刚发版 · 同族到目标版本、前一项已带到位的不算",
         [("zstd", "0.12.4", "0.14.2"), ("boring", "5.2.0", "5.3.0"), ("boring-sys", "5.2.0", "5.3.0")],
         {root: {"zstd": {"0.12.4"}, "zstd-safe": {"6.0.6"}, "boring": {"5.2.0"}, "boring-sys": {"5.2.0"},
                 "syn": {"2.0.119"}},
          vendor: {"zstd": {"0.12.4"}, "zstd-safe": {"6.0.6"}}},
         {root: {"zstd@0.12.4": [("zstd", "0.12.4", "0.14.2"), ("zstd-safe", "6.0.6", "8.0.5")],
                 "boring@5.2.0": [("boring", "5.2.0", "5.3.0"), ("boring-sys", "5.2.0", "5.3.0"),
                                  ("foreign-types-shared", None, "0.4.1")]},
          vendor: {"zstd@0.12.4": [("zstd", "0.12.4", "0.14.2"), ("zstd-safe", "6.0.6", "8.0.5")]}},
         {("zstd-safe", "8.0.5"): ("怀疑期内", "仅 3.0 小时"),
          ("foreign-types-shared", "0.4.1"): ("怀疑期内", "仅 1.0 小时")},
         {(root, "zstd-safe", "8.0.5"): "zstd@0.12.4 -> 0.14.2",
          (root, "foreign-types-shared", "0.4.1"): "boring@5.2.0 -> 5.3.0",
          (vendor, "zstd-safe", "8.0.5"): "zstd@0.12.4 -> 0.14.2"},
         set()),
        ("只在 vendor 锁里的包被带到已 yank／查不到的版本 · 被带到候选清单之外的老版本已过怀疑期，放行",
         [("reqwest", "0.12.28", "0.12.29"), ("jiff", "0.2.15", "0.2.16")],
         {root: {"jiff": {"0.2.15"}, "portable-atomic": {"1.11.0"}},
          vendor: {"reqwest": {"0.12.28"}, "quinn-proto": {"0.11.16"}, "jiff": {"0.2.15"},
                   "portable-atomic": {"1.11.0"}}},
         {root: {"jiff@0.2.15": [("jiff", "0.2.15", "0.2.16"), ("portable-atomic", "1.11.0", "1.11.1")]},
          vendor: {"reqwest@0.12.28": [("reqwest", "0.12.28", "0.12.29"), ("quinn-proto", "0.11.16", "0.11.20"),
                                       ("quinn-udp", None, "0.6.0")],
                   "jiff@0.2.15": [("jiff", "0.2.15", "0.2.16"), ("portable-atomic", "1.11.0", "1.11.1")]}},
         {("quinn-proto", "0.11.20"): ("已 yank", "该版本已被撤回"),
          ("quinn-udp", "0.6.0"): ("查不到", "crates.io 查询失败"),
          ("portable-atomic", "1.11.1"): (None, "1000 小时前")},
         {(vendor, "quinn-proto", "0.11.20"): "reqwest@0.12.28 -> 0.12.29",
          (vendor, "quinn-udp", "0.6.0"): "reqwest@0.12.28 -> 0.12.29"},
         {(root, "portable-atomic", "1.11.1"), (vendor, "portable-atomic", "1.11.1")}),
        ("vendor 独有的一批（[`vendor_only`]）：目标被同批前一项带到位不再查 · 顺带动的老包已过怀疑期，放行",
         [("quinn", "0.11.11", "0.11.12"), ("quinn-proto", "0.11.16", "0.11.19")],
         {root: {"tokio": {"1.53.3"}},
          vendor: {"quinn": {"0.11.11"}, "quinn-proto": {"0.11.16"}, "quinn-udp": {"0.5.15"}, "tokio": {"1.53.3"}}},
         {vendor: {"quinn@0.11.11": [("quinn", "0.11.11", "0.11.12"), ("quinn-proto", "0.11.16", "0.11.19"),
                                     ("quinn-udp", "0.5.15", "0.5.16")]}},
         {("quinn-udp", "0.5.16"): (None, "800 小时前")},
         {},
         {(vendor, "quinn-udp", "0.5.16")}),
        ("没有闯入者：同名两个大版本各升一格 · 新版本甩掉的依赖消失 · 新增包本身是采纳目标",
         [("syn", "2.0.119", "2.0.120"), ("syn", "3.0.3", "3.0.4"), ("aaa-new", None, "0.1.0")],
         {root: {"syn": {"2.0.119", "3.0.3"}, "proc-macro-hack": {"0.5.20"}}, vendor: {}},
         {root: {"syn@2.0.119": [("syn", "2.0.119", "2.0.120"), ("proc-macro-hack", "0.5.20", None)],
                 "syn@3.0.3": [("syn", "3.0.3", "3.0.4")],
                 "aaa-new": [("aaa-new", None, "0.1.0")]}},
         {},
         {},
         set()),
    ]
    bad = []
    for label, eligible, before, effects, verdicts, want_bad, want_ok in cases:
        state = {lock: {n: set(vs) for n, vs in pkgs.items()} for lock, pkgs in before.items()}
        stray: list[str] = []

        def vet(name, ver, verdicts=verdicts, stray=stray):
            if (name, ver) not in verdicts:
                stray.append(f"{name} {ver}")
                return None, ""
            return verdicts[(name, ver)]

        blame: dict[tuple[str, str, str], str] = {}
        for lock in state:
            def update(spec, _version, lock=lock):
                for name, old, new in effects.get(lock, {}).get(spec, []):
                    vers = state[lock].setdefault(name, set())
                    vers.discard(old)
                    if new:
                        vers.add(new)
                return 0, ""

            step = blaming(update, lambda lock=lock: {n: set(vs) for n, vs in state[lock].items()}, lock, blame)
            for name, cur, new in eligible:
                step(f"{name}@{cur}" if cur else name, new)

        found = lock_intruders(before, state, eligible, vet)
        got_bad = {(lk, n, v): blame.get((lk, n, v)) for lk, n, v, why, _d in found if why}
        got_ok = {(lk, n, v) for lk, n, v, why, _d in found if not why}
        if got_bad != want_bad:
            bad.append(f"闯入者核对「{label}」：应判红 {sorted(want_bad.items()) or '无'}，"
                       f"实际 {sorted(got_bad.items()) or '无'}")
        if got_ok != want_ok:
            bad.append(f"闯入者核对「{label}」：应放行 {sorted(want_ok) or '无'}，实际 {sorted(got_ok) or '无'}")
        if stray:
            bad.append(f"闯入者核对「{label}」：不该查怀疑期的也查了：{' '.join(stray)}")
    return bad, len(cases)


def _self_check_lockstep() -> tuple[list[str], int]:
    """釘住 [`adopt_root`] 的 `update_group`：逐項沒到位的才一起升，一起升之後照舊按結果鎖判。兩個方向都要有。

    每條：(說明, 候選, 採納前的鎖, 逐項各自的改動, 一起升的改動, 應判失敗的名字, 一起升應收到的 spec)。
    改動是 `{名字: 新版本集合}`，由樁寫進假鎖；逐項那一輪不在表裡的 spec 什麼都不動、退 101。版本號是合成的。
    """
    cases = [
        ("互相 `=` 钉死的一族（2026-10-16 的 wasm-bindgen / js-sys 形状）：逐项全红、一起升到位",
         [("js-sys", "0.3.104", "0.3.106"), ("wasm-bindgen", "0.2.127", "0.2.129")],
         {"js-sys": {"0.3.104"}, "wasm-bindgen": {"0.2.127"}},
         {},
         {"js-sys": {"0.3.106"}, "wasm-bindgen": {"0.2.129"}},
         set(), ["js-sys@0.3.104", "wasm-bindgen@0.2.127"]),
        ("一起也升不动：照旧判失败",
         [("js-sys", "0.3.104", "0.3.106"), ("wasm-bindgen", "0.2.127", "0.2.129")],
         {"js-sys": {"0.3.104"}, "wasm-bindgen": {"0.2.127"}},
         {},
         {},
         {"js-sys", "wasm-bindgen"}, ["js-sys@0.3.104", "wasm-bindgen@0.2.127"]),
        ("逐项都到位：不一起升",
         [("quinn", "0.11.11", "0.11.12")],
         {"quinn": {"0.11.11"}},
         {"quinn@0.11.11": {"quinn": {"0.11.12"}}},
         {"quinn": {"0.11.99"}},
         set(), None),
        ("一起升落到候选清单之后才发的版本（不是目标）· 新增包没有旧版本可指名、不进一起升",
         [("js-sys", "0.3.104", "0.3.106"), ("wasm-bindgen", "0.2.127", "0.2.129"), ("ccc-new", None, "0.1.0")],
         {"js-sys": {"0.3.104"}, "wasm-bindgen": {"0.2.127"}},
         {},
         {"js-sys": {"0.3.107"}, "wasm-bindgen": {"0.2.129"}},
         {"js-sys", "ccc-new"}, ["js-sys@0.3.104", "wasm-bindgen@0.2.127"]),
    ]
    bad = []
    for label, eligible, before, per_item, together, want, want_group in cases:
        lock = {n: set(vs) for n, vs in before.items()}
        calls: list[list[str]] = []

        def update(spec, _version, lock=lock, per_item=per_item):
            if spec not in per_item:
                return 101, f"error: failed to select a version for `{spec}`"
            lock.update({n: set(vs) for n, vs in per_item[spec].items()})
            return 0, ""

        def update_group(specs, lock=lock, together=together, calls=calls):
            calls.append(list(specs))
            lock.update({n: set(vs) for n, vs in together.items()})
            return (0, "") if together else (101, "error: failed to select a version")

        with contextlib.redirect_stdout(io.StringIO()):
            got = {n for n, _why in adopt_root(eligible, update, lambda lock=lock: lock, update_group=update_group)}
        if got != want:
            bad.append(f"一起升「{label}」：应判失败 {sorted(want) or '无'}，实际判了 {sorted(got) or '无'}")
        if calls != ([want_group] if want_group else []):
            bad.append(f"一起升「{label}」：应收到 {want_group or '（不调）'}，实际 {calls or '（没调）'}")
    return bad, len(cases)


def _self_check_vendor_only() -> tuple[list[str], int]:
    """釘住 [`vendor_only`]：vendor dry-run 的候選裡只收根那條路夠不到的。每條：(說明, 候選, 根鎖, 應收下的)。"""
    root = {"defmt": {"1.1.1"}, "rand": {"0.10.3"}, "synstructure": {"0.13.2"}, "base64": {"0.22.1", "0.23.1"}}
    cases = [
        ("只在 vendor 锁里的名字（2026-10-16 的 quinn-proto）", [("quinn-proto", "0.11.16", "0.11.19")],
         {("quinn-proto", "0.11.16", "0.11.19")}),
        ("名字@旧版本 根锁也有：归根那条路，不收", [("defmt", "1.1.1", "1.2.0")], set()),
        ("同名、旧版本只在 vendor（老大版本）", [("rand", "0.8.7", "0.8.8")], {("rand", "0.8.7", "0.8.8")}),
        ("新增包：新版本根锁没有才收", [("synstructure", None, "0.14.0"), ("base64", None, "0.23.1")],
         {("synstructure", None, "0.14.0")}),
    ]
    bad = []
    for label, candidates, want in cases:
        got = set(vendor_only(candidates, root))
        if got != want:
            bad.append(f"vendor 独有的筛选「{label}」：应收 {sorted(want, key=str) or '无'}，实际 {sorted(got, key=str) or '无'}")
    return bad, len(cases)


def _self_check_dry_run() -> tuple[list[str], int]:
    """釘住 [`parse_dry_run`]：真實形狀的 dry-run 輸出喂進去，候選一個不丟，不該收的一個不收。

    每條：(說明, dry-run 輸出, 應得的候選)。
    ★ A–D 四組是 2026-10-16 在 cargo 1.98.0（fulcrum-build:local）裡對一次性專案實測的**原樣輸出**（C 組只刪了幾行）；
      E–F 兩組照 cargo 原始碼（797e8a9bc `ops/cargo_update.rs`）裡 `report_*` 與 `PackageChange` 拼出的形狀寫的，
      本機穩定版 cargo 打不出來。
    ⚠ 這裡曾經什麼都沒有：`UPDATING` / `ADDING` 錨在行尾，`(available: …)` 一掛上，整條候選被悄悄丟掉
      （在 `zstd = ">=0.12, <0.14"` 的一次性專案裡重現；真實樹當天 4 行、0 行帶註記，是潛伏的）。
    """
    cases = [
        ("A 直接依赖有跨大版本的更新：`(available: v0.14.2)`（zstd 那次的原样输出）",
         """\
    Updating crates.io index
     Locking 2 packages to latest Rust 1.98.0 compatible versions
    Updating zstd v0.12.4 -> v0.13.3 (available: v0.14.2)
    Updating zstd-safe v6.0.6 -> v7.3.0
warning: not updating lockfile due to dry run
""",
         [("zstd", "0.12.4", "0.13.3"), ("zstd-safe", "6.0.6", "7.3.0")]),
        ("B 无注记的 Updating / Adding 照收，`Removing` 不收",
         """\
    Updating crates.io index
     Locking 3 packages to latest Rust 1.85 compatible versions
      Adding memchr v2.8.3
    Removing ryu v1.0.23
    Updating serde_json v1.0.100 -> v1.0.154
      Adding zmij v1.0.23
warning: not updating lockfile due to dry run
""",
         [("memchr", None, "2.8.3"), ("serde_json", "1.0.100", "1.0.154"), ("zmij", None, "1.0.23")]),
        ("C MSRV 回退：Updating 与 Adding 都带 `(available: …, requires Rust 1.85)`",
         """\
    Updating crates.io index
     Locking 14 packages to latest Rust 1.75 compatible versions
    Removing addr2line v0.24.2
      Adding anstream v1.0.0
    Removing atty v0.2.14
    Updating clap v4.0.0 -> v4.5.61 (available: v4.6.7, requires Rust 1.85)
      Adding clap_builder v4.5.61 (available: v4.6.7, requires Rust 1.85)
    Updating clap_lex v0.3.3 -> v1.0.1 (available: v1.1.1, requires Rust 1.85)
    Updating strsim v0.10.0 -> v0.11.1
    Updating tokio v1.30.0 -> v1.53.3
    Removing winapi v0.3.9
warning: not updating lockfile due to dry run
""",
         [("anstream", None, "1.0.0"), ("clap", "4.0.0", "4.5.61"), ("clap_builder", None, "4.5.61"),
          ("clap_lex", "0.3.3", "1.0.1"), ("strsim", "0.10.0", "0.11.1"), ("tokio", "1.30.0", "1.53.3")]),
        ("D `--precise` 往回退：`Downgrading` 不是采纳候选，同一输出里的 Adding 照收",
         """\
    Updating crates.io index
    Removing memchr v2.8.3
      Adding ryu v1.0.23
 Downgrading serde_json v1.0.154 -> v1.0.100
    Removing zmij v1.0.23
warning: not updating lockfile due to dry run
""",
         [("ryu", None, "1.0.23")]),
        ("E 源码里的另三种注记：`(requires Rust X)` · `(published …, minimum age …)` · `(available: …, requires …, published …)`",
         """\
    Updating foo v1.2.0 -> v1.3.0 (requires Rust 1.99)
    Updating bar v0.4.1 -> v0.4.2 (published 3 hours ago, minimum age 7 days)
      Adding baz v2.0.0 (available: v2.0.1, published 3 hours ago)
    Updating qux v1.0.0 -> v1.0.1 (available: v1.0.2, requires Rust 1.90, published 5 hours ago)
""",
         [("foo", "1.2.0", "1.3.0"), ("bar", "0.4.1", "0.4.2"), ("baz", None, "2.0.0"), ("qux", "1.0.0", "1.0.1")]),
        ("F 不是候选：带注记的 Downgrading · `--verbose` 才有的 Unchanged · 来源不是 crates.io（git／path）· 收尾的 note",
         """\
 Downgrading foo v1.3.0 -> v1.2.9 (available: v1.3.1)
   Unchanged foo v1.0.0 (available: v2.0.0)
    Updating gitdep v1.0.0 (https://github.com/example/gitdep#abc12345) -> #def67890
    Updating pathdep v1.0.0 (/work/vendor/pathdep) -> v1.1.0
note: pass `--verbose` to see 3 unchanged dependencies behind latest
""",
         []),
    ]
    bad = []
    for label, out, want in cases:
        got = parse_dry_run(out)
        if got != want:
            lost = [c for c in want if c not in got]
            extra = [c for c in got if c not in want]
            bad.append(f"dry-run 行解析「{label}」：丢了 {lost or '无'}，多收了 {extra or '无'}")

    # 再走一遍真正的入口：用解釋器假扮 cargo（把第一個參數原樣吐到 stderr、退 0），
    # 證明 `cargo_candidates` 確實經過 `parse_dry_run`，不是只有自測在用它。
    label, out, want = cases[0]
    fake_cargo = [sys.executable, "-c", "import sys; sys.stderr.write(sys.argv[1])", out]
    got = cargo_candidates(fake_cargo)
    if got != want:
        bad.append(f"dry-run 行解析「{label}」经 cargo_candidates：应得 {want}，实际 {got}")
    return bad, len(cases) + 1


def self_check() -> int:
    """`--self-check`：用合成輸入釘住 `--apply` 的五處判定 —— 不出網、不跑 cargo。

    一、[`lock_alignment`] 的每種判定。
    ★ 口徑必須與 `tests/vendor/run.sh` 的 [2/5] 相同：根側的每個版本都得在 vendor 側，
      **除非沒有任何 pingora crate 用它**（那一問由 cargo tree 答，這裡用樁代替）。
      ⚠ 這裡曾經是「兩邊版本集合逐字相等」—— 比它聲稱鏡像的那道門更嚴，
      2026-09-24 那次 `--apply` 因此把 rand / thiserror / thiserror-impl 誤報成對不上、結束代碼 1。

    二、[`adopt_root`] 按結果鎖判成敗，見 [`_self_check_adopt_root`]。

    三、順帶帶進來的版本過懷疑期、點名是誰帶的，見 [`_self_check_intruders`]。

    四、vendor 清單的候選只收根那條路夠不到的，見 [`_self_check_vendor_only`]。

    五、互相 `=` 釘死的一族逐項升不動時一起升、照舊按結果鎖判，見 [`_self_check_lockstep`]。

    另：[`parse_dry_run`] 認不認得 cargo 的 dry-run 行（含行尾括號註記）不屬上面幾處 `--apply` 判定，見 [`_self_check_dry_run`]。
    """
    users = {
        ("base64", "0.23.1"): [],  # 只經 rcgen → pem 4 進產物（2026-09-24 的真實形狀）
        ("lru", "0.18.2"): ["pingora-pool"],  # pingora 在用、vendor 卻沒解析出來 —— 真問題
        ("zstd", "9.9.9"): None,  # 問不出來
    }
    root = {
        "rand": {"0.10.3"},
        "base64": {"0.22.1", "0.23.1"},
        "lru": {"0.18.2"},
        "zstd": {"9.9.9"},
        "pem": {"4.0.0"},
    }
    vendor = {
        "quinn-proto": {"0.11.16"},  # 只在 vendor 鎖裡（2026-10-16 的真實形狀）
        "rand": {"0.8.7", "0.10.3"},  # vendor 側合法地多一個老大版本（dev-dep／可選相依）
        "base64": {"0.22.1"},
        "lru": {"0.18.5"},
        "zstd": {"0.14.0"},
    }
    want = {
        "rand": "ok",
        "base64": "root-only-path",
        "lru": "mismatch",
        "zstd": "mismatch",  # ⛔ 問不出來 ≠ 沒人用
        "pem": "absent",
        "quinn-proto": "vendor-only",
    }
    got = {n: (v, why) for n, v, why in lock_alignment(root, vendor, list(want), lambda n, v: users[(n, v)])}
    bad = [f"{n}：期望 {w}，得到 {got.get(n, ('（缺）', ''))[0]}" for n, w in want.items() if got.get(n, ("",))[0] != w]
    if not bad and "pingora-pool" not in got["lru"][1]:
        bad.append("lru：判红的说明里没点名是哪个 pingora crate 在用它")
    root_bad, root_cases = _self_check_adopt_root()
    bad += root_bad
    intruder_bad, intruder_cases = _self_check_intruders()
    bad += intruder_bad
    dry_run_bad, dry_run_cases = _self_check_dry_run()
    bad += dry_run_bad
    vendor_bad, vendor_cases = _self_check_vendor_only()
    bad += vendor_bad
    lockstep_bad, lockstep_cases = _self_check_lockstep()
    bad += lockstep_bad
    if bad:
        print("DEP-CHECK SELF-CHECK FAILED：")
        for line in bad:
            print(f"  ✗ {line}")
        return 1
    print(f"DEP-CHECK SELF-CHECK PASSED —— 两把锁的对齐判定 {len(want)} 种情况各钉一条（与 tests/vendor/run.sh [2/5] 同一口径）；"
          f"根锁采纳按结果锁判成败 {root_cases} 组（两个方向都有）；"
          f"cargo dry-run 行解析 {dry_run_cases} 组（含行尾注记；`Downgrading` 等非候选行不收）；"
          f"顺带带进来的版本过怀疑期 {intruder_cases} 组（判红、放行、不该查的不查）；"
          f"vendor 独有的候选筛选 {vendor_cases} 组；"
          f"互相钉死的一族一起升 {lockstep_cases} 组（两个方向都有）")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
