#!/usr/bin/env python3
"""把 `bench/env-snapshot.sh` 采到的读数序列化成 JSON。

★ 单独一个文件而不是内联：内联的 python 要穿过 bash 的引号层，而
  **本仓的纪律是含反斜杠或反引号的内容一律写成文件** —— 内联那条路不报错，
  它只是安静地把某个转义吃掉一层，产出一份看起来正常的坏文件。

⛔ 本文件不做任何判断，只做序列化。合格性判据在 `bench/lib.sh`。
"""

import json
import os
import pathlib


def env(name: str) -> str:
    return os.environ.get(name, "")


def maybe_int(raw: str):
    """读不出来就是 None，⛔ 不给 0 —— 0 会被下游当成一个真读数。"""
    try:
        return int(raw)
    except (TypeError, ValueError):
        return None


def read_sysctls(names: str) -> dict:
    out = {}
    for name in names.split():
        path = pathlib.Path("/proc/sys") / name.replace(".", "/")
        try:
            out[name] = path.read_text().strip()
        except OSError:
            # ★ 读不到就如实记 None，⛔ 不省略这一项：
            #   「字段不在」与「读不到」在下游是两件事。
            out[name] = None
    return out


disqualifiers = [line for line in env("SNAP_DISQ").splitlines() if line.strip()]

snapshot = {
    "schema": "fulcrum-bench-env/1",
    "qualified": env("SNAP_QUALIFIED") == "true",
    "disqualifiers": disqualifiers,
    "attest": env("SNAP_ATTEST") or None,
    "thresholds": {
        "min_cpus": maybe_int(env("SNAP_MIN_CPUS")),
        "max_idle_load": env("SNAP_MAX_LOAD") or None,
    },
    "host": {
        "container_hostname": env("SNAP_HOST") or None,
        "kernel": env("SNAP_KERNEL") or None,
        "nproc": maybe_int(env("SNAP_NPROC")),
        "loadavg_1m": env("SNAP_LOAD1") or None,
        "cpu_model": env("SNAP_CPU") or None,
        "mem_total_kb": maybe_int(env("SNAP_MEM_KB")),
        # 空字符串表示**没有**把被测与负载生成器钉到不相交的核上。
        "cpu_affinity": env("SNAP_AFFINITY") or None,
    },
    # ⚠ 容器有自己的 netns ⇒ 这些多半是**容器的**值，不是宿主的。
    #   记它们是为了可追溯，⛔ 它们不参与合格性判定（见 env-snapshot.sh 那段注释）。
    "sysctl_as_seen_in_container": read_sysctls(env("SNAP_SYSCTLS")),
    "subjects": {
        "fulcrum": env("SNAP_FULCRUM") or None,
        "pinned_in_image": [
            line for line in env("SNAP_SUBJECTS").splitlines() if line.strip()
        ],
    },
    "load_params": {
        "duration": env("SNAP_DURATION") or None,
        "connections": maybe_int(env("SNAP_CONNECTIONS")),
        "workers": maybe_int(env("SNAP_WORKERS")),
        "payload_bytes": maybe_int(env("SNAP_PAYLOAD_BYTES")),
    },
}

out_path = pathlib.Path(env("SNAP_OUT"))
out_path.parent.mkdir(parents=True, exist_ok=True)
out_path.write_text(json.dumps(snapshot, indent=2, ensure_ascii=False) + "\n")
