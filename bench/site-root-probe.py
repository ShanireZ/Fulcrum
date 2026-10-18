"""站台根落在哪種文件系統上，以及**那裡認不認 `RWF_NOWAIT`**。

    python3 bench/site-root-probe.py <目錄>
    → 兩行 `键=值`，⛔ 自己不判紅（判紅是呼叫方的事，同 `env-snapshot.sh` 的其餘讀數）

## ★ 它為什麼是口徑的一部分，⛔ 不是一條可有可無的中繼資料

樞衡的靜態文件路徑 2026-09-06 起先在**本執行緒**上試一次
`open` + `fstat` + `preadv2(RWF_NOWAIT)`，讀全了就整趟零跨執行緒。
⚠ ⚠ ★ 而 **`RWF_NOWAIT` 是每個文件系統各自決定認不認的**：同一天在核心 `6.18`
上實測 **ext4 認、overlayfs 與 tmpfs 回 `EOPNOTSUPP`**。
⇒ **同一個二進制、同一份設定、同一臺機器，站台根換一種文件系統，
靜態吞吐差 45%**（診斷臺實測 28.6k → 44.4k）。

⛔ 那就意味著：一份不說出站台根在哪種文件系統上的靜態吞吐讀數，
**是重現不出來的**（G19 要求原始資料可被第三方重現）。⇒ 採進快照。

## ⚠ 兩個讀數互相盯著

一個壞掉的探測最典型的形態是**恆回傳「不認」** —— 而那與「跑在 overlayfs 上」
輸出完全相同。⇒ 本指令碼**同時**報文件系統類型（取自 `/proc/self/mountinfo`，
與 `preadv2` 走的是完全不同的兩條路）：
`fs=ext4` 配 `rwf_nowait=no` 是一個**看得出來不對勁**的組合，
而 `fs=overlay` 配 `rwf_nowait=no` 自洽。⛔ 兩個讀數一起看，別只看一個。
"""

import ctypes
import ctypes.util
import os
import sys

# uapi/linux/fs.h。⛔ 不從別處取：只要一個數，而多一層相依多一處會漂的東西。
RWF_NOWAIT = 0x00000008


class _Iovec(ctypes.Structure):
    _fields_ = [("iov_base", ctypes.c_void_p), ("iov_len", ctypes.c_size_t)]


def fs_type(path: str) -> str:
    """`path` 所在掛載點的文件系統類型。取不到就 `unknown`，⛔ 不猜。"""
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
    """在 `path` 下真發一次 `preadv2(RWF_NOWAIT)`。回 `yes` / `no` / `unknown`。

    ⚠ 探測文件有意**非空**：實測零長讀被短路在 `FMODE_NOWAIT` 檢查之前
    ⇒ 拿空文件探會把「不認」誤判成「認」。
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
        # ⛔ 「探不了」不等於「不認」—— 兩件事必須分得開。
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
