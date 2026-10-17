#!/usr/bin/env python3
"""造一份**合成的**對拍輸出目錄，給 `bench/verdict.sh` 的反證用。

    python3 tests/bench/mkfixture.py <目录> <qualified: true|false> <名字=rps> ...

★ ★ ★ 它存在的全部理由是一句話：**一個永遠拒絕出結論的判定器，與一個壞掉的
  判定器給出完全相同的輸出。** 本輪唯一跑得到的宿主是不合格的那一臺 ⇒
  「合格」那條分支在真實資料上一次都執行不到，必須用合成輸入把它走一遍。

⚠ ⛔ 它寫的是**測試夾具**，不是對拍產物。真實產物由 `bench/env-snapshot.sh` 與
  `bench/case/*.sh` 寫出來。⇒ 夾具裡的數字是編的，⛔ 任何情況下都不許被引用。
"""

import json
import pathlib
import sys


def oha_like(rps: float) -> dict:
    """長得像 oha 輸出的最小結構：只帶 `read-raw.py` 真的會讀的那幾個欄位。

    ⚠ 有意**不**去復刻 oha 的全部欄位：夾具越像真的，它越容易在 oha 換版本時
      悄悄失真。這裡只釘判據讀的那幾格，其餘留給真實資料。
    """
    return {
        "summary": {
            "successRate": 1.0,
            "requestsPerSec": rps,
            "sizePerRequest": 4096,
        },
        "statusCodeDistribution": {"200": 12345},
        "errorDistribution": {},
    }


def main() -> int:
    if len(sys.argv) < 4:
        print(
            "用法：python3 tests/bench/mkfixture.py <目录> <true|false> <名字=rps> ...",
            file=sys.stderr,
        )
        return 2

    out = pathlib.Path(sys.argv[1])
    qualified = sys.argv[2] == "true"
    subjects = sys.argv[3:]

    raw = out / "raw" / "synthetic"
    raw.mkdir(parents=True, exist_ok=True)

    for spec in subjects:
        name, _, rps = spec.partition("=")
        (raw / f"{name}.json").write_text(json.dumps(oha_like(float(rps))) + "\n")

    env = {
        "schema": "fulcrum-bench-env/1",
        "qualified": qualified,
        # ⚠ 不合格時必須有理由：一個 `qualified: false` 配空理由列表的快照是壞的，
        #   而 verdict 打出來的「不合格的理由：」下面會是空的 —— 那正是要能看見的。
        "disqualifiers": [] if qualified else ["fixture: 这是一份合成的不合格快照"],
        "attest": "FIXTURE · 这不是真实宿主" if qualified else None,
        "host": {"kernel": "fixture", "nproc": 16},
        "subjects": {"fulcrum": "FIXTURE"},
    }
    (out / "env.json").write_text(json.dumps(env, indent=2, ensure_ascii=False) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
