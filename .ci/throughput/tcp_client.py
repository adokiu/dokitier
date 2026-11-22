#!/usr/bin/env python3
"""Push `mb` megabytes through the tunnel over TCP and print the Mbps."""
import socket
import sys
import time

host = sys.argv[1]
port = int(sys.argv[2])
mb = int(sys.argv[3]) if len(sys.argv) > 3 else 512
payload = b"\0" * (1 << 20)

sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4 << 20)
sock.settimeout(120)

deadline = time.monotonic() + 30
while True:
    try:
        sock.connect((host, port))
        break
    except OSError:
        if time.monotonic() > deadline:
            raise
        time.sleep(0.5)

start = time.monotonic()
for _ in range(mb):
    sock.sendall(payload)
sock.shutdown(socket.SHUT_WR)
sock.close()
elapsed = time.monotonic() - start
print(f"{mb * 8 / elapsed:.1f}")
