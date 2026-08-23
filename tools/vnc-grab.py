#!/usr/bin/env python3
"""Grab one frame from a QEMU VNC display and save it as PNG.

Host-side ground truth for what is actually on the scan-out, without touching
the guest. `screendump` over QMP returns "no surface" on an `egl-headless`
display, so this is the capture path that works for the Helios VM.

Usage: vnc-grab.py OUT.png [host:port]   (default 127.0.0.1:5900)
"""
import socket
import struct
import sys

RAW = 0


def recvn(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionError(f"short read: wanted {n}, got {len(buf)}")
        buf += chunk
    return buf


def main() -> int:
    if not 2 <= len(sys.argv) <= 3:
        print(__doc__)
        return 2
    out = sys.argv[1]
    host, _, port = (sys.argv[2] if len(sys.argv) == 3 else "127.0.0.1:5900").partition(":")
    sock = socket.create_connection((host, int(port or 5900)), timeout=15)
    sock.settimeout(30)

    version = recvn(sock, 12)
    if not version.startswith(b"RFB "):
        raise SystemExit(f"not an RFB server: {version!r}")
    sock.sendall(b"RFB 003.008\n")

    count = recvn(sock, 1)[0]
    if count == 0:
        reason = recvn(sock, struct.unpack(">I", recvn(sock, 4))[0])
        raise SystemExit(f"server refused: {reason!r}")
    types = recvn(sock, count)
    if 1 not in types:
        raise SystemExit(f"only None auth is supported; server offers {list(types)}")
    sock.sendall(bytes([1]))
    if struct.unpack(">I", recvn(sock, 4))[0] != 0:
        raise SystemExit("VNC auth failed")

    sock.sendall(bytes([1]))  # ClientInit, shared
    width, height = struct.unpack(">HH", recvn(sock, 4))
    recvn(sock, 16)  # server pixel format, replaced below
    recvn(sock, struct.unpack(">I", recvn(sock, 4))[0])  # desktop name

    # 32bpp true colour, little-endian, shifts chosen so bytes arrive B,G,R,X.
    fmt = struct.pack(">BBBBHHHBBBxxx", 32, 24, 0, 1, 255, 255, 255, 16, 8, 0)
    sock.sendall(struct.pack(">Bxxx", 0) + fmt)
    sock.sendall(struct.pack(">BxH", 2, 1) + struct.pack(">i", RAW))
    sock.sendall(struct.pack(">BBHHHH", 3, 0, 0, 0, width, height))

    pixels = bytearray(width * height * 4)
    while True:
        msg = recvn(sock, 1)[0]
        if msg != 0:
            raise SystemExit(f"unexpected server message {msg}")
        recvn(sock, 1)
        rects = struct.unpack(">H", recvn(sock, 2))[0]
        for _ in range(rects):
            x, y, w, h, enc = struct.unpack(">HHHHi", recvn(sock, 12))
            if enc != RAW:
                raise SystemExit(f"server used encoding {enc}, only Raw is handled")
            data = recvn(sock, w * h * 4)
            for row in range(h):
                start = ((y + row) * width + x) * 4
                pixels[start:start + w * 4] = data[row * w * 4:(row + 1) * w * 4]
        break

    from PIL import Image

    image = Image.frombytes("RGBA", (width, height), bytes(pixels), "raw", "BGRA")
    image.convert("RGB").save(out)
    extrema = image.convert("L").getextrema()
    nonzero = sum(1 for i in range(0, len(pixels), 4) if pixels[i] or pixels[i + 1] or pixels[i + 2])
    print(f"{out}: {width}x{height}  luma min/max={extrema}  nonzero px={nonzero}/{width * height}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
