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
    # ── 内核参数与资源上限（G145）────────────────────────────────────────
    # ★ 记的是「声明了什么」与「实测是什么」**两份**，⛔ 不是只记实测：
    #   第三方要能自己重算那次比较，而只有实测值的话，一次不符与一次改了声明
    #   在快照里长得一模一样。
    "kernel_params": {
        # 容器侧四个键 —— 由 `docker run --sysctl` 设，由 env-snapshot 断言。
        "declared_in_container": [
            line for line in env("SNAP_DECLARED_CONTAINER").splitlines() if line.strip()
        ],
        # 宿主侧一个键 —— ⛔ 容器读不到，由 `bench/docker-run.sh` 在宿主上读了传进来。
        "declared_on_host": [
            line for line in env("SNAP_DECLARED_HOST").splitlines() if line.strip()
        ],
        "host_as_reported_by_launcher": [
            line for line in env("SNAP_HOST_KV").splitlines() if line.strip()
        ],
        # ⚠ 会咬的是这个，不是 `fs.file-max`（容器缺省实测只有 1024）。
        "nofile_declared": maybe_int(env("SNAP_NOFILE_DECLARED")),
        "nofile_observed": maybe_int(env("SNAP_NOFILE")),
    },
    # ── 站点根落在哪种文件系统上 ──────────────────────────────────────────
    #
    # ★ ★ ★ **这是口径，⛔ 不是元数据。** 枢衡的静态文件路径先在本线程上试一次
    #   `preadv2(RWF_NOWAIT)`，而认不认由文件系统各自决定（实测 ext4 认、
    #   overlayfs 与 tmpfs 回 `EOPNOTSUPP`）⇒ 同一个二进制、同一台机器，
    #   站点根换一种文件系统，静态吞吐差 45%。
    # ⚠ ⚠ **两个字段要一起读**：一个恒返回 `no` 的探测与「真的跑在 overlayfs 上」
    #   输出完全相同 ⇒ `fs=ext4` 配 `rwf_nowait=no` 是看得出来不对劲的组合。
    # ⛔ `unknown` 是「探不了」，**不是**「不认」——两件事必须分得开。
    "site_root": {
        "path": env("SNAP_SITE_ROOT") or None,
        "fs": env("SNAP_SITE_FS") or None,
        "rwf_nowait": env("SNAP_SITE_NOWAIT") or None,
    },
    # ★ 与 site_root **同形同写法**（G150 ②）：`diag/cache-hit-p99` 量的是
    #   「磁盘命中 p99 − 内存命中 p99」，而磁盘那一半直接由文件系统决定
    #   ⇒ 一份说不出自己缓存落在哪种文件系统上的差值**复现不出来**（G19 / G147）。
    # ⚠ ⚠ 上面那条「两个字段要一起读」在这里**一字不改地成立**。
    # ⛔ `unknown` 是「探不了」，**不是**「不认」。
    "cache_root": {
        "path": env("SNAP_CACHE_ROOT") or None,
        "fs": env("SNAP_CACHE_FS") or None,
        "rwf_nowait": env("SNAP_CACHE_NOWAIT") or None,
    },
    "subjects": {
        # ⚠ 这一项**恒为 null**：枢衡没有 `--version` 参数。⛔ 别把它读成
        #   「问过了，它没有版本」—— 身份在下面那两项里。
        "fulcrum": env("SNAP_FULCRUM") or None,
        # ★ 量的到底是哪个二进制：sha256 是唯一可靠的答案，构建身份把它映射回一次提交。
        "fulcrum_sha256": env("SNAP_FULCRUM_SHA") or None,
        "fulcrum_build_id": env("SNAP_FULCRUM_BUILD_ID") or None,
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
