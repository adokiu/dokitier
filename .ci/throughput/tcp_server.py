#!/usr/bin/env python3
"""Drain a TCP stream (server side of the tunnel throughput test)."""
import socket
import sys

port = int(sys.argv[1]) if len(sys.argv) > 1 else 5201
srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("0.0.0.0", port))
srv.listen(4)
srv.settimeout(20)
try:
    while True:
        conn, _ = srv.accept()
        with conn:
            total = 0
            while True:
                data = conn.recv(1 << 20)
                if not data:
                    break
                total += len(data)
        print(f"server_bytes {total}", flush=True)
        break
except socket.timeout:
    print("server_timeout", flush=True)
