#!/usr/bin/env python3
"""从 oha 的原始 JSON 里读出一类用例的读数，**并先证明那份读数算数**。

    python3 bench/read-raw.py <raw/某一类的目录>

每个被测打一行：

    <名字> <requestsPerSec>          读数有效
    <名字> INVALID <理由>            读数无效，⛔ 不许参与比较

★ ★ ★ **为什么校验必须在这里而不是在判定里**：一个把 5xx 飞快地回出去的被测，
  `requestsPerSec` 会非常好看。⇒ 「跑得快」这件事只有在「回的确实是那个资源」
  成立之后才有意义，而后者是一个**关于原始数据本身**的判断，不是关于比较的。
  ⚠ 用例脚本在开跑**前**已经核过一次（状态码 + 字节数），这里核的是**负载期间**
  有没有中途变坏 —— 两次核的不是同一段时间，⛔ 别把其中一次当成多余的。

⛔ 本文件不比较、不排名、不判定。
"""

import json
import pathlib
import sys


def read_one(path: pathlib.Path):
    """返回 (rps, None) 或 (None, 无效的理由)。"""
    try:
        data = json.loads(path.read_text())
    except (OSError, ValueError) as exc:
        return None, f"读不出JSON({exc.__class__.__name__})"

    summary = data.get("summary")
    if not isinstance(summary, dict):
        return None, "没有summary段"

    rps = summary.get("requestsPerSec")
    if not isinstance(rps, (int, float)):
        return None, "summary.requestsPerSec不是数字"

    # 成功率必须是 1.0。⚠ 用 `!= 1.0` 而不是「大于某个阈值」：
    #   本类的口径里**一个失败请求都不许有**，与 tests/stress 那条「零错误」同源。
    rate = summary.get("successRate")
    if rate != 1.0:
        return None, f"successRate={rate}(该是1.0)"

    # 状态码必须**只有** 200。⚠ ⚠ 判「只有」，⛔ 不是判「有」：
    #   `{"200": 5, "503": 99999}` 里 200 是有的，而那份读数量的是 503 有多快。
    dist = data.get("statusCodeDistribution")
    if not isinstance(dist, dict) or not dist:
        return None, "没有statusCodeDistribution"
    non200 = {k: v for k, v in dist.items() if k != "200"}
    if non200:
        return None, f"出现了非200的状态码:{non200}"

    # ── 传输层错误 ─────────────────────────────────────────────────────────
    #
    # ★ ★ ★ **`aborted due to deadline` 是良性的，其余一概不是。**
    #   它是 oha 的 `-z <时长>` 到点时把**还在飞的**请求砍掉留下的记录，
    #   ⇒ 每条并发连接恰好留一条。2026-09-05 实测（nginx，2s）：
    #   `-c 10` → 10 条，`-c 50` → 50 条，占成功数的 0.018% / 0.092%。
    #   ⚠ oha 不把它们算进 `successRate`，也不算进 `requestsPerSec` 的分子
    #   ⇒ 读数本身不受影响。
    #
    # ⚠ ⚠ ⛔ **不许因此就把整条 errorDistribution 判据删掉。** 连接被拒、
    #   被重置、读超时全都落在这个字段里，而那些正是「这份读数不算数」的样子。
    #   ⇒ 收窄成「只放行这一个 key，且它的量必须小」，⛔ 不是「不看这个字段」。
    #
    # ★ 量上再加一道界：良性那一条的条数不该超过成功数的 1%。它判的是
    #   「deadline 砍掉的远多于并发数」这种反常 —— 那时候它就不再是良性的了。
    errors = dict(data.get("errorDistribution") or {})
    deadline_aborts = errors.pop("aborted due to deadline", 0)
    if errors:
        return None, f"有传输层错误:{sorted(errors)[:3]}"

    ok_count = dist.get("200", 0)
    if deadline_aborts and ok_count and deadline_aborts > ok_count * 0.01:
        return None, (
            f"deadline砍掉的请求太多:{deadline_aborts}对{ok_count}个200"
            "(超过1%,已经不是「到点收尾」那一类了)"
        )

    return rps, None


def main() -> int:
    if len(sys.argv) != 2:
        print("用法：python3 bench/read-raw.py <raw/某一类的目录>", file=sys.stderr)
        return 2
    raw_dir = pathlib.Path(sys.argv[1])
    files = sorted(raw_dir.glob("*.json"))
    if not files:
        print(f"read-raw: {raw_dir} 下一个 .json 都没有", file=sys.stderr)
        return 1
    for path in files:
        name = path.stem
        rps, why = read_one(path)
        if why is None:
            print(f"{name} {rps:.4f}")
        else:
            print(f"{name} INVALID {why}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
