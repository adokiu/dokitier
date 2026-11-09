#!/usr/bin/env python3
"""Minimal UDP shared-listener repro with info-level console logging.

core1 (host, two instances net-a/net-b) shares udp://10.210.0.11:PORT;
core2 joins net-a, core3 joins net-b. Both instances get
[console_logger] level = "info" so `shared udp` / peer-conn tracing becomes
visible in docker logs.
"""
import json
import sys
import time
import urllib.request
import http.cookiejar

BASE = "http://127.0.0.1:13110"
PASSWORD = "fmj2BvyY34LoRJo2ar"
PORT = 23244
HOST_IP = "10.210.0.11"

cj = http.cookiejar.CookieJar()
opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cj))


def req(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(
        BASE + path, data=data, method=method,
        headers={"Content-Type": "application/json"} if body else {})
    with opener.open(r, timeout=30) as resp:
        return json.loads(resp.read())


req("POST", "/api/login", {"username": "admin", "password": PASSWORD})


def toml(host, ipv4, network, secret, peers, console_log="info"):
    lines = [
        f'hostname = "{host}"',
        f'instance_name = "{host}-{network}"',
        f'ipv4 = "{ipv4}"',
        "",
        "[network_identity]",
        f'network_name = "{network}"',
        f'network_secret = "{secret}"',
    ]
    for p in peers:
        lines += ["", "[[peer]]", f'uri = "{p}"']
    lines += [
        "", "[flags]", "no_tun = true", "",
        "[console_logger]", f'level = "{console_log}"', "",
    ]
    return "\n".join(lines)


def apply(node, network, cfg):
    r = req("POST", f"/api/board/nodes/{node}/command",
            {"network": network, "command": {"kind": "apply_network", "network": network, "toml": cfg}})
    print("apply", node, network, json.dumps(r)[:120])


def edges(node, network):
    nodes = req("GET", "/api/board/nodes")
    for n in nodes.get("nodes", []):
        if n.get("node_id") == node:
            for inst in (n.get("snapshot") or {}).get("instances", []):
                if inst.get("network_name") == network:
                    return len(((inst.get("topology") or {}).get("edges") or []))
    return -1


req("POST", "/api/board/nodes/core1/settings", {"shared_listeners": f"udp://{HOST_IP}:{PORT}"})
time.sleep(2)

apply("core1", "net-a", toml("core1", "10.100.0.1", "net-a", "s-a", []))
apply("core1", "net-b", toml("core1", "10.101.0.1", "net-b", "s-b", []))
apply("core2", "net-a", toml("core2", "10.100.0.2", "net-a", "s-a", [f"udp://{HOST_IP}:{PORT}"]))
apply("core3", "net-b", toml("core3", "10.101.0.3", "net-b", "s-b", [f"udp://{HOST_IP}:{PORT}"]))

print("waiting 30s, polling edges...")
for i in range(15):
    time.sleep(2)
    print(f"t={i*2}s core1:net-a={edges('core1','net-a')} core2:net-a={edges('core2','net-a')} "
          f"core1:net-b={edges('core1','net-b')} core3:net-b={edges('core3','net-b')}")
