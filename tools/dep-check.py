#!/usr/bin/env python3
"""每周依赖检查 —— 实现 PLAN.md G29 的「追新 + 24 小时安全怀疑期」。

G29 的口径是：`Cargo.toml` 用 `>=` 下界、不设上界，追最新（**包括破坏性大版本**），
但**任何版本必须已发布满 24 小时才允许采纳**。后半句 Cargo 表达不了，只能由本脚本执行。

这道怀疑期挡的是一类具体攻击：投毒版本发布后数小时内被发现并 yank。盲目 `cargo update`
会正好撞上那个窗口；等满 24 小时再取，绝大多数这类事件已经暴露。

做法：
  1. 用 `cargo update --dry-run` 问出「如果现在更新，会动哪些包」——只有这批才需要查；
     全量查 Cargo.lock 里几百个包纯属浪费。★ 根清单与 vendor 清单各问一次，vendor 那份只收
     根锁里没有的版本（见 `vendor_only`）。
  2. 对每个候选版本查 crates.io 的发布时间与 yanked 状态。
  3. 发布不足 N 小时、或已被 yank 的，一律挡下并说明原因。
  4. `--apply` 时只对**通过怀疑期**的候选做 `cargo update -p <name> --precise <ver>`；
     成败看跑完之后的两把锁，不看 cargo 的退出码（同族包会被前一项顺带带走，见 `adopt_root`）。
     ★ 顺带带进来的版本同样要过怀疑期：两把锁采纳前后逐包比，有一个过不了就**整批作废、
     两把锁逐字节还原**（见 `lock_intruders`）。

★ 另有一件 `cargo update` 结构上做不到的事：**盯住被 fork 的 pingora-core 的上游**。

G30 把 `pingora-core` 用 `[patch.crates-io]` 指向了 `vendor/pingora/`，于是它**彻底脱离了
crates.io 的更新流**——`cargo update` 再也不会为它查任何东西。上游发了 0.9 也不会有任何提示，
而 fork 的全部价值（抬上界、修掉两条公告）恰恰要靠跟进上游才不会随时间腐化。
所以本脚本单独查一次上游最新版，与 fork 的基线比对。

★ 第三件同样在 `cargo update` 视野之外的事：**构建镜像的编译器**（G36 / D13 结案）。

`docker/Dockerfile.build` 的基础镜像已钉到 digest，好处是可复现，代价是**不会自己跟进**。
G29 的精神是追新，所以这个钉子要定期拔——但拔的判据不是「digest 变没变」：

★ **构建镜像不是运行时镜像。** G13 定的分发是 musl 静态二进制、§6.2 明确不做官方容器镜像，
所以镜像里的 Debian 包（cmake / clang / glibc）**一个都不进产物**，只影响编译过程。
真正影响产物的变量只有 **rustc 版本**。因此本检查只在 rustc 变了时才影响退出码；
Debian 侧的 digest 漂移只打一行提示。

★ 与 fork rebase 一样，它**只报告、拒绝自动应用**（`--apply` 也不动）：换编译器必须重跑
三场景，不该无人值守——一次静默的编译器切换正是修掉的那个洞。

用法：
    python tools/dep-check.py                 # 只报告
    python tools/dep-check.py --apply         # 采纳通过怀疑期的更新
    python tools/dep-check.py --hours 48      # 换一个怀疑期长度
    python tools/dep-check.py --skip-fork     # 跳过 fork 上游检查
    python tools/dep-check.py --skip-image    # 跳过构建镜像检查

退出码（可叠加）：
    0   没有任何要处理的
    10  有可采纳的 cargo 更新（--apply 模式下表示已采纳）
    20  ★ fork 那条线要人管：上游有新版需要人工 rebase / **本次没能查证**
    40  ★ 构建镜像要人管：rustc 有新版 / 钉子掉了 / **pebble 有新版**（G64）/ **本次没能查证**
    80  ★ systemd 测试宿主镜像要人管（G39）：大版本变了 / **本次没能查证**
   160  ★ 有未登记的安全公告 / **本次没能查证**（新增，见下）
   255  ★ 上面几档同时亮到装不下了（见 `finish()`）——**不是第六档**，是「说不清楚」
    1   出错；`--apply` 时也表示采纳没成（含顺带带进没过怀疑期的版本、整批作废，见 `lock_intruders`）

★ 20 / 40 / 80 / 160 都把「没能查证」算作**要人管**，不算作「无事」。这是本项目复审反复抓到的
  那个形状：把失败当成成功的返回值。它们各自只发一两个请求，失败多半是真有问题，重跑也便宜。

    （各位可叠加，所以 30、50、170、250 之类都是合法组合。
      ⚠ 但**加起来不能超过 255**：五档全亮是 310，POSIX 截断成 54，
      而 54 按本编码解出来是「10+40+4」——一个不存在、而且看起来更轻的组合。
      `finish()` 负责在越界时改报 255 并逐条列出真正亮着的那几档。）

★ ★ 160 那一项为什么在这里而不是另起一个脚本：`supply-audit.py` 默认**只审根锁**，
  而且**没有任何东西自动调用它**——它一直是一件「记得的时候跑一下」的事。
  代价兑现了一次：`h2` 的 RUSTSEC-2026-0258 是**手动跑一次 vendor 锁才撞见的**。
  ⚠ 更要紧的是**产品的真实依赖住在 vendor 锁里**（`[patch.crates-io]` 把 `pingora-core`
  指向 `vendor/pingora`），而根锁连一个 rustls 相关的包都没有。**最该被扫的那把锁，恰恰没人扫。**
  并进本脚本而不另设节奏，理由同 G36：**没人记得住第二件事**。
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

# ★ 只看發版是不夠的。上游 main 上的依賴上界改動在發版前就有價值——
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
    """相对仓库根的显示名；路径不在仓库里时退回绝对路径，**不抛异常**。

    ★ 这不是洁癖：`relative_to` 在路径不在 REPO 下时会 raise，而它出现在**错误分支**的
      打印语句里——也就是说，唯一会走到它的场合恰恰是「已经出问题了」的时候，
      再抛一个 ValueError 只会把真正的诊断盖掉。
    """
    return str(p.relative_to(REPO)) if p.is_relative_to(REPO) else str(p)


def run(cmd: list[str], *, cwd: Path = REPO) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)


def cargo_candidates(cargo: list[str], manifest: str | None = None) -> list[tuple[str, str | None, str]]:
    """返回 [(crate, 当前版本 or None, 候选版本)]；`manifest` 给了就问那份清单（vendor），否则问根。"""
    proc = run([*cargo, "update", "--dry-run", *(["--manifest-path", manifest] if manifest else [])])
    out = (proc.stdout or "") + (proc.stderr or "")
    if proc.returncode != 0:
        print(out, file=sys.stderr)
        raise SystemExit(f"cargo update --dry-run 失败（退出码 {proc.returncode}）")

    return parse_dry_run(out)


def parse_dry_run(out: str) -> list[tuple[str, str | None, str]]:
    """`cargo update --dry-run` 的输出 → [(crate, 当前版本 or None, 候选版本)]。纯文本进、列表出 —— `--self-check` 直接喂它。

    只认 `Updating` 与 `Adding`（含行尾括号注记，见 [`UPDATING`] 上的说明）；`Downgrading` / `Removing` / `Unchanged`、
    `Locking …` 之类的表头、`warning:` 与 `note:` 都不是采纳候选。来源不是 crates.io 的包，行里旧版本后面跟着 `(来源)`，
    本来就匹配不上 —— 它们没有 crates.io 的发布时间可判怀疑期。
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
    """vendor 清单 dry-run 的候选里，**根那条路够不到**的那些：`名字@旧版本` 根锁里没有。

    # ★ 为什么要有这一步

    候选原先只来自根清单的 `cargo update --dry-run`，`--apply` 再把它们镜像进 vendor 锁。
    ⇒ 只在 vendor 锁里的版本（`pingora-core` 的 dev-dep、vendor 成员才开的可选依赖拖进来的）**永远不是候选**：
      2026-10-16 vendor 的 dry-run 动 29 项，25 项是这一类，其中 `quinn-proto` 0.11.16 带五条公告
      （RUSTSEC-2026-0338…0342），而本脚本一条都不报。

    ★ `名字@旧版本` 根锁也有的**不收**：它归根那条路决定、`_align_vendor_lock` 照旧镜像 ——
      两边各自决定，两把锁就会对不上（`tests/vendor/run.sh` [2/5]）。同名、但旧版本只在 vendor 的
      （vendor 合法地多一个老大版本，如 `rand` 0.8）照收。新增包（旧版本为空）看的是新版本根锁有没有。
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
    """一个具体版本过没过怀疑期：`(None, 说明)` = 可采纳，`(原因, 说明)` = 挡下。

    ★ 候选清单与采纳之后的闯入者核对（[`lock_intruders`]）共用这一个判据 —— 各写一份迟早分家。
      查不到、已 yank 同样挡下：没能查证不算通过。
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
    """候选逐个过 `vet`（同 [`quarantine_verdict`]），打出 ✓ / ✗，返回过了的那些。根与 vendor 独有的两批共用。"""
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
    """粗略的 semver 排序键；只用于「上游是不是比基线新」这一个判断。"""
    core = re.split(r"[-+]", v, maxsplit=1)[0]
    return tuple(int(x) if x.isdigit() else 0 for x in core.split("."))


# ★ 本儲存庫有**兩把鎖**，而 `tests/vendor/run.sh` 要求它們對同一個包解出同一組版本。
#   ⚠ 定義在這裡而不是各處寫字面值，是因為之前它只寫在
#     `ADVISORY_LOCKS` 裡，而 `--apply` 那條路**完全不知道第二把鎖的存在**。
VENDOR_LOCK = "vendor/pingora/Cargo.lock"
VENDOR_MANIFEST = "vendor/pingora/Cargo.toml"


def fork_baseline() -> str | None:
    """fork 当前对齐的上游版本 = vendor 里 pingora-core 的 `version`。"""
    if not FORK_MANIFEST.exists():
        return None
    # 只看 [package] 段，避免撞上依賴項裡的 version
    text = FORK_MANIFEST.read_text(encoding="utf-8").split("[dependencies]")[0]
    m = PKG_VERSION.search(text)
    return m.group(1) if m else None


def check_upstream_main(baseline: str) -> None:
    """★ 看一眼上游 main 相对基线 tag 领先多少，并挑出像依赖改动的提交。

    只报告、不影响退出码——main 上的东西还没发版，是「值得提前捞」而不是「必须跟进」。
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
    """反向比一次，列出「只在 tag 上、不在 main 上」的提交标题。

    ★ 单独一次请求，因为 `compare/A...B` **只给 A→B 那一个方向的 commits 列表**：
    `behind_by` 是个数字，而那些提交的**内容**要反着问一次才拿得到。
    ⚠ 拿不到就说「拿不到」，不返回空列表冒充「没有」——
    本仓库为「某处没有记录 ≠ 那件事没发生」付过账。
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
    """★ 被 patch 的 crate 脱离了 cargo 的更新流，必须单独查。返回 True 表示**这条线要人管**。

    ★ ★ 「要人管」包含「没能查证」，不只是「上游有新版」。
    原先这两种网络失败都 `return False`（= 无事），于是 crates.io 一抖，每周检查就会
    输出「fork 基线仍是上游最新发布版」——**它并不知道，却把不知道说成了知道**。
    这与同文件构建镜像检查、`supply-audit.py` 的查证覆盖率是同一条纪律。
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
    """Docker Hub 上某仓库里名字含 `name_filter` 的 tag → digest。不需要 Docker，也不拉镜像。

    ★ 两处检查（构建镜像 / systemd 测试宿主）共用这一份翻页逻辑，不各写一份——
      本仓库已经在「两份收尾脚本分头长歪」上吃过一次亏。
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
    """Debian 某个 suite 当前的 systemd 版本（如 trixie → `257.13-1~deb13u1`）。"""
    req = urllib.request.Request(DEB_SOURCES_API, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=30) as resp:
        data = json.load(resp)
    for v in data.get("versions", []):
        if suite in v.get("suites", []):
            return v.get("version")
    return None


def check_build_image() -> bool:
    """★ 构建镜像的编译器有没有落后（G36 / D13）。返回 True = 需要人管。

    判据只有一个：**rustc 版本**。Debian 侧的 digest 漂移只提示，不判红——
    那些包不进产物（G13 是 musl 静态二进制），把它算作失败会让这道门几乎常年亮着，
    而**永远亮着的告警等于没有告警**。
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
    """★ 门禁里那个本地 CA（pebble，G64）有没有落后。返回 True = 需要人管。

    ★ **为什么并进构建镜像那一档（40）而不是新开一档**：它就钉在
    `docker/Dockerfile.build` 里，与 rustc 同一个文件、同一种处置（改文件 → 重建镜像 →
    重跑门禁）。★ 而更硬的理由是退出码只有 8 位：现有五档相加已经是 310，
    再加一档必然溢出（见 `finish()`）。**一个会被截断的位，比没有这个位更糟。**

    ⚠ 判据是「上游有没有发新版」，不是「哈希对不对」——哈希对不对由 `docker build`
    自己在下载时验（`sha256sum -c`），装不上就当场失败，不需要这里再查一遍。
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
    """★ M1 的 systemd 测试宿主镜像有没有落后（G39）。返回 True = 需要人管。

    判据是 **systemd 大版本**，与 G36 判 rustc 同构，理由却不同：
    G36 那边「Debian 包不进产物」所以只盯 rustc；这边**被测对象本身就是 systemd**——
    M1 spike #1 的结论（alien MainPID 之下 `systemctl stop` 不再等排空）是对某个
    systemd 版本成立的，★ **版本是结论的一部分**。

    ★ 一条实测出来的边界，值得写在这里：Debian stable 在一个 suite 内**冻结大版本**，
    所以「大版本变了」实际只会在换 suite（trixie → 下一个）时发生。这道门因此不是
    「每周都可能红」的那种，而是**换基底时必然红**的那种——它守的正是那一刻。
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
# 要掃的鎖。★ 順序有意義：vendor 那把是**產品的真實依賴**，放前面。
ADVISORY_LOCKS = (VENDOR_LOCK, "Cargo.lock")


def _load_supply_audit():
    """按路径加载 `supply-audit.py`（名字带连字符，不能直接 import）。

    ★ 只为一件事：**复用它的 `ACCEPTED` 与 `osv_batch`，而不是复制一份**。
      本仓库已经吃过「同一份清单两处副本」的亏（`tests/vendor/run.sh` 那张 16 项
      手写 BUMPED 名单，是 FORK.md 上界表的人工镜像，才拆掉）。
      两份豁免名单分家的表现是：这边判红、那边判绿，而两边都说自己是权威。
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
    """第五项：对**两把锁**扫安全公告。返回 True 表示要人管。

    ★ 只做公告这一半：OSV 是批量接口（`querybatch`），几个请求就够。
      `supply-audit.py` 慢的是逐包查 crates.io 陈旧度那半，那半仍留给手动深审。
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
    """把叠加出来的退出码交出去；**溢出时改成 255 并把真相打出来**。

    ★ ★ POSIX 的退出码只有 8 位。五档全亮是 10+20+40+80+160 = **310**，
    而调用方看到的是 `310 & 0xFF` = **54**——按本脚本自己的编码解出来是
    「10 + 40 + 4」，其中 4 根本不是一档。也就是说**最严重的那次报告，
    解码出来是一个不存在的组合**，而且看起来比实际轻。
    （`20+80+160` = 260 → **4** 更糟：三档全亮，读出来像「几乎什么都没亮」。）

    ⚠ 这不是理论：这几档是一档一档加上来的，加到第五档时就已经越界了，
    而没有任何东西会说出来——**位图式的退出码天生缺一个「装不下了」的表示**。
    处置：超出就退 255（惯例上的「说不清楚」），并把**亮着的那几档逐条列出来**。
    人读得懂的清单，比一个被截断的位图有用。
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
    #    不在 `cargo update` 的視野裡，而它恰恰是本專案最重要的那個依賴。
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
        """`adopt_root` 的 `update_group`：一次 `cargo update -p a -p b …`（不带 `--precise`）。
        ★ `blaming` 在**调用时**才包：它记的是包上那一刻之后新出现的版本，早包会把逐项那一轮的也算到这一次头上。"""
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
        # ⚠ vendor 獨有的一項也可能把**兩把鎖共有**的包拉走（新版本要更新的依賴）⇒ 動過的名字都再對一次齊。
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
    """根锁那一侧的采纳：逐项 `cargo update -p 名字@旧版本 --precise 新版本`，**按结果锁判成败**。

    返回真失败的那几项 `[(名字, 说明)]`，空表 = 根锁全部到位。`update(spec, 版本)` 返回
    `(退出码, 输出)`，`read_lock()` 返回 `{名字: {版本}}` —— 都由调用方给，自测里换成桩。
    vendor 独有的那一批（[`vendor_only`]）走同一个函数，`update` / `read_lock` 换成 vendor 清单与锁、`label` 跟着换。

    # ⚠ 退出码不能当判据（与 `_align_vendor_lock` 同一条）

    同族包会被前一项**顺带带到目标版本**：`boring-sys` 跟 `boring`、`jiff-static` 跟 `jiff`、
    `zerocopy-derive` 跟 `zerocopy`、`zstd-safe` / `zstd-sys` 跟 `zstd`。轮到它自己时
    `名字@旧版本` 已经不在锁里，cargo 报 `did not match any packages` 并非零退出。
    ⇒ 2026-10-16 那次 37 项里 9 项是这个形状，`--apply` 因此退 1，而锁一项不差 ——
      一个绿的结果被报成红，比漏报更伤：读的人要么误判，要么学会无视红。
    ★ 判据取**结果**：全部跑完之后读根锁，见 [`root_lock_problems`]。反过来也成立：
      cargo 退 0 而锁不对（后一项把前一项挪走、旧版本仍被别的包钉着）同样判失败。

    # ★ 互相 `=` 钉死的一族：逐项永远升不动，一起升（`update_group`）

    `wasm-bindgen` 0.2.127 / `-macro` / `-macro-support` / `-shared` / `-futures` 与 `js-sys` / `web-sys` 彼此要 `=` 版本
    ⇒ 任何一项单独 `--precise` 都撞上另一项的 `=` 钉子（cargo 1.98.0：`failed to select a version for the
    requirement js-sys = "=0.3.104"`），2026-10-16 vendor 独有的那批 25 项里 7 项是这个形状，逐项采纳永远红。
    ⇒ 给了 `update_group(specs)` 时，逐项跑完还没到位、旧版本还在锁里的那几项**一起**交给它
    （`cargo update -p a@旧 -p b@旧 …`，不带 `--precise`：一次解开一族，落到各自最新的兼容版，实测正是 dry-run 的目标），
    然后**照旧按结果锁判**。⚠ 不带 `--precise` 可能落到候选清单之后才发的版本 ⇒ 那不是目标，这里判没到位，
    采纳后的怀疑期核对（[`lock_intruders`]）也会现查它 —— 判据不放松。
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
    """逐项核对根锁：目标版本在、旧版本不在。返回没做到的那几项 `[(名字, 说明)]`。

    ★ 同一个名字在图里可以有好几个版本，每一项只对自己那一对（旧 → 新）负责：
      旧版本恰好是同名另一项的目标时（一个依赖方 0.3 → 0.4、另一个 0.4 → 0.5），它留着是对的。
      新增包（旧版本为空）只看目标在不在。
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
    """采纳前后逐把锁比：新出现、却**不是任何一项采纳目标**的 `(名字, 版本)`，各自过一遍怀疑期。

    返回 `[(锁, 名字, 版本, 原因 or None, 说明)]`，原因非空的就是闯入者 —— G29 被绕过了。
    `before` / `after` 是 `{锁: {名字: {版本}}}`；`vet(名字, 版本)` 同 [`quarantine_verdict`]。

    # ★ ★ 为什么要有这一步

    `cargo update -p A@旧 --precise 新` 是保守更新，但 A 的新版本要求的依赖**锁里满足不了时照样会动**，
    而且被拉到「满足要求的最新版」，**不看发布时间**。cargo 1.98.0 实测：`zstd` 0.12.4 → 0.13.0
    把 `zstd-safe` 6.0.6 拉到 7.3.0，而 `^7.0.0` 有 7.0.0 就够了。
    ⇒ 同族包的最新版还在怀疑期里（候选清单里它正被挡着），或 A 的新版本新增了一个刚发版的依赖，
      `--apply` 都会把它**悄悄**采纳 —— [`root_lock_problems`] 只核「每一项到没到目标」，不核「别的动没动」。

    ★ 采纳目标在候选清单里已过过怀疑期，不再查；其余新出现的版本**一律现查**，不只认候选清单里被挡的：
      cargo 每跑一次都会刷新索引，候选清单之后才发的版本同样可能被带进来；而被带到候选清单之外、
      早已过了怀疑期的老版本（上面的 7.3.0 就是这种）也不该判红。
    ★ 消失的版本（新版本甩掉的依赖）不在此列：移走东西不是采纳。
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
    """打出 [`lock_intruders`] 的结果，返回闯入者个数（> 0 即整批作废）。"""
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
    """给 `update(spec, 版本)` 包一层：每跑完一项读一次锁，新出现的 `(锁, 名字, 版本)` 记进 `blame`，
    值是带它进来的那一项。只为报告 —— 闯入者要说得出是谁带进来的，人才知道在等哪一个。
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
    """某把锁里每个 registry 包 → 它解出来的版本集合。"""
    out: dict[str, set[str]] = {}
    for n, v in _load_supply_audit().parse_lock(REPO / rel_lock):
        out.setdefault(n, set()).add(v)
    return out


def _align_vendor_lock(cargo: list[str], eligible: list[tuple[str, str | None, str]], blame: dict) -> int:
    """把同一批更新也落进 **vendor/pingora/Cargo.lock**，并**逐个包核对两把锁一致**。

    # ★ ★ ★ 为什么要有这一步（§10）

    本仓库有**两把锁**，而 [`tests/vendor/run.sh`] 第 [2/5] 步要求它们对同一个包
    解出同一组版本，否则**拒绝跑回归网** —— 理由是「vendor 测试跑的不是产物里那套组合，
    结果不可采信」。⚠ 而本函数出现之前，`--apply` **只更新根那一把**。

    ⇒ 那 9 项更新落地之后，回归网当场红，并逐条列出 **8 个**对不上的包。
    ★ ★ 讽刺的是本脚本**早就知道有两把锁** —— 安全公告那一节读的正是两把
    （`ADVISORY_LOCKS`）。**「知道有两把」与「两把都更新」是两件事。**

    # ⚠ 退出码不能当判据

    对 vendor 清单跑 `cargo update` 有**两种良性失败**，都会让它非零：

    · 那个包根本不在 vendor 锁里（实测：`combine`）；
    · 它已被同族的另一条更新顺带带过去了（实测：`ref-cast-impl` 跟着 `ref-cast` 走）。

    ⇒ 判据取的是**结果**不是**过程**：两把锁对这个名字解出来的版本集合一不一致。
    ★ 而「vendor 锁里根本没有这个名字」算一致 —— 它没有可对不上的东西。
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
    """读两把锁，对 `names` 逐个跑 [`lock_alignment`] 并打出来，返回对不上的个数。"""
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
    """产物里有哪些 pingora crate 用到 `name@version`；问不出来返回 None。

    ★ 与 `tests/vendor/run.sh` [2/5] 的 `who_uses` 问的是同一句：根 workspace 的反向树，
      `-e normal,build`（产物里的图）、`--target all`（宁可多算路径）。
    """
    proc = run([*cargo, "tree", "--locked", "-i", f"{name}@{version}",
                "-e", "normal,build", "--target", "all", "--prefix", "none"])
    if proc.returncode != 0:
        return None
    return sorted({m.group(0) for line in proc.stdout.splitlines()
                   if (m := re.match(r"pingora-[a-z-]+", line))})


def lock_alignment(root, vendor, names, pingora_users):
    """逐个包判定两把锁对不对得上，返回 [(名字, 判定, 说明)] —— 与 [2/5] 同一口径。

    判定 ∈ "ok" / "absent"（vendor 锁里没有这个名字）/ "vendor-only"（根锁里没有这个名字）/
    "root-only-path"（根侧多出的版本没有任何 pingora crate 在用）/ "mismatch"。`pingora_users(名字, 版本)`
    返回用到它的 pingora crate 列表，问不出来返回 None（⛔ 问不出来 ≠ 没人用，一律判 mismatch）。
    ★ vendor 侧**多**出来的版本合法（dev-dep／未启用的可选依赖拖进来的老大版本），不判。
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
    """钉住 [`adopt_root`]：根锁那一侧的成败看**结果锁**，不看 cargo 的退出码。两个方向都要有。

    每条：(说明, 候选, 非零退出的 spec → 退出码, 采纳后的根锁, 应判失败的名字)。版本号是合成的。
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
    """钉住 [`lock_intruders`] 与 [`blaming`]：顺带带进来的版本逐个过怀疑期，判红的要点名是哪一项带进来的。

    每条：(说明, 候选, 采纳前的两把锁, 每一项在每把锁里的改动, 怀疑期判定, 应判红 → 带进来的那一项,
    应放行的)。改动是 `(名字, 移走的版本 or None, 加进的版本 or None)`，由 `update` 桩写进假锁，
    逐项走一遍 `blaming` 再比前后 —— 与 `--apply` 同一条路。版本号与判定都是合成的。
    ★ 判定表之外的版本被拿去查，同样算失败：采纳目标和没动的版本不该再问 crates.io。
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
    """钉住 [`adopt_root`] 的 `update_group`：逐项没到位的才一起升，一起升之后照旧按结果锁判。两个方向都要有。

    每条：(说明, 候选, 采纳前的锁, 逐项各自的改动, 一起升的改动, 应判失败的名字, 一起升应收到的 spec)。
    改动是 `{名字: 新版本集合}`，由桩写进假锁；逐项那一轮不在表里的 spec 什么都不动、退 101。版本号是合成的。
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
    """钉住 [`vendor_only`]：vendor dry-run 的候选里只收根那条路够不到的。每条：(说明, 候选, 根锁, 应收下的)。"""
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
    """钉住 [`parse_dry_run`]：真实形状的 dry-run 输出喂进去，候选一个不丢，不该收的一个不收。

    每条：(说明, dry-run 输出, 应得的候选)。
    ★ A–D 四组是 2026-10-16 在 cargo 1.98.0（fulcrum-build:local）里对一次性项目实测的**原样输出**（C 组只删了几行）；
      E–F 两组照 cargo 源码（797e8a9bc `ops/cargo_update.rs`）里 `report_*` 与 `PackageChange` 拼出的形状写的，
      本机稳定版 cargo 打不出来。
    ⚠ 这里曾经什么都没有：`UPDATING` / `ADDING` 锚在行尾，`(available: …)` 一挂上，整条候选被悄悄丢掉
      （在 `zstd = ">=0.12, <0.14"` 的一次性项目里复现；真实树当天 4 行、0 行带注记，是潜伏的）。
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
    # 證明 `cargo_candidates` 確實經過 `parse_dry_run`，不是隻有自測在用它。
    label, out, want = cases[0]
    fake_cargo = [sys.executable, "-c", "import sys; sys.stderr.write(sys.argv[1])", out]
    got = cargo_candidates(fake_cargo)
    if got != want:
        bad.append(f"dry-run 行解析「{label}」经 cargo_candidates：应得 {want}，实际 {got}")
    return bad, len(cases) + 1


def self_check() -> int:
    """`--self-check`：用合成输入钉住 `--apply` 的五处判定 —— 不出网、不跑 cargo。

    一、[`lock_alignment`] 的每种判定。
    ★ 口径必须与 `tests/vendor/run.sh` 的 [2/5] 相同：根侧的每个版本都得在 vendor 侧，
      **除非没有任何 pingora crate 用它**（那一问由 cargo tree 答，这里用桩代替）。
      ⚠ 这里曾经是「两边版本集合逐字相等」—— 比它声称镜像的那道门更严，
      2026-09-24 那次 `--apply` 因此把 rand / thiserror / thiserror-impl 误报成对不上、退出码 1。

    二、[`adopt_root`] 按结果锁判成败，见 [`_self_check_adopt_root`]。

    三、顺带带进来的版本过怀疑期、点名是谁带的，见 [`_self_check_intruders`]。

    四、vendor 清单的候选只收根那条路够不到的，见 [`_self_check_vendor_only`]。

    五、互相 `=` 钉死的一族逐项升不动时一起升、照旧按结果锁判，见 [`_self_check_lockstep`]。

    另：[`parse_dry_run`] 认不认得 cargo 的 dry-run 行（含行尾括号注记）不属上面几处 `--apply` 判定，见 [`_self_check_dry_run`]。
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
        "rand": {"0.8.7", "0.10.3"},  # vendor 側合法地多一個老大版本（dev-dep／可選依賴）
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
