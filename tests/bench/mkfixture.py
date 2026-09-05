#!/usr/bin/env python3
"""造一份**合成的**对拍输出目录，给 `bench/verdict.sh` 的反证用。

    python3 tests/bench/mkfixture.py <目录> <qualified: true|false> <名字=rps> ...

★ ★ ★ 它存在的全部理由是一句话：**一个永远拒绝出结论的判定器，与一个坏掉的
  判定器给出完全相同的输出。** 本轮唯一跑得到的宿主是不合格的那一台 ⇒
  「合格」那条分支在真实数据上一次都执行不到，必须用合成输入把它走一遍。

⚠ ⛔ 它写的是**测试夹具**，不是对拍产物。真实产物由 `bench/env-snapshot.sh` 与
  `bench/case/*.sh` 写出来。⇒ 夹具里的数字是编的，⛔ 任何情况下都不许被引用。
"""

import json
import pathlib
import sys


def oha_like(rps: float) -> dict:
    """长得像 oha 输出的最小结构：只带 `read-raw.py` 真的会读的那几个字段。

    ⚠ 有意**不**去复刻 oha 的全部字段：夹具越像真的，它越容易在 oha 换版本时
      悄悄失真。这里只钉判据读的那几格，其余留给真实数据。
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
        # ⚠ 不合格时必须有理由：一个 `qualified: false` 配空理由列表的快照是坏的，
        #   而 verdict 打出来的「不合格的理由：」下面会是空的 —— 那正是要能看见的。
        "disqualifiers": [] if qualified else ["fixture: 这是一份合成的不合格快照"],
        "attest": "FIXTURE · 这不是真实宿主" if qualified else None,
        "host": {"kernel": "fixture", "nproc": 16},
        "subjects": {"fulcrum": "FIXTURE"},
    }
    (out / "env.json").write_text(json.dumps(env, indent=2, ensure_ascii=False) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
