#!/usr/bin/env python3
"""tests/proxyproto/run.sh 的裸 socket 用戶端（M2 批 L 第 ① 步）。

★ ★ **為什麼不能用 curl**：這一格要在 HTTP 請求（或 TLS ClientHello）**之前**
先寫一段 PROXY protocol 頭，而 curl 沒有任何辦法讓我們在連接開頭塞自己的位元組。
⇒ 自帶一個用戶端，而它只做三件事：連、按指定方式寫、把結果印成一行。

⚠ ⚠ **它必須能表達「頭和請求在同一個 TCP 段裡」與「分兩次發」的區別** ——
那正是 fork 那一側 `rewind()` 存在的全部理由：一次 read 很可能把 PROXY 頭
與它後面的請求**一起**讀回來，而多讀到的那幾個位元組必須原樣還回流裡。
★ 少了 `--split` 這個開關，兩條路會被同一條判據蓋住，
  而**丟掉 rewind 的實作只在「一起發」時才壞**。

輸出（永遠一行，便於 shell 直接比對）：
    STATUS=<碼> BODY=<體>     正常拿到回應
    CLOSED                    對端在給出任何回應之前關掉了連接
    ERROR=<原因>              別的失敗（連不上、超時、TLS 失敗……）
"""

import argparse
import socket
import ssl
import struct
import sys
import time

V2_SIG = bytes([0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A])


def build_header(spec, dst_port):
    """把 `--header` 的寫法翻成真的位元組。`None` = 不發頭。"""
    if spec is None or spec == "none":
        return b""

    if spec == "v1unknown":
        # ★ 合法，且語義是「沒有真實用戶端」—— 上游 LB 的健康檢查就長這樣。
        return b"PROXY UNKNOWN\r\n"

    if spec == "v2local":
        # ver_cmd = 0x20（v2 + LOCAL），payload 長度 0。
        return V2_SIG + bytes([0x20, 0x11]) + struct.pack("!H", 0)

    kind, ip, sport = spec.split(":", 2)
    if kind == "v1":
        return ("PROXY TCP4 %s 127.0.0.1 %s %d\r\n" % (ip, sport, dst_port)).encode()
    if kind == "v2":
        payload = (
            socket.inet_aton(ip)
            + socket.inet_aton("127.0.0.1")
            + struct.pack("!H", int(sport))
            + struct.pack("!H", dst_port)
        )
        # ver_cmd = 0x21（v2 + PROXY），fam = 0x11（AF_INET + STREAM）。
        return V2_SIG + bytes([0x21, 0x11]) + struct.pack("!H", len(payload)) + payload
    raise SystemExit("认不得的 --header 写法：%s" % spec)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--tls", action="store_true")
    ap.add_argument("--sni", default="a.example")
    ap.add_argument("--header", default=None)
    ap.add_argument("--path", default="/")
    ap.add_argument("--host-header", default="a.example")
    ap.add_argument(
        "--split",
        action="store_true",
        help="★ 头与请求分两次发（中间停 50ms）——不加就是**一个段里全发完**",
    )
    ap.add_argument("--timeout", type=float, default=8.0)
    args = ap.parse_args()

    hdr = build_header(args.header, args.port)
    req = (
        "GET %s HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n"
        % (args.path, args.host_header)
    ).encode()

    try:
        sock = socket.create_connection((args.host, args.port), timeout=args.timeout)
        sock.settimeout(args.timeout)
    except OSError as e:
        print("ERROR=connect:%s" % e)
        return 0

    try:
        # ⚠ ⚠ PROXY 頭**永遠**走裸 socket，即使這條連接接下來要做 TLS ——
        #   它按規格就在 TLS 之前，而這正是被測的那件事。
        if hdr and args.split:
            sock.sendall(hdr)
            time.sleep(0.05)
        elif hdr:
            pass  # 與請求一起發，見下

        if args.tls:
            if hdr and not args.split:
                sock.sendall(hdr)
                # ★ TLS 那條路做不到「頭與 ClientHello 在同一個段裡」——
                #   ClientHello 是 ssl 庫自己寫的，我們插不進去。⇒ 只能先發頭。
                #   ⚠ 這一句寫在這裡，是為了別讓讀的人以為 TLS 那格也覆蓋了合並行送。
            ctx = ssl.create_default_context()
            ctx.check_hostname = False
            ctx.verify_mode = ssl.CERT_NONE
            stream = ctx.wrap_socket(sock, server_hostname=args.sni)
            stream.sendall(req)
        else:
            stream = sock
            if hdr and not args.split:
                stream.sendall(hdr + req)  # ★ ★ 一個段，這條路要 rewind 才對
            else:
                stream.sendall(req)

        chunks = []
        while True:
            b = stream.recv(65536)
            if not b:
                break
            chunks.append(b)
        raw = b"".join(chunks)
    except (OSError, ssl.SSLError) as e:
        print("ERROR=io:%s" % e)
        return 0
    finally:
        try:
            sock.close()
        except OSError:
            pass

    if not raw:
        # ★ 一個位元組都沒回來 —— 那正是「清單內的來源不發頭 ⇒ 關連接」的形狀。
        print("CLOSED")
        return 0

    head, _, body = raw.partition(b"\r\n\r\n")
    first = head.split(b"\r\n", 1)[0].decode("latin-1")
    parts = first.split(" ")
    status = parts[1] if len(parts) > 1 else "?"
    print("STATUS=%s BODY=%s" % (status, body.decode("latin-1").strip()))
    return 0


if __name__ == "__main__":
    sys.exit(main())
