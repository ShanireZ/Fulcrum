#!/usr/bin/env python3
"""從 oha 的原始 JSON 裡讀出一類案例的讀數，**並先證明那份讀數算數**。

    python3 bench/read-raw.py <raw/某一类的目录>

每個被測打一行：

    <名字> <requestsPerSec>          讀數有效
    <名字> INVALID <理由>            讀數無效，⛔ 不許參與比較

★ ★ ★ **為什麼檢核必須在這裡而不是在判定裡**：一個把 5xx 飛快地回出去的被測，
  `requestsPerSec` 會非常好看。⇒ 「跑得快」這件事只有在「回的確實是那個資源」
  成立之後才有意義，而後者是一個**關於原始資料本身**的判斷，不是關於比較的。
  ⚠ 案例指令碼在開跑**前**已經核過一次（狀態碼 + 位元組數），這裡核的是**負載期間**
  有沒有中途變壞 —— 兩次核的不是同一段時間，⛔ 別把其中一次當成多餘的。

⛔ 本文件不比較、不排名、不判定。
"""

import json
import pathlib
import sys


def read_one(path: pathlib.Path):
    """回傳 (rps, None) 或 (None, 無效的理由)。"""
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

    # 成功率必須是 1.0。⚠ 用 `!= 1.0` 而不是「大於某個閾值」：
    #   本類的口徑裡**一個失敗請求都不許有**，與 tests/stress 那條「零錯誤」同源。
    rate = summary.get("successRate")
    if rate != 1.0:
        return None, f"successRate={rate}(该是1.0)"

    # 狀態碼必須**只有** 200。⚠ ⚠ 判「只有」，⛔ 不是判「有」：
    #   `{"200": 5, "503": 99999}` 裡 200 是有的，而那份讀數量的是 503 有多快。
    dist = data.get("statusCodeDistribution")
    if not isinstance(dist, dict) or not dist:
        return None, "没有statusCodeDistribution"
    non200 = {k: v for k, v in dist.items() if k != "200"}
    if non200:
        return None, f"出现了非200的状态码:{non200}"

    # ── 傳輸層錯誤 ─────────────────────────────────────────────────────────
    #
    # ★ ★ ★ **`aborted due to deadline` 是良性的，其餘一概不是。**
    #   它是 oha 的 `-z <时长>` 到點時把**還在飛的**請求砍掉留下的記錄，
    #   ⇒ 每條並行連接恰好留一條。2026-09-05 實測（nginx，2s）：
    #   `-c 10` → 10 條，`-c 50` → 50 條，佔成功數的 0.018% / 0.092%。
    #   ⚠ oha 不把它們算進 `successRate`，也不算進 `requestsPerSec` 的分子
    #   ⇒ 讀數本身不受影響。
    #
    # ⚠ ⚠ ⛔ **不許因此就把整條 errorDistribution 判據刪掉。** 連接被拒、
    #   被重置、讀超時全都落在這個欄位裡，而那些正是「這份讀數不算數」的樣子。
    #   ⇒ 收窄成「只放行這一個 key，且它的量必須小」，⛔ 不是「不看這個欄位」。
    #
    # ★ 量上再加一道界：良性那一條的條數不該超過成功數的 1%。它判的是
    #   「deadline 砍掉的遠多於並行數」這種反常 —— 那時候它就不再是良性的了。
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
