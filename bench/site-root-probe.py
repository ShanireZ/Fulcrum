"""站点根落在哪种文件系统上，以及**那里认不认 `RWF_NOWAIT`**。

    python3 bench/site-root-probe.py <目录>
    → 两行 `键=值`，⛔ 自己不判红（判红是调用方的事，同 `env-snapshot.sh` 的其余读数）

## ★ 它为什么是口径的一部分，⛔ 不是一条可有可无的元数据

枢衡的静态文件路径 2026-09-06 起先在**本线程**上试一次
`open` + `fstat` + `preadv2(RWF_NOWAIT)`，读全了就整趟零跨线程。
⚠ ⚠ ★ 而 **`RWF_NOWAIT` 是每个文件系统各自决定认不认的**：同一天在内核 `6.18`
上实测 **ext4 认、overlayfs 与 tmpfs 回 `EOPNOTSUPP`**。
⇒ **同一个二进制、同一份配置、同一台机器，站点根换一种文件系统，
静态吞吐差 45%**（诊断台实测 28.6k → 44.4k）。

⛔ 那就意味着：一份不说出站点根在哪种文件系统上的静态吞吐读数，
**是复现不出来的**（G19 要求原始数据可被第三方复现）。⇒ 采进快照。

## ⚠ 两个读数互相盯着

一个坏掉的探测最典型的形态是**恒返回「不认」** —— 而那与「跑在 overlayfs 上」
输出完全相同。⇒ 本脚本**同时**报文件系统类型（取自 `/proc/self/mountinfo`，
与 `preadv2` 走的是完全不同的两条路）：
`fs=ext4` 配 `rwf_nowait=no` 是一个**看得出来不对劲**的组合，
而 `fs=overlay` 配 `rwf_nowait=no` 自洽。⛔ 两个读数一起看，别只看一个。
"""

import ctypes
import ctypes.util
import os
import sys

# uapi/linux/fs.h。⛔ 不从别处取：只要一个数，而多一层依赖多一处会漂的东西。
RWF_NOWAIT = 0x00000008


class _Iovec(ctypes.Structure):
    _fields_ = [("iov_base", ctypes.c_void_p), ("iov_len", ctypes.c_size_t)]


def fs_type(path: str) -> str:
    """`path` 所在挂载点的文件系统类型。取不到就 `unknown`，⛔ 不猜。"""
    try:
        dev = os.stat(path).st_dev
        want = f"{os.major(dev)}:{os.minor(dev)}"
        with open("/proc/self/mountinfo", encoding="utf-8") as f:
            for line in f:
                parts = line.split()
                if parts[2] == want:
                    return parts[parts.index("-") + 1]
    except (OSError, ValueError, IndexError):
        pass
    return "unknown"


def rwf_nowait_works(path: str) -> str:
    """在 `path` 下真发一次 `preadv2(RWF_NOWAIT)`。回 `yes` / `no` / `unknown`。

    ⚠ 探测文件有意**非空**：实测零长读被短路在 `FMODE_NOWAIT` 检查之前
    ⇒ 拿空文件探会把「不认」误判成「认」。
    """
    probe = os.path.join(path, ".bench-nowait-probe")
    try:
        libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
        libc.preadv2.argtypes = [
            ctypes.c_int,
            ctypes.POINTER(_Iovec),
            ctypes.c_int,
            ctypes.c_long,
            ctypes.c_int,
        ]
        libc.preadv2.restype = ctypes.c_ssize_t
        with open(probe, "wb") as f:
            f.write(b"nowait")
        fd = os.open(probe, os.O_RDONLY)
        try:
            buf = ctypes.create_string_buffer(6)
            iov = _Iovec(ctypes.cast(buf, ctypes.c_void_p), 6)
            rc = libc.preadv2(fd, ctypes.byref(iov), 1, 0, RWF_NOWAIT)
        finally:
            os.close(fd)
        return "yes" if rc == 6 and buf.raw[:6] == b"nowait" else "no"
    except (OSError, AttributeError, TypeError):
        # ⛔ 「探不了」不等于「不认」—— 两件事必须分得开。
        return "unknown"
    finally:
        try:
            os.unlink(probe)
        except OSError:
            pass


def main() -> int:
    if len(sys.argv) != 2:
        print("用法：site-root-probe.py <目录>", file=sys.stderr)
        return 2
    d = sys.argv[1]
    print(f"fs={fs_type(d)}")
    print(f"rwf_nowait={rwf_nowait_works(d)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
