#!/usr/bin/env python3
"""行为核对：发一个带 `Connection: close` 的请求之后，服务端**真的先关了**吗？

    python3 bench/close-probe.py <host> <port> <path>
    python3 bench/close-probe.py --capture-request <port> <输出文件>
    python3 bench/close-probe.py --self-check

探针的退出码：
    0  限时内读到了 EOF，且状态行是 200 ⇒ 服务端关了
    3  限时内没读到 EOF ⇒ 服务端**没关**（回了什么头都不算数）
    4  读到了 EOF，但状态行不是 200
    1  连不上
    2  用法错

`--capture-request`：在 127.0.0.1:<port> 上只接一条带完整请求头的连接，把**请求头**原文落进
<输出文件>，回 200 + `Connection: close`；0 = 抓到了，1 = 20 秒内没等到。用例脚本拿它抓
**oha 真正发出的那个请求**（用负载那一组参数发一次），判据 ⑥ ② 判里面有没有 `Connection: close`
（owner 2026-09-23 改判：原先用 TIME_WAIT 占比判「服务端先关」，实测是竞态、且会被截断）。

★ ★ ★ **判在行为上，⛔ 不判响应头。** 一个回了 `Connection: close` 却不关连接的实现，
  头是对的、行为是错的 —— 而「高并发短连接」那一类要的是**服务端先关**这个行为
  （owner 2026-09-23 拍板；理由之一是 TIME_WAIT 该落在服务端、不压压测端的临时端口）。

⚠ `bench/case/short-connection-throughput.sh` 开跑前用它核四家各一次，并且**先跑本文件的
  自测**，自测不过整类不跑 —— ⛔ 一个从没见过「不关」的探针，与一个恒报「关了」的坏探针
  给出完全相同的输出。
"""

import pathlib
import re
import socket
import sys
import tempfile
import threading
import time

TIMEOUT = 3.0
STATUS_200 = re.compile(rb"^HTTP/1\.[01] 200 ")


def probe(host, port, path, timeout=TIMEOUT):
    try:
        s = socket.create_connection((host, port), timeout=timeout)
    except OSError:
        return 1
    with s:
        req = "GET %s HTTP/1.1\r\nHost: bench\r\nConnection: close\r\n\r\n" % path
        s.sendall(req.encode("ascii"))
        data = b""
        try:
            while True:
                chunk = s.recv(65536)
                if not chunk:
                    break  # EOF：对端关了
                data += chunk
        except socket.timeout:
            return 3
    return 0 if STATUS_200.match(data) else 4


def capture_once(listener, outfile, deadline):
    """只接一条**带完整请求头**的连接，把请求头原文（到第一个空行为止）写进 `outfile`，
    回 200 + `Connection: close` 然后关。返回 0 = 抓到了；1 = 截止前没等到。

    ⚠ 先来的空连接（连上就断、一个字节都不发 —— 编排脚本的 `wait_port` 就是这样）与残缺请求
      **跳过、接着等**，⛔ 不能被它们打发掉。
    ⚠ 只存请求头，⛔ 不存正文 —— 判据只看头（`bench/lib.sh` 的 `bench_request_close_violations`）。
    """
    while True:
        left = deadline - time.monotonic()
        if left <= 0:
            return 1
        listener.settimeout(left)
        try:
            conn, _ = listener.accept()
        except OSError:  # 含 socket.timeout
            return 1
        with conn:
            conn.settimeout(5)
            buf = b""
            try:
                while b"\r\n\r\n" not in buf and len(buf) < 65536:
                    chunk = conn.recv(4096)
                    if not chunk:
                        break
                    buf += chunk
            except OSError:
                continue
            if b"\r\n\r\n" not in buf:
                continue
            head = buf.split(b"\r\n\r\n", 1)[0] + b"\r\n\r\n"
            pathlib.Path(outfile).write_bytes(head)
            try:
                conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            except OSError:
                pass
            return 0


def _serve_once(listener, status, close_after):
    """合成被测：只接一条连接、回一份固定响应；`close_after` 决定回完关不关。"""
    try:
        conn, _ = listener.accept()
    except OSError:
        return  # 探针根本没连上来（监听端已被关掉）：由那一条断言去说，⛔ 不在这里再抛一次
    with conn:
        conn.settimeout(5)
        buf = b""
        while b"\r\n\r\n" not in buf:
            chunk = conn.recv(4096)
            if not chunk:
                return
            buf += chunk
        conn.sendall(b"HTTP/1.1 %d X\r\nContent-Length: 2\r\n\r\nok" % status)
        if not close_after:
            # ⛔ 不关：一直等到对端先走（探针超时后关掉它那一侧）。
            try:
                while conn.recv(4096):
                    pass
            except OSError:
                pass


def _synthetic(status, close_after):
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    t = threading.Thread(target=_serve_once, args=(listener, status, close_after), daemon=True)
    t.start()
    return listener, t


def self_check():
    fails = 0
    n = 0

    def want(label, expected, got):
        nonlocal fails, n
        n += 1
        if got != expected:
            fails += 1
            print("✗ %s（该是 %s，实得 %s）" % (label, expected, got), file=sys.stderr)

    # ★ 两个方向都要有：少了「不关」那一条，这个探针与一个恒报「关了」的坏探针分不开。
    for label, status, close_after, expected in (
        ("回完就关的合成被测没被判成「关了」", 200, True, 0),
        ("回完不关的合成被测被判成了「关了」—— 这个探针是空操作", 200, False, 3),
        ("回 404 的合成被测被判成了 200", 404, True, 4),
    ):
        listener, t = _synthetic(status, close_after)
        with listener:
            port = listener.getsockname()[1]
            want(label, expected, probe("127.0.0.1", port, "/p", timeout=0.5))
        t.join(timeout=5)

    # 连不上 ⇒ 1。⚠ 先绑一个端口拿到号再关掉，⛔ 不猜一个「应该没人用」的端口。
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    dead = s.getsockname()[1]
    s.close()
    want("连不上的端口没被判成 1", 1, probe("127.0.0.1", dead, "/p", timeout=0.5))

    # 抓请求头：先来一条**空连接**（连上就断 —— 编排脚本的 wait_port 就是这样），再来一条真请求
    #   ⇒ 必须跳过前者、只存后者的**请求头**（正文不存）、并回 200 + Connection: close。
    with tempfile.TemporaryDirectory() as tmp:
        out = pathlib.Path(tmp) / "req.txt"
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        with listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen(4)
            port = listener.getsockname()[1]
            result = {}
            t = threading.Thread(
                target=lambda: result.setdefault("rc", capture_once(listener, str(out), time.monotonic() + 5)),
                daemon=True,
            )
            t.start()
            socket.create_connection(("127.0.0.1", port), timeout=2).close()
            resp = b""
            with socket.create_connection(("127.0.0.1", port), timeout=2) as c:
                c.sendall(b"GET /x HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\nBODY-NOT-HEADER")
                try:
                    while True:
                        chunk = c.recv(4096)
                        if not chunk:
                            break
                        resp += chunk
                except socket.timeout:
                    pass
            t.join(timeout=5)
        want("抓头服务端被一条空连接打发了（没有接着等真请求）", 0, result.get("rc"))
        want(
            "抓到的不是完整请求头（或把正文也存了进去）",
            b"GET /x HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
            out.read_bytes() if out.exists() else b"",
        )
        want("抓头服务端没有回 200 + Connection: close", True, resp.startswith(b"HTTP/1.1 200") and b"Connection: close" in resp)

    if fails:
        print("[bench/close-probe] ★ 自测未通过（%d / %d 条）" % (fails, n), file=sys.stderr)
        return 1
    print("[bench/close-probe] 自测通过（合成被测，%d 条）" % n)
    return 0


def main(argv):
    if len(argv) == 2 and argv[1] == "--self-check":
        return self_check()
    if len(argv) == 4 and argv[1] == "--capture-request":
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        with listener:
            listener.bind(("127.0.0.1", int(argv[2])))
            listener.listen(16)
            return capture_once(listener, argv[3], time.monotonic() + 20)
    if len(argv) != 4:
        print(
            "用法：python3 bench/close-probe.py <host> <port> <path>"
            " | --capture-request <port> <输出文件> | --self-check",
            file=sys.stderr,
        )
        return 2
    return probe(argv[1], int(argv[2]), argv[3])


if __name__ == "__main__":
    sys.exit(main(sys.argv))
