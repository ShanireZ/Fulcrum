#!/usr/bin/env python3
"""供应链全量审计 —— 刷新 docs/platform/supply-chain.md 的那组数字。

★ **它和 `dep-check.py` 是两件不同的事，不要互相替代。**

`dep-check.py` 只报告 `cargo update` 能动的东西，而 `cargo update` **只在现有版本需求范围内
移动**。任何被上游清单的上界卡住的依赖，它一个都不会报——的实测里，
`cargo update --dry-run` 一直说「无可用更新」，而当时有 44 个包落后、两条真漏洞在库里。

本脚本做的是 `dep-check.py` 结构上做不到的两件事：

  1. **逐包对比 crates.io 的 `max_stable_version`** —— 找出陈旧包，不管它为什么陈旧
  2. **对 `Cargo.lock` 全量查 OSV** —— 找出安全公告

## ★ 两条判据来自 supply-chain.md 记下的教训

**① 必须分桶。** 「在 lock 里」「在编译图里」「能不能升」是三件不同的事，混在一起就会得出
吓人但无意义的数字——44 个陈旧包里有 17 个在目标平台上一行代码都不会被编译。
传 `--cargo` 就能拿到编译图（`cargo tree --target ... -e normal`），据此分桶；不传则只报 lock 层面。

**② 陈旧不判红，未登记的公告才判红。** 目标构建里长期有 8–10 个升不动的陈旧包，成因全在
第三方 crate 那一层（它们本身已是最新版）。把陈旧算作失败会让这道门**永远红**，
而永远亮着的告警等于没有告警。已登记并接受的公告同理——它们照常打印，但不改退出码。

用法：
    python tools/supply-audit.py
    python tools/supply-audit.py --cargo "docker run --rm -v ...:/w -w /w fulcrum-build:local cargo"
    python tools/supply-audit.py --lock vendor/pingora/Cargo.lock
    python tools/supply-audit.py --markdown        # 输出可直接贴进 supply-chain.md 的表

退出码（★ 会叠加，风格同 dep-check.py）：
    0   没有未登记的安全公告，且两侧的查证覆盖率都达标
    20  ★ 有未登记的安全公告
    40  ★ 查证覆盖率不足（超过 --max-unresolved-pct，默认 10%）——**版本侧或公告侧任一**
    60  两者都有
    1   出错

★ 40 这一档来自复审的第 3 条：`unresolved` 原先**只影响打印、不影响退出码**，
  于是 crates.io 整体不可达时，脚本会打一行警告然后照常 exit 0——只看退出码的 cron / CI
  会把一次「什么都没查到」记成一次通过。

★ ★ 把同一条纪律补到了**公告那一侧**。此前 `osv_batch` 没有任何错误处理：
  OSV 不可达就直接抛异常（吵，但给不出诊断），而 `zip(chunk, results)` 在 OSV 少回几条时
  会**静默地**把尾巴上那批包当成「查过且干净」。现在两侧用同一个阈值，且**公告没查全时
  绝不打印「没有未登记的安全公告」**——那会是一句有依据感的假话。
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

# Windows 控制台默认不是 UTF-8，中文输出会变乱码。强制一下。
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8")
    except (AttributeError, ValueError):
        pass

REPO = Path(__file__).resolve().parent.parent
UA = "fulcrum-supply-audit (https://github.com/ShanireZ/Fulcrum)"
DEFAULT_TARGET = "x86_64-unknown-linux-gnu"

# ★ 已登记并接受的公告：照常打印，但不改退出码。
#   每一条都必须写清「为什么接受」和「出路在哪」——没有理由的条目等于偷偷降噪。
ACCEPTED: dict[str, str] = {
    "RUSTSEC-2025-0069": (
        "daemonize 失维、无 CVE、无可升版本。它做的是特权丢弃，手写替代违反安全基线；"
        "★ 出路是 G31——systemd Type=notify 前台运行后 conf.daemon 恒 false，"
        "该依赖整个可以删掉。见 vendor/pingora/FORK.md 第 4 节。"
    ),
}

# Cargo.lock 的 [[package]] 块
PKG_BLOCK = re.compile(r"\[\[package\]\]\n(.*?)(?=\n\[\[package\]\]|\n\[metadata|\Z)", re.S)
FIELD = {k: re.compile(rf'^{k} = "(.*)"$', re.M) for k in ("name", "version", "source")}

# cargo tree --prefix none 的输出形如 "tokio v1.53.1" 或 "tokio v1.53.1 (*)"
TREE_LINE = re.compile(r"^(\S+) v(\S+)")


def parse_lock(path: Path) -> list[tuple[str, str]]:
    """返回 lock 里所有**来自 registry** 的 (name, version)。

    ★ 没有 `source` 字段的是本地 path 包（工作区成员、vendor 的 fork），
      它们不在 crates.io 上，查版本和公告都没有意义。
    """
    text = path.read_text(encoding="utf-8")
    out: list[tuple[str, str]] = []
    for block in PKG_BLOCK.findall(text):
        src = FIELD["source"].search(block)
        if not src or not src.group(1).startswith("registry+"):
            continue
        name = FIELD["name"].search(block)
        ver = FIELD["version"].search(block)
        if name and ver:
            out.append((name.group(1), ver.group(1)))
    return sorted(set(out))


def compiled_set(cargo: list[str], target: str) -> set[tuple[str, str]] | None:
    """用 `cargo tree` 拿到**真正参与编译**的包集合。拿不到就返回 None（降级为只报 lock 层）。"""
    proc = subprocess.run(
        [*cargo, "tree", "--target", target, "-e", "normal", "--workspace", "--prefix", "none"],
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        print(f"  ⚠ cargo tree 失败（退出码 {proc.returncode}），跳过分桶", file=sys.stderr)
        if proc.stderr:
            print("  " + proc.stderr.strip().splitlines()[0], file=sys.stderr)
        return None
    found: set[tuple[str, str]] = set()
    for line in proc.stdout.splitlines():
        m = TREE_LINE.match(line.strip())
        if m:
            found.add((m.group(1), m.group(2)))
    return found or None


def max_stable(name: str) -> tuple[str | None, str | None]:
    """返回 (最新稳定版, 失败原因)。**两者必有其一**。

    ★ 绝不把「查不到」和「已是最新」混成同一个返回值——本页的数字是要贴回
      supply-chain.md 当依据的，一次静默的网络抖动会让某个陈旧包凭空消失。
    """
    url = f"https://crates.io/api/v1/crates/{name}"
    # ★ 有界重试。原先 429 时是 `return max_stable(name)` 的无限递归：
    #   本脚本要连着请求 crates.io 一百多次，被限流是常态，持续 429 会一路递归到
    #   RecursionError 崩掉，而不是给出「被限流了」这个真正的诊断。
    attempts = 3
    for attempt in range(1, attempts + 1):
        req = urllib.request.Request(url, headers={"User-Agent": UA})
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                data = json.load(resp)
        except urllib.error.HTTPError as e:
            if e.code == 429 and attempt < attempts:
                time.sleep(5 * attempt)  # 退避
                continue
            return None, f"HTTP {e.code}" + ("（被限流，已重试 3 次）" if e.code == 429 else "")
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as e:
            if attempt < attempts:
                time.sleep(2 * attempt)
                continue
            return None, f"{type(e).__name__}: {e}"
        latest = (data.get("crate") or {}).get("max_stable_version")
        if latest is None:
            return None, "crates.io 没给 max_stable_version（只有预发布版？）"
        return latest, None
    return None, "重试耗尽"


def osv_batch(
    pkgs: list[tuple[str, str]],
) -> tuple[dict[tuple[str, str], list[str]], list[tuple[tuple[str, str], str]]]:
    """对全量包查 OSV。

    返回 `(hits, unqueried)`：
      - `hits`     = {(name, version): [advisory_id, ...]}，只含有命中的
      - `unqueried` = [((name, version), 原因)]，**本次根本没查成的包**

    ★ 为什么必须把「没查成」单独返回，而不是让它混进「没命中」：
      这两者在数据结构上长得一模一样（都是 `hits` 里没有那个键），但含义相反——
      一个是「查过，干净」，一个是「不知道」。原先本函数遇到网络错误直接抛异常，
      虽然吵（退出码 1），却给不出「OSV 没查成」这个诊断；而一旦有人图省事在外面
      裹一层 `try/except` 当成空结果，整份审计就会在**一条公告都没查过**的情况下
      输出「没有未登记的安全公告」。这正是复审那批缺陷的形状。
    """
    hits: dict[tuple[str, str], list[str]] = {}
    unqueried: list[tuple[tuple[str, str], str]] = []
    CHUNK = 500  # querybatch 一次别塞太多
    for i in range(0, len(pkgs), CHUNK):
        chunk = pkgs[i : i + CHUNK]
        body = {
            "queries": [
                {"package": {"name": n, "ecosystem": "crates.io"}, "version": v} for n, v in chunk
            ]
        }
        results = None
        attempts = 3
        for attempt in range(1, attempts + 1):
            req = urllib.request.Request(
                "https://api.osv.dev/v1/querybatch",
                data=json.dumps(body).encode(),
                headers={"Content-Type": "application/json", "User-Agent": UA},
            )
            try:
                with urllib.request.urlopen(req, timeout=120) as resp:
                    results = json.load(resp)["results"]
                break
            except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError,
                    json.JSONDecodeError, KeyError) as e:
                why = f"{type(e).__name__}: {e}"
                if attempt < attempts:
                    time.sleep(3 * attempt)  # 退避
                    continue
                for pkg in chunk:
                    unqueried.append((pkg, why))
        if results is None:
            continue

        # ★ `zip` 会在短的那一边**静默停下**。OSV 正常是等长返回，但「正常」不是判据——
        #   真短了的话，尾巴上那批包会一声不响地变成「查过且干净」。宁可显式记成没查成。
        if len(results) != len(chunk):
            for pkg in chunk[len(results) :]:
                unqueried.append((pkg, f"OSV 只回了 {len(results)}/{len(chunk)} 条结果"))
        for (n, v), res in zip(chunk, results):
            ids = [x["id"] for x in (res.get("vulns") or [])]
            # ★ `querybatch` 单条结果的 vuln 列表可能被截断，此时带 `next_page_token`。
            #   本脚本只关心「有没有未登记的公告」，而截断后的列表**只会漏、不会多**——
            #   漏掉的那条恰好是未登记的，`unregistered` 就少算了。
            #   实测：单包 16 条公告仍是完整返回、不带 token，所以现实中很少触发；
            #   但「很少触发」不是「不会触发」，触发了就把这个包记成没查成，别让它冒充干净。
            if res.get("next_page_token"):
                unqueried.append(((n, v), "OSV 的公告列表被分页截断（next_page_token）"))
            if ids:
                hits[(n, v)] = ids
    return hits, unqueried


def osv_detail(advisory_id: str) -> dict:
    req = urllib.request.Request(
        f"https://api.osv.dev/v1/vulns/{advisory_id}", headers={"User-Agent": UA}
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.load(resp)
    except urllib.error.URLError:
        return {}


def bucket(pkg: tuple[str, str], compiled: set[tuple[str, str]] | None) -> str:
    if compiled is None:
        return "?"
    return "编译" if pkg in compiled else "仅 lock"


def main() -> int:
    ap = argparse.ArgumentParser(description="Fulcrum 供应链全量审计（陈旧包 + OSV 公告）")
    ap.add_argument("--lock", default="Cargo.lock", help="要审计的 Cargo.lock（默认仓库根的那份）")
    ap.add_argument(
        "--cargo",
        default=None,
        help="cargo 命令；给了才能分桶（区分「参与编译」与「仅在 lock 里」）。"
        "在容器里跑时可传 'docker run ... cargo' 之类的完整前缀（空格分隔）",
    )
    ap.add_argument("--target", default=DEFAULT_TARGET, help=f"分桶用的目标三元组（默认 {DEFAULT_TARGET}）")
    ap.add_argument("--markdown", action="store_true", help="额外输出可贴进 supply-chain.md 的表")
    ap.add_argument(
        "--max-unresolved-pct",
        type=float,
        default=10.0,
        metavar="PCT",
        help="未能查证的包占比超过它就判红（默认 10）。★ 阈值卡的是比例不是绝对值："
        "被 crates.io 限流是常态，零星查不到不该拦人，而**整体查不到**必须拦",
    )
    args = ap.parse_args()

    lock_path = (REPO / args.lock) if not Path(args.lock).is_absolute() else Path(args.lock)
    if not lock_path.exists():
        print(f"找不到 {lock_path}", file=sys.stderr)
        return 1

    pkgs = parse_lock(lock_path)
    print(f"── {lock_path.relative_to(REPO) if lock_path.is_relative_to(REPO) else lock_path} ──")
    print(f"registry 包总数：{len(pkgs)}")

    compiled = None
    if args.cargo:
        print("\n── 取编译图（用于分桶）──")
        compiled = compiled_set(args.cargo.split(), args.target)
        if compiled:
            print(f"  {args.target} 上真正参与编译：{len(compiled)} 个")
    else:
        print("  ⚠ 未传 --cargo，无法分桶；下面的数字是 **lock 层面**的，会偏大")

    # ── 安全公告 ────────────────────────────────────────────────────────────
    print("\n── OSV 全量查询 ──")
    hits, osv_unqueried = osv_batch(pkgs)
    osv_unqueried_pct = (100.0 * len(osv_unqueried) / len(pkgs)) if pkgs else 0.0
    if osv_unqueried:
        whys = sorted({why for _, why in osv_unqueried})
        print(f"  ⚠ {len(osv_unqueried)} 个包**没能查证公告**（占 {osv_unqueried_pct:.1f}%）："
              + "；".join(whys[:3]))
        print("  ★ 这一栏非空时，「零公告」与「没有未登记的公告」都**不成立**——没查过不等于干净。")
    unregistered = 0
    seen_accepted: set[str] = set()  # ★ 反向校验用：这次真正命中过的已登记条目
    if not hits:
        print("  查到的包里零公告" if osv_unqueried else "  零公告")
    for pkg, ids in sorted(hits.items()):
        for aid in ids:
            detail = osv_detail(aid)
            summary = detail.get("summary") or "（无摘要）"
            mark = "○ 已登记" if aid in ACCEPTED else "★ 未登记"
            if aid in ACCEPTED:
                seen_accepted.add(aid)
            else:
                unregistered += 1
            print(f"  {mark}  {pkg[0]} {pkg[1]}  [{bucket(pkg, compiled)}]  {aid}")
            print(f"           {summary}")
            if aid in ACCEPTED:
                print(f"           接受理由：{ACCEPTED[aid]}")

    # ★ 反向校验忽略名单：只查「命中时是否已登记」是不够的，还要查
    #   「已登记的条目是不是还命中得上」。否则依赖被删掉之后条目会静默留存，
    #   将来某个依赖把它拉回来时这道门会直接放行。
    #   （同一条教训当初登记为 **D10**：过期的声明比过期的状态更危险。
    #     ⚠ 那一条已经结案，不在 `PLAN.md` §11 里了。）
    # ★ 但这条反向校验**只在公告确实查过时才成立**：一次 OSV 失败会让所有 ACCEPTED 条目
    #   看起来都「没命中」，从而建议把它们全删掉——那是把「没查到」当成「不存在」，
    #   方向正好和这条校验想防的错误相反。
    dead = sorted(set(ACCEPTED) - seen_accepted) if not osv_unqueried else []
    if osv_unqueried and ACCEPTED:
        print(f"\n（本次有包没查成公告，跳过 ACCEPTED 的反向校验——"
              f"否则 {len(ACCEPTED)} 条会因为「没查到」被误判成「该删」）")
    if dead:
        print(f"\n★ ACCEPTED 里有 {len(dead)} 条本次**没有命中任何包**，应当删掉：")
        for aid in dead:
            print(f"  · {aid}")
        print("  留着它们等于给未来埋一个「静默放行」的口子。")

    # ── 陈旧包 ──────────────────────────────────────────────────────────────
    print("\n── 逐包对比 crates.io 的 max_stable_version ──")
    stale: list[tuple[str, str, str, str]] = []
    unresolved: list[tuple[str, str]] = []  # ★ 查不到的单列一类，绝不当成「已是最新」
    names = sorted({n for n, _ in pkgs})
    by_name: dict[str, list[str]] = {}
    for n, v in pkgs:
        by_name.setdefault(n, []).append(v)
    for i, name in enumerate(names, 1):
        latest, why = max_stable(name)
        if latest is None:
            unresolved.append((name, why or "未知原因"))
        else:
            for v in by_name[name]:
                if v != latest:
                    stale.append((name, v, latest, bucket((name, v), compiled)))
        if i % 40 == 0:
            print(f"  …已查 {i}/{len(names)}")

    in_build = [s for s in stale if s[3] == "编译"]
    lock_only = [s for s in stale if s[3] == "仅 lock"]
    print(f"\n陈旧包：{len(stale)} 个", end="")
    if compiled is not None:
        print(f"（★ 其中真正参与编译 {len(in_build)} 个，仅存在于 lock 里 {len(lock_only)} 个）")
    else:
        print("（未分桶）")
    for n, v, latest, b in sorted(in_build or stale):
        print(f"  {n} {v} → {latest}  [{b}]")
    if compiled is not None and lock_only:
        print(f"  （另有 {len(lock_only)} 个仅在 lock 里，不进产物，已折叠）")

    # ★ 查不到的必须显式报出来。它们既不是「陈旧」也不是「最新」，而是**没查证**；
    #   混进任何一边都会让贴回文档的数字失真。
    unresolved_pct = (100.0 * len(unresolved) / len(names)) if names else 0.0
    if unresolved:
        print(f"\n⚠ {len(unresolved)} 个包**未能查证**（不计入上面的陈旧数）"
              f"，占 {unresolved_pct:.1f}%：")
        for name, why in unresolved:
            print(f"  ? {name}  —— {why}")
        print("  ★ 这一栏非空时，本次的陈旧包数量是**下界**，不是实测值。")

    if args.markdown:
        print("\n── 可贴进 supply-chain.md 的表 ──\n")
        print("| | 数值 |")
        print("|---|---|")
        print(f"| 锁定包总数（registry） | {len(pkgs)} |")
        if compiled is not None:
            print(f"| 陈旧包（参与编译） | {len(in_build)} |")
            print(f"| 陈旧包（仅 lock） | {len(lock_only)} |")
        else:
            print(f"| 陈旧包（未分桶） | {len(stale)} |")
        if unresolved:
            print(f"| ⚠ 未能查证版本 | {len(unresolved)}（上面的陈旧数是下界）|")
        print(f"| 安全公告 | {sum(len(v) for v in hits.values())} |")
        print(f"| ★ 其中未登记 | {unregistered} |")
        if osv_unqueried:
            print(f"| ⚠ 未能查证公告 | {len(osv_unqueried)}（上面的公告数是下界）|")

    print()
    rc = 0
    if unregistered:
        print(f"★ {unregistered} 条**未登记**的安全公告——要么修，要么写进本脚本的 ACCEPTED 并说明理由。")
        rc += 20
    elif osv_unqueried:
        # ★ 不能说「没有未登记的安全公告」——有包根本没查过，这句话会是一句**有依据感的假话**。
        print(f"★ 在查成的那部分里没有未登记的公告；但有 {len(osv_unqueried)} 个包没查成，结论不完整。")
    else:
        print("没有未登记的安全公告。")

    # ★ ★ 「什么都没查到」不许记成绿。
    #   crates.io 整体不可达时（公司网络、DNS、限流），全部包都会落进 unresolved，
    #   而这一栏原先**只影响打印、不影响退出码**——每周的 cron / CI 只看退出码，
    #   于是一次**什么都没查证**的运行会被记成一次通过。这正是本项目复审两轮
    #   反复抓到的那个形状：把「没能检查」当成「检查通过」。
    #
    #   ★ 但它不能变成常红：被限流是常态，零星几个包查不到不该拦人。所以卡的是**比例**，
    #     默认留 10% 的余量，可用 --max-unresolved-pct 调整。
    #   ★ 同一条纪律作用在两侧：**版本**查不到（crates.io）与**公告**查不到（OSV）
    #     都会让本次结论变成下界，都用同一个阈值卡。
    short = []
    if unresolved_pct > args.max_unresolved_pct:
        short.append(f"版本：{len(unresolved)}/{len(names)} 个包未查证（{unresolved_pct:.1f}%）"
                     "→ 陈旧包数量是下界")
    if osv_unqueried_pct > args.max_unresolved_pct:
        short.append(f"公告：{len(osv_unqueried)}/{len(pkgs)} 个包未查证（{osv_unqueried_pct:.1f}%）"
                     "→ 公告数量是下界")
    if short:
        print(f"★ 查证覆盖率不足（阈值 {args.max_unresolved_pct:.1f}%）：")
        for s in short:
            print(f"  · {s}")
        print("  **不要**把本次的数字贴回 supply-chain.md。")
        print("  多半是网络／DNS／限流问题；换网络重跑，或确认这是常态后调 --max-unresolved-pct。")
        rc += 40
    return rc


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
