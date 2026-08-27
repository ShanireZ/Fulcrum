#!/usr/bin/env python3
"""每周依赖检查 —— 实现 PLAN.md G29 的「追新 + 24 小时安全怀疑期」。

G29 的口径是：`Cargo.toml` 用 `>=` 下界、不设上界，追最新（**包括破坏性大版本**），
但**任何版本必须已发布满 24 小时才允许采纳**。后半句 Cargo 表达不了，只能由本脚本执行。

这道怀疑期挡的是一类具体攻击：投毒版本发布后数小时内被发现并 yank。盲目 `cargo update`
会正好撞上那个窗口；等满 24 小时再取，绝大多数这类事件已经暴露。

做法：
  1. 用 `cargo update --dry-run` 问出「如果现在更新，会动哪些包」——只有这批才需要查；
     全量查 Cargo.lock 里几百个包纯属浪费。
  2. 对每个候选版本查 crates.io 的发布时间与 yanked 状态。
  3. 发布不足 N 小时、或已被 yank 的，一律挡下并说明原因。
  4. `--apply` 时只对**通过怀疑期**的候选做 `cargo update -p <name> --precise <ver>`。

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
    1   出错

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
import json
import re
import subprocess
import sys
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

# Windows 控制台默认不是 UTF-8，中文输出会变乱码。强制一下，免得每周看到的都是问号。
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8")
    except (AttributeError, ValueError):
        pass

REPO = Path(__file__).resolve().parent.parent
UA = "fulcrum-dep-check (https://github.com/ShanireZ/Fulcrum)"

# G30 的 fork。基线版本直接读 fork 里 pingora-core 的 `version`，
# 不另设一份需要手工同步的记录——那种记录迟早与事实分家。
FORK_CRATE = "pingora-core"
FORK_MANIFEST = REPO / "vendor" / "pingora" / FORK_CRATE / "Cargo.toml"
FORK_DOC = "vendor/pingora/FORK.md"
FORK_UPSTREAM_REPO = "cloudflare/pingora"

# ★ 只看发版是不够的。上游 main 上的依赖上界改动在发版前就有价值——
#   nix 0.24→0.31 那条就是先落在 main、我们直接照抄省下一整轮迁移的。
#   所以除了比对 crates.io 的版本，还看一眼 main 领先基线 tag 多少提交。
DEP_BUMP_HINT = re.compile(r"\b(bump|upgrade|update|deps?|dependenc)", re.I)

PKG_VERSION = re.compile(r'^\s*version\s*=\s*"([^"]+)"', re.M)

# ── 构建镜像（G36 / D13）─────────────────────────────────────────────────────
# ★ 钉定的 tag 与 digest **只从 Dockerfile 读**，不另存一份——同 fork 基线的道理：
#   第二份记录迟早与事实分家（这个项目已经栽过一次：文档说 trixie，镜像里是 bookworm）。
BUILD_DOCKERFILE = REPO / "docker" / "Dockerfile.build"
BUILD_DOC = "docs/platform/build-and-test.md"
# 上游用来「追新」的那个浮动 tag。钉子就是从它身上取下来的。
BASE_FLOATING_TAG = "1-trixie"
BASE_HUB_REPO = "library/rust"
# FROM rust:1.97.1-trixie@sha256:3382bd…
FROM_LINE = re.compile(
    r"^FROM\s+(?P<repo>[^\s:@]+):(?P<tag>[^\s@]+?)(?:@(?P<digest>sha256:[0-9a-f]{64}))?\s*$",
    re.M,
)
# 从 `1.97.1-trixie` 里取出 `1.97.1`
TAG_VERSION = re.compile(r"^(\d+(?:\.\d+)*)-")

# ── pebble（G64）：门禁里那个本地 ACME CA ─────────────────────────────────────
# ★ 它与 rustc 钉在**同一个文件**里，处置也一样（改文件 → 重建镜像 → 重跑门禁），
#   所以并进 40 那一档，不新开一位——退出码只有 8 位，现有五档相加已经 310。
PEBBLE_REPO = "letsencrypt/pebble"
PEBBLE_PIN = re.compile(r"^ARG\s+PEBBLE_VERSION=(\S+)", re.MULTILINE)
PEBBLE_SHA_ARGS = (
    "PEBBLE_SHA256_AMD64",
    "PEBBLE_SHA256_ARM64",
    "CHALLTESTSRV_SHA256_AMD64",
    "CHALLTESTSRV_SHA256_ARM64",
)

# ── systemd 测试宿主镜像（G39 / ）───────────────────────────────────
# ★ 与 G36 的构建镜像**同构但不同判据**：那边判 rustc，这边判 **systemd 大版本**。
#   理由：M1 spike #1 的结论（alien MainPID 之下 systemctl stop 不再等排空）是
#   **对某个 systemd 版本成立**的，版本本身就是结论的一部分。浮动 tag 会让结论某天
#   悄悄失效，而没有任何一行输出会说出来——这正是构建镜像那次栽过的形状。
SYSTEMD_DOCKERFILE = REPO / "docker" / "Dockerfile.systemd"
SYSTEMD_HUB_REPO = "library/debian"
SYSTEMD_DOC = "docs/verification/m1-systemd.md"
# ★ 钉定的 systemd 版本写在 Dockerfile 里一行**机器可读**的注释上，与 FROM 行同处一个文件。
#   不另存第二份——同 fork 基线与构建镜像的道理（第二份记录迟早与事实分家）。
PINNED_SYSTEMD = re.compile(r"^#\s*fulcrum-pinned-systemd:\s*(?P<ver>\S+)\s*$", re.M)
# Debian 的源码 API：一个很小的 JSON，直接给出每个 suite 当前的版本。纯 HTTP。
DEB_SOURCES_API = "https://sources.debian.org/api/src/systemd/"
# 从 `257.13-1~deb13u1` 里取出大版本 `257`
SYSTEMD_MAJOR = re.compile(r"^(\d+)")

# cargo 的 dry-run 输出形如：
#     Updating pingora-core v0.8.1 -> v0.9.0
#       Adding some-new-crate v1.2.3
UPDATING = re.compile(r"^\s*Updating\s+(\S+)\s+v(\S+)\s+->\s+v(\S+)\s*$")
ADDING = re.compile(r"^\s*Adding\s+(\S+)\s+v(\S+)\s*$")


def rel(p: Path) -> str:
    """相对仓库根的显示名；路径不在仓库里时退回绝对路径，**不抛异常**。

    ★ 这不是洁癖：`relative_to` 在路径不在 REPO 下时会 raise，而它出现在**错误分支**的
      打印语句里——也就是说，唯一会走到它的场合恰恰是「已经出问题了」的时候，
      再抛一个 ValueError 只会把真正的诊断盖掉。
    """
    return str(p.relative_to(REPO)) if p.is_relative_to(REPO) else str(p)


def run(cmd: list[str], *, cwd: Path = REPO) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)


def cargo_candidates(cargo: list[str]) -> list[tuple[str, str | None, str]]:
    """返回 [(crate, 当前版本 or None, 候选版本)]。"""
    proc = run([*cargo, "update", "--dry-run"])
    out = (proc.stdout or "") + (proc.stderr or "")
    if proc.returncode != 0:
        print(out, file=sys.stderr)
        raise SystemExit(f"cargo update --dry-run 失败（退出码 {proc.returncode}）")

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


def crate_versions(name: str) -> dict[str, dict]:
    url = f"https://crates.io/api/v1/crates/{name}"
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=30) as resp:
        data = json.load(resp)
    return {v["num"]: v for v in data.get("versions", [])}


def parse_ts(raw: str) -> datetime:
    # crates.io 给的是 RFC3339，尾巴可能是 Z
    return datetime.fromisoformat(raw.replace("Z", "+00:00"))


def version_key(v: str) -> tuple:
    """粗略的 semver 排序键；只用于「上游是不是比基线新」这一个判断。"""
    core = re.split(r"[-+]", v, maxsplit=1)[0]
    return tuple(int(x) if x.isdigit() else 0 for x in core.split("."))


# ★ 本仓库有**两把锁**，而 `tests/vendor/run.sh` 要求它们对同一个包解出同一组版本。
#   ⚠ 定义在这里而不是各处写字面量，是因为之前它只写在
#     `ADVISORY_LOCKS` 里，而 `--apply` 那条路**完全不知道第二把锁的存在**。
VENDOR_LOCK = "vendor/pingora/Cargo.lock"
VENDOR_MANIFEST = "vendor/pingora/Cargo.toml"


def fork_baseline() -> str | None:
    """fork 当前对齐的上游版本 = vendor 里 pingora-core 的 `version`。"""
    if not FORK_MANIFEST.exists():
        return None
    # 只看 [package] 段，避免撞上依赖项里的 version
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

    # ★ ★ ★ 「落后」这一半必须先说，而且要说得比「领先」那一半响。
    #   实测：main 与 0.8.1 是 **diverged**（共同祖先是 0.8.0），
    #   而 main **落后 8 个提交**，那 8 个里有 `RUSTSEC-2026-0098/0099 fixes` 与
    #   `Bound default HTTP/2 server limits to mitigate memory exhaustion`。
    #   ⇒ 本函数原先只读 `ahead_by`，于是它说的是「main 领先 190 个提交」——
    #     一句会被读成「main ⊇ tag + 190」的话，而那是假的。
    #   ★ 那句话当天真的把一个决定引到了「从 main 捞改动」上。
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
    for _ in range(5):  # 有界翻页，别写成无限循环
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
    # ★ 这一条同时守着自己的修复：钉子被人拿掉了，本检查就该红。
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
        # ★ 查不到 ≠ 没落后。**「没能检查」不许当成「检查通过」**——这是本项目复审
        #   两轮反复抓到的形状。只有一个请求，失败多半是真有问题，重跑也便宜。
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

    # digest 变了。★ 只有 rustc 也变了才判红。
    # 找出与浮动 tag 同 digest 的那个精确版本 tag，例如 `1.97.2-trixie`。
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
        # 同一个 rustc，只是 Debian 侧重打了包。按 G36 只提示。
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
        # ★ 这一条同时守着自己：有人把 pebble 那段删了或改了写法，本检查就该红，
        #   而不是安静地什么都不查（「没能检查」当成「检查通过」是本项目栽过的形状）。
        print(f"★ {rel(BUILD_DOCKERFILE)} 里读不出 `ARG PEBBLE_VERSION=` —— "
              f"pebble 那一段被改过写法？本次 pebble 没有结论。")
        return True
    pinned = m.group(1)

    # ★ 四个哈希也要在。少一个架构的哈希，那个架构上的门禁会在构建时才炸，
    #   而那时的报错离「谁把它删了」已经很远。
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
    # ★ 与 G36 那条一样，这一句同时守着自己的修复：钉子被人拿掉了，本检查就该红。
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
        # ★ 查不到 ≠ 没落后。**「没能检查」不许当成「检查通过」**（本项目已抓到过五种面目）。
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
        # 同一个大版本，只是 Debian 侧重打了包或出了小版本。按 G36 的口径只提示。
        print(f"systemd 测试宿主的大版本仍是 {pinned_major}，无需拔钉子。")
        if live_sd != pinned_sd:
            print(f"  （suite `{suite}` 的 systemd 已是 {live_sd}，钉的是 {pinned_sd}；"
                  f"同一大版本，按 G39 只提示不判红）")
        if live_digest != pinned_digest:
            print(f"  （`{pinned_tag}` 的 digest 已重打为 {live_digest[:19]}…）")
        return False

    print(f"systemd 测试宿主已是 `{pinned_tag}` 当前指向的那一个（systemd {pinned_sd}），无需拔钉子。")
    return False

# ── 安全公告（第五项）───────────────────────────────────────────
# 要扫的锁。★ 顺序有意义：vendor 那把是**产品的真实依赖**，放前面。
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

    # ★ 反向校验：ACCEPTED 里有条目在两把锁上都不再命中 → 它该被删掉，否则名单会烂。
    #   ⚠ 只在**确实查成了**的时候才做——`unqueried` 非空时上面已经 return，所以这里安全。
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
    # ⚠ ⚠ **这几档不是二进制位**，是十进制的 10 / 20 / 40 / 80 / 160。
    #   ⚠ 写成 `code & bit == bit` 是错的 —— 文档里虽然管它们叫「一位」，
    #   我就当真按位与去解了。结果 310 解出来**只有「fork 上游要人管」一条**：
    #   一份读起来很像结论的错报告，而且比真相轻得多。
    #   ★ 它们都是 10 的倍数、逐档翻倍，所以先除以 10 才是真正的位图。
    #   （这条与本项目反复记的是同一件事：**判据要照定义写，不照它长得像什么写**。）
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
    args = ap.parse_args()

    cargo = args.cargo.split()
    cutoff = datetime.now(timezone.utc) - timedelta(hours=args.hours)

    # ── fork 的上游先查。它与 cargo 那条路完全独立：patch 过去的 crate
    #    不在 `cargo update` 的视野里，而它恰恰是本项目最重要的那个依赖。
    fork_behind = False
    if not args.skip_fork:
        print("── fork 上游检查 ──")
        fork_behind = check_fork_upstream(cutoff)
        print()
    fork_code = 20 if fork_behind else 0

    # ── 构建镜像（G36 / D13）。同样在 `cargo update` 的视野之外。
    image_behind = False
    if not args.skip_image:
        print("── 构建镜像检查 ──")
        image_behind = check_build_image()
        # ★ pebble 并进同一档（40）。⚠ 两个都要跑完再取或，**不能写成
        #   `check_build_image() or check_pebble()`**：`or` 短路，rustc 一红
        #   pebble 那条就整轮不查了——而它恰恰是「没能检查算作检查通过」的近亲。
        print()
        print("── 门禁本地 CA（pebble，G64）检查 ──")
        pebble_behind = check_pebble()
        image_behind = image_behind or pebble_behind
        print()
    image_code = 40 if image_behind else 0

    # ── systemd 测试宿主镜像（G39）。同样在 `cargo update` 的视野之外，
    #    而且它比构建镜像更需要盯着：**systemd 的版本是 M1 结论的一部分**。
    systemd_behind = False
    if not args.skip_systemd_image:
        print("── systemd 测试宿主镜像检查 ──")
        systemd_behind = check_systemd_image()
        print()
    systemd_code = 80 if systemd_behind else 0

    # ── 安全公告。★ 它与上面三条一样在 `cargo update` 的视野之外：
    #    `cargo update` 只管「有没有新版」，管不了「现有版本有没有公告」。
    advisories_bad = False
    if not args.skip_advisories:
        print("── 安全公告检查（两把锁）──")
        advisories_bad = check_advisories()
        print()
    advisory_code = 160 if advisories_bad else 0

    print("── crates.io 更新检查 ──")
    candidates = cargo_candidates(cargo)
    if not candidates:
        print("没有可用更新，依赖已是最新。")
        print("★ 注意：这句话只覆盖 `cargo update` 能动的东西——"
              "被上游清单上界卡住的依赖它一个都不会报。全量审计见 docs/platform/supply-chain.md。")
        return finish(fork_code + image_code + systemd_code + advisory_code)

    # ★ ★ 第二格是**当前版本**，而它是补上的，理由是一次真的失败：
    #   `syn` 在图里有两个大版本（2.0.119 与 3.0.3），于是
    #   `cargo update -p syn --precise 3.0.4` 报 `specification 'syn' is ambiguous`，
    #   ⚠ 而**另外八项全成功了** —— 一次「大部分绿」的采纳，最容易被读成全绿。
    #   ⇒ 用 `name@当前版本` 这个 pkgid spec 去指名道姓（cargo 自己在那条报错里
    #     给出的正是这种写法）。
    eligible: list[tuple[str, str | None, str]] = []
    blocked: list[tuple[str, str, str]] = []

    print(f"候选更新 {len(candidates)} 项，怀疑期 {args.hours} 小时（早于 {cutoff:%Y-%m-%d %H:%M UTC} 发布的才算数）\n")

    for name, cur, new in candidates:
        label = f"{name} {cur or '(新增)'} -> {new}"
        try:
            versions = crate_versions(name)
        except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError) as e:
            blocked.append((label, "查不到", f"crates.io 查询失败：{e}"))
            continue

        meta = versions.get(new)
        if meta is None:
            blocked.append((label, "查不到", "crates.io 上没有这个版本（本地索引比线上新？）"))
            continue
        if meta.get("yanked"):
            blocked.append((label, "已 yank", "该版本已被撤回"))
            continue

        published = parse_ts(meta["created_at"])
        age_h = (datetime.now(timezone.utc) - published).total_seconds() / 3600
        if published > cutoff:
            blocked.append((label, "怀疑期内", f"发布于 {published:%Y-%m-%d %H:%M UTC}，仅 {age_h:.1f} 小时"))
        else:
            eligible.append((name, cur, new))
            print(f"  ✓ {label}    （发布于 {published:%Y-%m-%d %H:%M UTC}，{age_h:.0f} 小时前）")

    if blocked:
        print("\n被挡下的：")
        for label, why, detail in blocked:
            print(f"  ✗ {label}    [{why}] {detail}")

    if not eligible:
        print("\n没有通过怀疑期的更新，本次什么都不做。")
        return finish(fork_code + image_code + systemd_code + advisory_code)

    if not args.apply:
        print(f"\n{len(eligible)} 项可采纳。加 --apply 执行。")
        return finish(10 + fork_code + image_code + systemd_code + advisory_code)

    print("\n开始采纳：")
    failed = 0
    for name, cur, version in eligible:
        # ⚠ 同名多版本时裸 `name` 是**歧义的**，而 cargo 会因此整条失败。
        #   带上当前版本就唯一了；`cur` 为空（新增包）时只能裸着来。
        spec = f"{name}@{cur}" if cur else name
        proc = run([*cargo, "update", "-p", spec, "--precise", version])
        ok = proc.returncode == 0
        print(f"  {'✓' if ok else '✗'} {spec} -> {version}")
        if not ok:
            failed += 1
            print((proc.stderr or proc.stdout).strip())

    failed += _align_vendor_lock(cargo, eligible)

    print(
        "\n★ 记住 G29 的后半句：升级之后必须跑完当前里程碑的全部验证"
        "（M0 是 tests/m0/run.sh；M3 之后是全量对拍）。"
    )
    return 1 if failed else finish(10 + fork_code + image_code + systemd_code + advisory_code)


def _align_vendor_lock(cargo: list[str], eligible: list[tuple[str, str | None, str]]) -> int:
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
    for name, cur, version in eligible:
        spec = f"{name}@{cur}" if cur else name
        run([*cargo, "update", "-p", spec, "--precise", version,
             "--manifest-path", VENDOR_MANIFEST])

    sa = _load_supply_audit()
    root = {}
    vendor = {}
    for store, rel in ((root, "Cargo.lock"), (vendor, VENDOR_LOCK)):
        for n, v in sa.parse_lock(REPO / rel):
            store.setdefault(n, set()).add(v)

    bad = 0
    for name, _cur, _version in eligible:
        rv, vv = root.get(name, set()), vendor.get(name)
        if vv is None:
            print(f"  ○ {name:<20} vendor 锁里没有这个包 —— 无从对不上")
        elif rv == vv:
            print(f"  ✓ {name:<20} 两把锁一致：{' '.join(sorted(rv))}")
        else:
            bad += 1
            print(f"  ✗ {name:<20} 根 {' '.join(sorted(rv))} ≠ vendor {' '.join(sorted(vv))}")
    if bad:
        print(f"  ⚠ {bad} 个包两把锁对不上 —— tests/vendor/run.sh 会拒绝跑回归网。")
    return bad


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
