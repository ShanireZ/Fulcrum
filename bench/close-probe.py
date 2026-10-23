#!/usr/bin/env python3
"""行為核對：發一個帶 `Connection: close` 的請求之後，伺服器端**真的先關了**嗎？

    python3 bench/close-probe.py <host> <port> <path>
    python3 bench/close-probe.py --tls <SNI> <host> <port> <path>
    python3 bench/close-probe.py --capture-request <port> <輸出檔案>
    python3 bench/close-probe.py --self-check

`--tls <SNI>`（G168，TLS 握手那一類）：同一個核對走 TLS（帶這個 SNI、⛔ 不驗證書 —— 被測用的是自簽證書，
這裡判的是「回完關不關」，不是 PKI）。握不上算「連不上」（1）。

探針的結束代碼：
    0  限時內讀到了 EOF，且狀態行是 200 ⇒ 伺服器端關了
    3  限時內沒讀到 EOF ⇒ 伺服器端**沒關**（回了什麼頭都不算數）
    4  讀到了 EOF，但狀態行不是 200
    1  連不上
    2  用法錯

`--capture-request`：在 127.0.0.1:<port> 上只接一條帶完整請求頭的連接，把**請求頭**原文落進
<輸出檔案>，回 200 + `Connection: close`；0 = 抓到了，1 = 20 秒內沒等到。案例指令碼拿它抓
**oha 真正發出的那個請求**（用負載那一組參數發一次），判據 ⑥ ② 判裡面有沒有 `Connection: close`
（owner 2026-09-23 改判：原先用 TIME_WAIT 佔比判「伺服器端先關」，實測是競態、且會被截斷）。

★ ★ ★ **判在行為上，⛔ 不判回應頭。** 一個回了 `Connection: close` 卻不關連接的實作，
  頭是對的、行為是錯的 —— 而「高並行短連接」那一類要的是**伺服器端先關**這個行為
  （owner 2026-09-23 拍板；理由之一是 TIME_WAIT 該落在伺服器端、不壓壓測端的臨時連接埠）。

⚠ `bench/case/short-connection-throughput.sh` 開跑前用它核四家各一次，並且**先跑本文件的
  自測**，自測不過整類不跑 —— ⛔ 一個從沒見過「不關」的探針，與一個恆報「關了」的壞探針
  給出完全相同的輸出。
"""

import pathlib
import re
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time

TIMEOUT = 3.0
STATUS_200 = re.compile(rb"^HTTP/1\.[01] 200 ")


def _client_tls():
    """不驗證書的用戶端 context：被測是自簽證書，這裡判的是行為，不是 PKI。"""
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


def probe(host, port, path, timeout=TIMEOUT, sni=None):
    try:
        s = socket.create_connection((host, port), timeout=timeout)
    except OSError:
        return 1
    if sni is not None:
        # ★ `suppress_ragged_eofs` 預設為真：對端不發 close_notify 直接斷時讀到的也是 b""（= EOF），
        #   與明文那條路同一個判法。
        try:
            s = _client_tls().wrap_socket(s, server_hostname=sni)
        except (OSError, ssl.SSLError):
            s.close()
            return 1
    with s:
        # ★ TLS 那條路的 Host 與 SNI 是同一個名字：樞衡按 Host 挑站台（TLS 站台寫的是主機名），
        #   明文那幾類的站台是 `:連接埠`、什麼 Host 都接，所以那邊寫死 `bench` 不礙事（2026-10-23 撞見）。
        req = "GET %s HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n" % (path, sni or "bench")
        s.sendall(req.encode("ascii"))
        data = b""
        try:
            while True:
                chunk = s.recv(65536)
                if not chunk:
                    break  # EOF：對端關了
                data += chunk
        except socket.timeout:
            return 3
    return 0 if STATUS_200.match(data) else 4


def capture_once(listener, outfile, deadline):
    """只接一條**帶完整請求頭**的連接，把請求頭原文（到第一個空行為止）寫進 `outfile`，
    回 200 + `Connection: close` 然後關。回傳 0 = 抓到了；1 = 截止前沒等到。

    ⚠ 先來的空連接（連上就斷、一個位元組都不發 —— 編排指令碼的 `wait_port` 就是這樣）與殘缺請求
      **跳過、接著等**，⛔ 不能被它們打發掉。
    ⚠ 只存請求頭，⛔ 不存正文 —— 判據只看頭（`bench/lib.sh` 的 `bench_request_close_violations`）。
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


def _serve_once(listener, status, close_after, tls=None):
    """合成被測：只接一條連接、回一份固定回應；`close_after` 決定回完關不關；`tls` 給了就先握 TLS。"""
    try:
        conn, _ = listener.accept()
    except OSError:
        return  # 探針根本沒連上來（監聽端已被關掉）：由那一條斷言去說，⛔ 不在這裡再拋一次
    if tls is not None:
        try:
            conn = tls.wrap_socket(conn, server_side=True)
        except (OSError, ssl.SSLError):
            conn.close()
            return
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
            # ⛔ 不關：一直等到對端先走（探針超時後關掉它那一側）。
            try:
                while conn.recv(4096):
                    pass
            except OSError:
                pass


def _synthetic(status, close_after, tls=None):
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    t = threading.Thread(target=_serve_once, args=(listener, status, close_after, tls), daemon=True)
    t.start()
    return listener, t


def _server_tls(tmp):
    """自測用的伺服器端 context：現造一張 P-256 自簽證書（用鏡像裡的 openssl CLI）。拿不到 ⇒ None。"""
    if shutil.which("openssl") is None:
        return None
    key, crt = pathlib.Path(tmp) / "k.pem", pathlib.Path(tmp) / "c.pem"
    rc = subprocess.run(
        ["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
         "-keyout", str(key), "-out", str(crt), "-days", "1", "-subj", "/CN=probe.test"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    ).returncode
    if rc != 0:
        return None
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(str(crt), str(key))
    return ctx


def self_check():
    fails = 0
    n = 0

    def want(label, expected, got):
        nonlocal fails, n
        n += 1
        if got != expected:
            fails += 1
            print("✗ %s（该是 %s，实得 %s）" % (label, expected, got), file=sys.stderr)

    # ★ 兩個方向都要有：少了「不關」那一條，這個探針與一個恆報「關了」的壞探針分不開。
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

    # ★ `--tls` 那條路（G168）同樣兩個方向都要有 —— TLS 層把 EOF 讀成例外、或把超時讀成 EOF 的實作，
    #   在明文那三條上一條都露不出來。拿不到證書（沒有 openssl）⇒ 這一半判不動，算失敗、⛔ 不算跳過。
    with tempfile.TemporaryDirectory() as tmp:
        tls = _server_tls(tmp)
        want("TLS 自测造不出证书（openssl 不在？）—— 这一半判不动", True, tls is not None)
        if tls is not None:
            for label, close_after, expected in (
                ("TLS：回完就关的合成被测没被判成「关了」", True, 0),
                ("TLS：回完不关的合成被测被判成了「关了」", False, 3),
            ):
                listener, t = _synthetic(200, close_after, tls)
                with listener:
                    port = listener.getsockname()[1]
                    want(label, expected, probe("127.0.0.1", port, "/p", timeout=0.5, sni="probe.test"))
                t.join(timeout=5)
            # 對著一個明文伺服器走 TLS ⇒ 握不上 ⇒ 1（不是「關了」）。
            listener, t = _synthetic(200, True)
            with listener:
                port = listener.getsockname()[1]
                want("TLS：握不上没被判成 1", 1, probe("127.0.0.1", port, "/p", timeout=0.5, sni="probe.test"))
            t.join(timeout=5)

    # 連不上 ⇒ 1。⚠ 先綁一個連接埠拿到號再關掉，⛔ 不猜一個「應該沒人用」的連接埠。
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    dead = s.getsockname()[1]
    s.close()
    want("连不上的端口没被判成 1", 1, probe("127.0.0.1", dead, "/p", timeout=0.5))

    # 抓請求頭：先來一條**空連接**（連上就斷 —— 編排指令碼的 wait_port 就是這樣），再來一條真請求
    #   ⇒ 必須跳過前者、只存後者的**請求頭**（正文不存）、並回 200 + Connection: close。
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
    if len(argv) == 6 and argv[1] == "--tls":
        return probe(argv[3], int(argv[4]), argv[5], sni=argv[2])
    if len(argv) != 4:
        print(
            "用法：python3 bench/close-probe.py <host> <port> <path>"
            " | --tls <SNI> <host> <port> <path> | --capture-request <port> <输出文件> | --self-check",
            file=sys.stderr,
        )
        return 2
    return probe(argv[1], int(argv[2]), argv[3])


if __name__ == "__main__":
    sys.exit(main(sys.argv))
