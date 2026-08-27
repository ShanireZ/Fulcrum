#!/usr/bin/env python3
"""tests/proxyproto/run.sh 的裸 socket 客户端（M2 批 L 第 ① 步）。

★ ★ **为什么不能用 curl**：这一格要在 HTTP 请求（或 TLS ClientHello）**之前**
先写一段 PROXY protocol 头，而 curl 没有任何办法让我们在连接开头塞自己的字节。
⇒ 自带一个客户端，而它只做三件事：连、按指定方式写、把结果印成一行。

⚠ ⚠ **它必须能表达「头和请求在同一个 TCP 段里」与「分两次发」的区别** ——
那正是 fork 那一侧 `rewind()` 存在的全部理由：一次 read 很可能把 PROXY 头
与它后面的请求**一起**读回来，而多读到的那几个字节必须原样还回流里。
★ 少了 `--split` 这个开关，两条路会被同一条判据盖住，
  而**丢掉 rewind 的实现只在「一起发」时才坏**。

输出（永远一行，便于 shell 直接比对）：
    STATUS=<码> BODY=<体>     正常拿到响应
    CLOSED                    对端在给出任何响应之前关掉了连接
    ERROR=<原因>              别的失败（连不上、超时、TLS 失败……）
"""

import argparse
import socket
import ssl
import struct
import sys
import time

V2_SIG = bytes([0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A])


def build_header(spec, dst_port):
    """把 `--header` 的写法翻成真的字节。`None` = 不发头。"""
    if spec is None or spec == "none":
        return b""

    if spec == "v1unknown":
        # ★ 合法，且语义是「没有真实客户端」—— 上游 LB 的健康检查就长这样。
        return b"PROXY UNKNOWN\r\n"

    if spec == "v2local":
        # ver_cmd = 0x20（v2 + LOCAL），payload 长度 0。
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
        # ⚠ ⚠ PROXY 头**永远**走裸 socket，即使这条连接接下来要做 TLS ——
        #   它按规格就在 TLS 之前，而这正是被测的那件事。
        if hdr and args.split:
            sock.sendall(hdr)
            time.sleep(0.05)
        elif hdr:
            pass  # 与请求一起发，见下

        if args.tls:
            if hdr and not args.split:
                sock.sendall(hdr)
                # ★ TLS 那条路做不到「头与 ClientHello 在同一个段里」——
                #   ClientHello 是 ssl 库自己写的，我们插不进去。⇒ 只能先发头。
                #   ⚠ 这一句写在这里，是为了别让读的人以为 TLS 那格也覆盖了合并发送。
            ctx = ssl.create_default_context()
            ctx.check_hostname = False
            ctx.verify_mode = ssl.CERT_NONE
            stream = ctx.wrap_socket(sock, server_hostname=args.sni)
            stream.sendall(req)
        else:
            stream = sock
            if hdr and not args.split:
                stream.sendall(hdr + req)  # ★ ★ 一个段，这条路要 rewind 才对
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
        # ★ 一个字节都没回来 —— 那正是「清单内的来源不发头 ⇒ 关连接」的形状。
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
