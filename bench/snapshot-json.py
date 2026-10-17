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
            # ★ 讀不到就如實記 None，⛔ 不省略這一項：
            #   「欄位不在」與「讀不到」在下游是兩件事。
            out[name] = None
    return out


def parse_cpu_list(spec: str):
    """`0-1` / `0,2` / `2-3,6` → 逻辑 CPU 号的列表；写法不认识回 None（⛔ 不猜）。"""
    out = []
    for part in spec.split(","):
        part = part.strip()
        a, dash, b = part.partition("-")
        if dash:
            if not (a.isdigit() and b.isdigit()) or int(a) > int(b):
                return None
            out.extend(range(int(a), int(b) + 1))
        elif part.isdigit():
            out.append(int(part))
        else:
            return None
    return out


def cpu_topology(siblings_raw: str, server: str, load: str):
    """`<cpu>=<thread_siblings_list>` 一行一条 → `host.cpu_topology`（2026-10-05，诊断、⛔ 不判）。

    物理核用它的超线程兄弟集合当名字（`/sys` 原样的写法，如 `0-1`）。
    ★ `shared_cores` 非空 = 被测与压测端**共用物理核**（超线程兄弟互抢执行单元）。
    ⚠ 一行都读不到 ⇒ None（「没记到」，⛔ 不是「没有超线程」）；
      亲和没给、写法不认识、或指到不存在的 CPU ⇒ 那一组记 None（⛔ 不猜）。
    """
    siblings = {}
    for line in siblings_raw.splitlines():
        cpu, sep, sib = line.partition("=")
        if sep and cpu.strip().isdigit() and sib.strip():
            siblings[int(cpu.strip())] = sib.strip()
    if not siblings:
        return None

    def cores(spec):
        cpus = parse_cpu_list(spec) if spec else None
        if cpus is None or any(c not in siblings for c in cpus):
            return None
        return sorted({siblings[c] for c in cpus}, key=lambda s: parse_cpu_list(s) or [])

    s, ld = cores(server), cores(load)
    return {
        "thread_siblings": {str(c): siblings[c] for c in sorted(siblings)},
        "server_cores": s,
        "load_cores": ld,
        "shared_cores": None if s is None or ld is None else [c for c in s if c in ld],
    }


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
        # 空字串表示**沒有**把被測與負載生成器釘到不相交的核上。
        "cpu_affinity": env("SNAP_AFFINITY") or None,
        # 上面那兩組落在哪幾個**物理核**上、有沒有共用（見 `cpu_topology` 的文件註解）。
        "cpu_topology": cpu_topology(
            env("SNAP_CPU_SIBLINGS"), env("SNAP_SERVER_CPUS"), env("SNAP_LOAD_CPUS")
        ),
    },
    # ⚠ 容器有自己的 netns ⇒ 這些多半是**容器的**值，不是宿主的。
    #   記它們是為了可追溯，⛔ 它們不參與合格性判定（見 env-snapshot.sh 那段註解）。
    "sysctl_as_seen_in_container": read_sysctls(env("SNAP_SYSCTLS")),
    # ── 核心參數與資源上限（G145）────────────────────────────────────────
    # ★ 記的是「宣告了什麼」與「實測是什麼」**兩份**，⛔ 不是隻記實測：
    #   第三方要能自己重算那次比較，而只有實測值的話，一次不符與一次改了宣告
    #   在快照里長得一模一樣。
    "kernel_params": {
        # 容器側四個鍵 —— 由 `docker run --sysctl` 設，由 env-snapshot 斷言。
        "declared_in_container": [
            line for line in env("SNAP_DECLARED_CONTAINER").splitlines() if line.strip()
        ],
        # 宿主側一個鍵 —— ⛔ 容器讀不到，由 `bench/docker-run.sh` 在宿主上讀了傳進來。
        "declared_on_host": [
            line for line in env("SNAP_DECLARED_HOST").splitlines() if line.strip()
        ],
        "host_as_reported_by_launcher": [
            line for line in env("SNAP_HOST_KV").splitlines() if line.strip()
        ],
        # ⚠ 會咬的是這個，不是 `fs.file-max`（容器預設實測只有 1024）。
        "nofile_declared": maybe_int(env("SNAP_NOFILE_DECLARED")),
        "nofile_observed": maybe_int(env("SNAP_NOFILE")),
    },
    # ── 站台根落在哪種檔案系統上 ──────────────────────────────────────────
    #
    # ★ ★ ★ **這是口徑，⛔ 不是中繼資料。** 樞衡的靜態檔案路徑先在本執行緒上試一次
    #   `preadv2(RWF_NOWAIT)`，而認不認由檔案系統各自決定（實測 ext4 認、
    #   overlayfs 與 tmpfs 回 `EOPNOTSUPP`）⇒ 同一個二進位、同一臺機器，
    #   站台根換一種檔案系統，靜態吞吐差 45%。
    # ⚠ ⚠ **兩個欄位要一起讀**：一個恆回傳 `no` 的探測與「真的跑在 overlayfs 上」
    #   輸出完全相同 ⇒ `fs=ext4` 配 `rwf_nowait=no` 是看得出來不對勁的組合。
    # ⛔ `unknown` 是「探不了」，**不是**「不認」——兩件事必須分得開。
    "site_root": {
        "path": env("SNAP_SITE_ROOT") or None,
        "fs": env("SNAP_SITE_FS") or None,
        "rwf_nowait": env("SNAP_SITE_NOWAIT") or None,
    },
    # ★ 與 site_root **同形同寫法**（G150 ②）：`diag/cache-hit-p99` 量的是
    #   「磁碟命中 p99 − 記憶體命中 p99」，而磁碟那一半直接由檔案系統決定
    #   ⇒ 一份說不出自己快取落在哪種檔案系統上的差值**重現不出來**（G19 / G147）。
    # ⚠ ⚠ 上面那條「兩個欄位要一起讀」在這裡**一字不改地成立**。
    # ⛔ `unknown` 是「探不了」，**不是**「不認」。
    "cache_root": {
        "path": env("SNAP_CACHE_ROOT") or None,
        "fs": env("SNAP_CACHE_FS") or None,
        "rwf_nowait": env("SNAP_CACHE_NOWAIT") or None,
    },
    "subjects": {
        # ⚠ 這一項**恆為 null**：樞衡沒有 `--version` 參數。⛔ 別把它讀成
        #   「問過了，它沒有版本」—— 身分在下面那兩項裡。
        "fulcrum": env("SNAP_FULCRUM") or None,
        # ★ 量的到底是哪個二進位：sha256 是唯一可靠的答案，建置身分把它映射回一次提交。
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
