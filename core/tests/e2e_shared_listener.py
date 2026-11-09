#!/usr/bin/env python3
"""End-to-end test for core-level shared listeners (udp / ws / quic / tcp).

One public port of a *host* core is shared by two different EasyTier networks.
A second and third core join those two networks and peer only with the host's
shared public address. The test drives an already running dokitier console
(board API), pushes network configs to the test cores, and polls the node
snapshots for peer topology edges.

Topology used for every protocol:

    net-a:  core1(host, two instances)  <-- peer -->  core2
    net-b:  core1(host, two instances)  <-- peer -->  core3
                    ^ same public proto://host-ip:port for both networks

Success per protocol (PASS) requires:

* core1 net-a / net-b instances exist, have distinct instance-id and ipv4,
* every one of {core1:net-a, core2:net-a, core1:net-b, core3:net-b} reports
  topology.edges > 0 for several consecutive polls (the shared listener
  demultiplexes two networks over one public port),
* optional cross-check: the host instance's edge peer-id equals the client
  instance peer-id (reported as a warning when snapshots are skewed).

Nothing outside the console board API is used (except an optional DB cleanup
hook, see --purge-cmd, needed because the board API has no "forget network
config" endpoint and deletes are undone by the console reconcile loop).

Usage:

    python3 core/tests/e2e_shared_listener.py \
        --base-url http://127.0.0.1:13110 \
        --username admin --password '...' \
        --host-core core1 --host-ip 10.210.0.11 \
        --client-core-a core2 --client-core-b core3 \
        --protocols udp,ws,quic,tcp

    # single protocol, custom port, longer timeout
    python3 core/tests/e2e_shared_listener.py --protocols quic --timeout 90

Exit code is 0 only when every requested protocol passes. A JSON report can
be written with --report /tmp/shared-listener-e2e.json.
"""

from __future__ import annotations

import argparse
import http.cookiejar
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

PROTOCOLS = ("udp", "ws", "wss", "quic", "tcp")
# default public ports, one port per protocol; only one protocol is tested at a
# time and the port is restored afterwards.
DEFAULT_PORTS = {"udp": 21234, "ws": 21345, "wss": 21678, "quic": 21456, "tcp": 21567}


def log(msg: str) -> None:
    stamp = time.strftime("%H:%M:%S")
    print(f"[{stamp}] {msg}", flush=True)


class ApiError(RuntimeError):
    pass


class Console:
    """Minimal cookie-authenticated JSON client for the board API."""

    def __init__(self, base_url: str, timeout: float = 40.0):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.cookies = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(self.cookies)
        )

    def request(self, method: str, path: str, body=None, timeout=None) -> dict:
        url = self.base_url + path
        data = None
        headers = {}
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with self.opener.open(req, timeout=timeout or self.timeout) as resp:
                raw = resp.read()
        except urllib.error.HTTPError as err:  # keep the body for diagnostics
            raw = err.read()
            try:
                payload = json.loads(raw)
            except Exception:
                payload = {"raw": raw.decode("utf-8", "replace")[:500]}
            payload.setdefault("http_status", err.code)
            return payload
        try:
            return json.loads(raw)
        except Exception as exc:  # pragma: no cover - defensive
            raise ApiError(f"non-JSON response from {path}: {raw[:200]!r}") from exc

    def login(self, username: str, password: str) -> None:
        res = self.request(
            "POST", "/api/login", {"username": username, "password": password}
        )
        if not res.get("ok"):
            raise ApiError(f"login failed: {res}")

    def nodes(self) -> dict:
        res = self.request("GET", "/api/board/nodes")
        if "nodes" not in res:
            raise ApiError(f"unexpected /api/board/nodes payload: {str(res)[:300]}")
        return res

    def set_shared_listeners(self, node_id: str, listeners: str) -> dict:
        return self.request(
            "POST",
            f"/api/board/nodes/{node_id}/settings",
            {"shared_listeners": listeners},
        )

    def apply_network(self, node_id: str, network: str, toml: str) -> dict:
        return self.request(
            "POST",
            f"/api/board/nodes/{node_id}/command",
            {
                "network": network,
                "command": {"kind": "apply_network", "network": network, "toml": toml},
            },
        )

    def delete_network(self, node_id: str, network: str) -> dict:
        return self.request(
            "POST",
            f"/api/board/nodes/{node_id}/command",
            {
                "network": network,
                "command": {"kind": "delete_network", "network": network},
            },
        )

    def audit(self) -> list:
        res = self.request("GET", "/api/board/audit")
        return res.get("logs") or []


# ---------------------------------------------------------------------------
# snapshot helpers
# ---------------------------------------------------------------------------


def node_by_id(nodes: dict, node_id: str) -> dict | None:
    for node in nodes.get("nodes") or []:
        if node.get("node_id") == node_id:
            return node
    return None


def node_instances(nodes: dict, node_id: str) -> list:
    node = node_by_id(nodes, node_id)
    if not node:
        return []
    snap = node.get("snapshot") or {}
    return snap.get("instances") or []


def node_snapshot_ts(nodes: dict, node_id: str) -> int:
    node = node_by_id(nodes, node_id)
    if not node:
        return 0
    snap = node.get("snapshot") or {}
    return int(snap.get("ts") or 0)


def find_instance(nodes: dict, node_id: str, network: str) -> dict | None:
    for inst in node_instances(nodes, node_id):
        if inst.get("network_name") == network:
            return inst
    return None


def edge_count(inst: dict | None) -> int:
    if not inst:
        return -1
    return len((inst.get("topology") or {}).get("edges") or [])


def edge_peers(inst: dict | None) -> set[int]:
    """peer ids this instance currently has an edge to."""
    if not inst:
        return set()
    peers = set()
    for edge in (inst.get("topology") or {}).get("edges") or []:
        for key in ("a", "b"):
            pid = edge.get(key)
            if isinstance(pid, int):
                peers.add(pid)
    return peers


def edge_tunnels(inst: dict | None) -> list:
    out = []
    if not inst:
        return out
    for edge in (inst.get("topology") or {}).get("edges") or []:
        for tunnel in edge.get("tunnels") or []:
            out.append(
                {
                    "type": tunnel.get("type"),
                    "local_addr": tunnel.get("local_addr"),
                    "remote_addr": tunnel.get("remote_addr"),
                }
            )
    return out


def is_loopback_tunnel(tunnels: list) -> bool:
    for tunnel in tunnels:
        for key in ("local_addr", "remote_addr"):
            addr = tunnel.get(key) or ""
            if addr.split("://", 1)[-1].startswith("127.0.0.1:"):
                return True
    return False


# ---------------------------------------------------------------------------
# config building
# ---------------------------------------------------------------------------


def build_toml(
    node_id: str,
    ipv4: str,
    network: str,
    secret: str,
    peers: list[str],
    instance_name: str,
) -> str:
    lines = [
        f'hostname = "{node_id}"',
        f'instance_name = "{instance_name}"',
        f'ipv4 = "{ipv4}"',
        "",
        "[network_identity]",
        f'network_name = "{network}"',
        f'network_secret = "{secret}"',
    ]
    for uri in peers:
        lines += ["", "[[peer]]", f'uri = "{uri}"']
    lines += ["", "[flags]", "no_tun = true", ""]
    return "\n".join(lines)


def public_addr(proto: str, host_ip: str, port: int) -> str:
    return f"{proto}://{host_ip}:{port}"


# ---------------------------------------------------------------------------
# protocol run
# ---------------------------------------------------------------------------


class ProtocolResult:
    def __init__(self, proto: str, port: int):
        self.proto = proto
        self.port = port
        self.status = "FAIL"
        self.elapsed_s = 0.0
        self.reasons: list[str] = []
        self.warnings: list[str] = []
        self.edges: dict[str, int] = {}
        self.instances: dict[str, dict] = {}
        self.apply: dict[str, dict] = {}
        self.settings: dict[str, dict] = {}

    def as_dict(self) -> dict:
        return {
            "protocol": self.proto,
            "port": self.port,
            "status": self.status,
            "elapsed_s": round(self.elapsed_s, 2),
            "reasons": self.reasons,
            "warnings": self.warnings,
            "edges": self.edges,
            "instances": self.instances,
            "apply": self.apply,
            "settings": self.settings,
        }


def wait_for_absence(
    console: Console, nodes_ids: list[str], nets: list[str], timeout: float = 25.0
) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        nodes = console.nodes()
        present = []
        for node_id in nodes_ids:
            for net in nets:
                if find_instance(nodes, node_id, net):
                    present.append(f"{node_id}:{net}")
        if not present:
            return True
        time.sleep(2)
    return False


def apply_with_retry(
    console: Console, node_id: str, network: str, toml: str, attempts: int = 3
) -> dict:
    last = {}
    for attempt in range(1, attempts + 1):
        last = console.apply_network(node_id, network, toml)
        if last.get("ok"):
            return last
        log(f"    apply {node_id}/{network} attempt {attempt} failed: {last}")
        time.sleep(2.0 * attempt)
    return last


def run_protocol(
    console: Console,
    args: argparse.Namespace,
    proto: str,
    port: int,
    index: int,
    baseline_shared: str,
) -> ProtocolResult:
    result = ProtocolResult(proto, port)
    result.settings["host_baseline"] = baseline_shared
    started = time.time()

    net_a, net_b = args.network_a, args.network_b
    host = args.host_core
    core_a = args.client_core_a
    core_b = args.client_core_b
    all_nodes = [host, core_a, core_b]
    shared = public_addr(proto, args.host_ip, port)

    # fresh per-protocol subnets to avoid clashing with earlier networks still
    # being torn down / other test networks present on the same cores.
    octet_a = 100 + 2 * index
    octet_b = octet_a + 1
    ip_a = {host: f"10.{octet_a}.0.1/24", core_a: f"10.{octet_a}.0.2/24"}
    ip_b = {host: f"10.{octet_b}.0.1/24", core_b: f"10.{octet_b}.0.3/24"}

    # ---------------------------------------------------------------- cleanup
    def purge_configs() -> None:
        if not args.purge_cmd:
            return
        cmd = args.purge_cmd.replace("{networks}", f"{net_a},{net_b}")
        log(f"    purge: {cmd}")
        proc = subprocess.run(cmd, shell=True, capture_output=True, text=True)
        if proc.returncode != 0:
            result.warnings.append(
                f"purge command failed rc={proc.returncode}: "
                f"{(proc.stderr or proc.stdout).strip()[:200]}"
            )

    def delete_test_networks() -> None:
        for node_id in all_nodes:
            for net in (net_a, net_b):
                res = console.delete_network(node_id, net)
                if not res.get("ok") and "not" not in str(res.get("error", "")).lower():
                    result.warnings.append(
                        f"delete {node_id}/{net}: {res.get('error') or res}"
                    )

    purge_configs()
    delete_test_networks()
    wait_for_absence(console, all_nodes, [net_a, net_b], timeout=15)

    # ------------------------------------------------------------ prechecks
    nodes = console.nodes()
    for node_id in [host, core_a, core_b]:
        node = node_by_id(nodes, node_id)
        if not node:
            result.reasons.append(f"node {node_id} not registered in console")
        elif node.get("status") != "online":
            result.reasons.append(
                f"node {node_id} is {node.get('status')} (last_seen={node.get('last_seen')})"
            )
    if result.reasons:
        result.elapsed_s = time.time() - started
        return result

    log(
        f"  [{proto}] shared public addr {shared}; "
        f"net-a ipv4 {ip_a[host]} <-> {ip_a[core_a]}, "
        f"net-b ipv4 {ip_b[host]} <-> {ip_b[core_b]}"
    )

    # ------------------------------------------- push network configs first
    # (instances start with the previous listener set; the settings change
    # below triggers the console reconcile which re-applies the latest config
    # and therefore rewrites the internal listeners for this protocol.)
    configs = [
        (
            host,
            net_a,
            build_toml(host, ip_a[host], net_a, f"{net_a}-secret", [], f"e2e-{net_a}"),
        ),
        (
            host,
            net_b,
            build_toml(host, ip_b[host], net_b, f"{net_b}-secret", [], f"e2e-{net_b}"),
        ),
        (
            core_a,
            net_a,
            build_toml(
                core_a,
                ip_a[core_a],
                net_a,
                f"{net_a}-secret",
                [public_addr(proto, args.host_ip, port)],
                f"e2e-{net_a}",
            ),
        ),
        (
            core_b,
            net_b,
            build_toml(
                core_b,
                ip_b[core_b],
                net_b,
                f"{net_b}-secret",
                [public_addr(proto, args.host_ip, port)],
                f"e2e-{net_b}",
            ),
        ),
    ]
    for node_id, net, toml in configs:
        res = apply_with_retry(console, node_id, net, toml)
        result.apply[f"{node_id}:{net}"] = {
            "ok": bool(res.get("ok")),
            "error": res.get("error"),
        }
        if not res.get("ok"):
            result.reasons.append(
                f"apply_network {node_id}/{net} failed: {res.get('error') or res}"
            )
    if result.reasons:
        result.elapsed_s = time.time() - started
        return result

    # ----------------------------------------- switch the host shared listener
    log(f"  [{proto}] set {host} shared_listeners = {shared}")
    settings = console.set_shared_listeners(host, shared)
    result.settings[host] = settings
    if not settings.get("ok"):
        result.reasons.append(
            f"set shared_listeners {shared} failed: "
            f"{settings.get('error') or settings}"
        )
        if "Address already in use" in str(settings.get("error", "")):
            result.reasons.append(
                "hint: the public port is still held by a previous shared-listener "
                "hot update (known leak when a network keeps active sessions while "
                "the shared listener is rebound); pick another port with "
                "--ports <proto>=<port> or restart the host core before retrying"
            )
    if args.clear_client_listeners:
        for node_id in (core_a, core_b):
            res = console.set_shared_listeners(node_id, "")
            result.settings[node_id] = res

    # the console re-applies the stored configs ~5s after a settings change,
    # which is when the instances pick up the rewritten internal listeners.
    log(f"  [{proto}] waiting for topology edges (timeout {args.timeout}s) ...")

    stable_required = args.stable_rounds
    stable = 0
    last_summary = ""
    last_snap: dict = {}
    deadline = started + args.timeout
    while time.time() < deadline and not result.reasons:
        time.sleep(args.poll_interval)
        try:
            nodes = console.nodes()
        except Exception as exc:  # network hiccup; keep polling
            log(f"    poll error: {exc}")
            continue
        last_snap = nodes

        inst_a_host = find_instance(nodes, host, net_a)
        inst_a_client = find_instance(nodes, core_a, net_a)
        inst_b_host = find_instance(nodes, host, net_b)
        inst_b_client = find_instance(nodes, core_b, net_b)

        counts = {
            f"{host}:{net_a}": edge_count(inst_a_host),
            f"{core_a}:{net_a}": edge_count(inst_a_client),
            f"{host}:{net_b}": edge_count(inst_b_host),
            f"{core_b}:{net_b}": edge_count(inst_b_client),
        }
        result.edges = counts
        last_summary = json.dumps(counts, sort_keys=True)

        if all(value > 0 for value in counts.values()):
            stable += 1
        else:
            stable = 0
        if stable >= stable_required:
            break

    # ----------------------------------------------------------- final checks
    nodes = last_snap or console.nodes()
    inst = {
        "host-a": find_instance(nodes, host, net_a),
        "client-a": find_instance(nodes, core_a, net_a),
        "host-b": find_instance(nodes, host, net_b),
        "client-b": find_instance(nodes, core_b, net_b),
    }
    result.edges = {
        f"{host}:{net_a}": edge_count(inst["host-a"]),
        f"{core_a}:{net_a}": edge_count(inst["client-a"]),
        f"{host}:{net_b}": edge_count(inst["host-b"]),
        f"{core_b}:{net_b}": edge_count(inst["client-b"]),
    }
    for key, value in inst.items():
        if not value:
            result.reasons.append(f"instance {key} missing from snapshot")
            continue
        topo = value.get("topology") or {}
        result.instances[key] = {
            "network_name": value.get("network_name"),
            "instance_id": value.get("instance_id"),
            "peer_id": value.get("peer_id"),
            "ipv4": value.get("ipv4"),
            "edges": len(topo.get("edges") or []),
            "topology_nodes": len(topo.get("nodes") or []),
            "tunnels": edge_tunnels(value),
        }

    if not result.reasons:
        # (1) every instance must observe at least one edge
        if not all(value > 0 for value in result.edges.values()):
            result.reasons.append(
                f"timeout after {args.timeout}s: edges not all positive: {last_summary}"
            )
        # (2) both networks must run independent instances / addresses on host
        host_a = result.instances.get("host-a") or {}
        host_b = result.instances.get("host-b") or {}
        if host_a.get("instance_id") and host_a.get("instance_id") == host_b.get("instance_id"):
            result.reasons.append("net-a and net-b share one instance_id on host")
        if host_a.get("ipv4") and host_a.get("ipv4") == host_b.get("ipv4"):
            result.reasons.append("net-a and net-b share one ipv4 on host")
        if host_a.get("network_name") != net_a or host_b.get("network_name") != net_b:
            result.reasons.append("host snapshot network names mismatch")

        # (3) soft cross-check: host edge peer-id must be the client instance
        client_a = result.instances.get("client-a") or {}
        client_b = result.instances.get("client-b") or {}
        if inst["host-a"] and inst["client-a"]:
            if client_a.get("peer_id") not in edge_peers(inst["host-a"]) or (
                host_a.get("peer_id") not in edge_peers(inst["client-a"])
            ):
                result.warnings.append(
                    "net-a peer-id cross-check skewed "
                    f"(host peers={sorted(edge_peers(inst['host-a']))}, "
                    f"client peer_id={client_a.get('peer_id')})"
                )
        if inst["host-b"] and inst["client-b"]:
            if client_b.get("peer_id") not in edge_peers(inst["host-b"]) or (
                host_b.get("peer_id") not in edge_peers(inst["client-b"])
            ):
                result.warnings.append(
                    "net-b peer-id cross-check skewed "
                    f"(host peers={sorted(edge_peers(inst['host-b']))}, "
                    f"client peer_id={client_b.get('peer_id')})"
                )

        # (4) evidence that the host side was reached through the shared
        # listener: the proxy re-originates from loopback. Not fatal if the
        # peer reports a non-loopback underlay, so record it as a warning.
        host_tunnels = (result.instances.get("host-a") or {}).get("tunnels") or []
        if host_tunnels and not is_loopback_tunnel(host_tunnels):
            result.warnings.append(
                "host net-a tunnels do not show a 127.0.0.1 hop "
                f"(shared-listener proxy hop not observed): {host_tunnels}"
            )
        # expected tunnel transport for the protocol selection
        expected_types = {
            "udp": {"udp"},
            "tcp": {"tcp"},
            "ws": {"websocket", "ws", "tcp"},
            "wss": {"websocket", "wss", "tcp"},
            "quic": {"quic"},
        }[proto]
        for key in ("client-a", "client-b"):
            tunnels = (result.instances.get(key) or {}).get("tunnels") or []
            types = {t.get("type") for t in tunnels}
            if tunnels and not (types & expected_types):
                result.warnings.append(
                    f"{key} tunnel types {sorted(types)} not in expected {sorted(expected_types)}"
                )

    if not result.reasons:
        result.status = "PASS"
    else:
        # add live diagnostics for the failure report
        try:
            fresh = console.nodes()
            for node_id in [host, core_a, core_b]:
                node = node_by_id(fresh, node_id) or {}
                result.warnings.append(
                    f"node {node_id}: status={node.get('status')} "
                    f"last_seen={node.get('last_seen')} "
                    f"snapshot_ts={node_snapshot_ts(fresh, node_id)}"
                )
            now = time.time()
            recent = [
                entry
                for entry in console.audit()
                if int(entry.get("ts") or 0) >= now - 120
                and entry.get("network") in (net_a, net_b)
            ][:15]
            for entry in recent:
                result.warnings.append(
                    f"audit {entry.get('ts')} {entry.get('action')} "
                    f"{entry.get('node_id')}/{entry.get('network')}"
                )
        except Exception as exc:  # diagnostics must never mask the failure
            result.warnings.append(f"diagnostics failed: {exc}")

    result.elapsed_s = time.time() - started
    return result


def cleanup_protocol(
    console: Console,
    args: argparse.Namespace,
    result: ProtocolResult,
    baseline_shared: str,
    is_last: bool,
) -> None:
    """Remove the two test networks and restore the host shared listener."""
    nets = [args.network_a, args.network_b]
    nodes_ids = [args.host_core, args.client_core_a, args.client_core_b]

    if args.purge_cmd:
        cmd = args.purge_cmd.replace("{networks}", f"{nets[0]},{nets[1]}")
        proc = subprocess.run(cmd, shell=True, capture_output=True, text=True)
        if proc.returncode != 0:
            result.warnings.append(
                f"purge command failed rc={proc.returncode}: "
                f"{(proc.stderr or proc.stdout).strip()[:200]}"
            )
    for node_id in nodes_ids:
        for net in nets:
            res = console.delete_network(node_id, net)
            if not res.get("ok"):
                result.warnings.append(f"cleanup delete {node_id}/{net}: {res.get('error')}")

    restore = args.final_restore if is_last else args.between_restore
    if restore == "baseline":
        value = baseline_shared
    elif restore == "startup":
        value = args.startup_shared
    else:  # clear
        value = ""
    if restore != "none":
        res = console.set_shared_listeners(args.host_core, value)
        if not res.get("ok"):
            result.warnings.append(f"restore shared_listeners {value!r}: {res.get('error')}")
        result.settings["host_restored"] = value

    if not wait_for_absence(console, nodes_ids, nets, timeout=20):
        result.warnings.append("test network instances still present 20s after cleanup")


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------


def parse_args(argv=None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="E2E test: one core public port shared by two networks (udp/ws/quic/tcp)."
    )
    parser.add_argument("--base-url", default="http://127.0.0.1:13110")
    parser.add_argument("--username", default="admin")
    parser.add_argument("--password", default=os.environ.get("DOKI_PASSWORD", ""))
    parser.add_argument("--host-core", default="core1", help="core that owns the shared listener")
    parser.add_argument("--host-ip", default="10.210.0.11", help="underlay ip of the host core")
    parser.add_argument("--client-core-a", default="core2")
    parser.add_argument("--client-core-b", default="core3")
    parser.add_argument("--protocols", default="udp,ws,quic,tcp")
    parser.add_argument(
        "--ports",
        default="",
        help="override ports, e.g. 'udp=21234,ws=21345,quic=21456,tcp=21567'",
    )
    parser.add_argument("--network-a", default="net-a")
    parser.add_argument("--network-b", default="net-b")
    parser.add_argument("--timeout", type=float, default=75.0, help="seconds per protocol")
    parser.add_argument("--poll-interval", type=float, default=2.0)
    parser.add_argument("--stable-rounds", type=int, default=3)
    parser.add_argument(
        "--between-restore",
        choices=["baseline", "clear", "startup", "none"],
        default="baseline",
        help="shared listener state between protocols",
    )
    parser.add_argument(
        "--final-restore",
        choices=["baseline", "clear", "startup"],
        default="clear",
        help="shared listener state after the last protocol",
    )
    parser.add_argument(
        "--startup-shared",
        default="tcp://0.0.0.0:12401",
        help="value used by --*-restore startup (core containers' startup listener)",
    )
    parser.add_argument(
        "--clear-client-listeners",
        action="store_true",
        help="also clear shared listeners on the client cores during a protocol",
    )
    parser.add_argument(
        "--purge-cmd",
        default=os.environ.get("E2E_PURGE_CMD", ""),
        help=(
            "optional shell command run before/after each protocol to forget the test "
            "networks' stored configs (the board API cannot delete config_versions). "
            "The placeholder {networks} is replaced by 'net-a,net-b'."
        ),
    )
    parser.add_argument("--report", default="", help="write a JSON report to this path")
    return parser.parse_args(argv)


def main(argv=None) -> int:
    args = parse_args(argv)
    if not args.password:
        log("ERROR: --password or DOKI_PASSWORD is required")
        return 2

    protocols = [p.strip() for p in args.protocols.split(",") if p.strip()]
    unknown = [p for p in protocols if p not in PROTOCOLS]
    if unknown:
        log(f"ERROR: unsupported protocols {unknown}; choose from {PROTOCOLS}")
        return 2
    ports = dict(DEFAULT_PORTS)
    for part in args.ports.split(","):
        if part.strip():
            name, _, value = part.partition("=")
            ports[name.strip()] = int(value)

    console = Console(args.base_url)
    console.login(args.username, args.password)

    nodes = console.nodes()
    baseline_shared = ""
    host_node = node_by_id(nodes, args.host_core)
    if host_node:
        baseline_shared = (host_node.get("shared_listeners") or "").strip()
    log(
        f"console={args.base_url} host={args.host_core} "
        f"clients={args.client_core_a}/{args.client_core_b} "
        f"protocols={protocols} baseline_shared={baseline_shared!r}"
    )

    results: list[ProtocolResult] = []
    overall_started = time.time()
    for index, proto in enumerate(protocols):
        log(f"=== protocol {proto} (port {ports[proto]}) ===")
        result = run_protocol(
            console, args, proto, ports[proto], index, baseline_shared
        )
        results.append(result)
        log(
            f"  -> {result.status} in {result.elapsed_s:.1f}s "
            f"edges={result.edges}"
        )
        for reason in result.reasons:
            log(f"     reason: {reason}")
        for warning in result.warnings:
            log(f"     warn:   {warning}")
        try:
            cleanup_protocol(
                console, args, result, baseline_shared, index == len(protocols) - 1
            )
        except Exception as exc:  # cleanup must not mask the test result
            result.warnings.append(f"cleanup failed: {exc}")
            log(f"     cleanup error: {exc}")

    total = time.time() - overall_started
    passed = [r for r in results if r.status == "PASS"]
    failed = [r for r in results if r.status != "PASS"]
    log("=== summary ===")
    for result in results:
        detail = ""
        if result.status != "PASS":
            detail = " | " + "; ".join(result.reasons[:3])
        log(
            f"  {result.proto:4s} {result.port:<6d} {result.status:4s} "
            f"{result.elapsed_s:6.1f}s edges={result.edges}{detail}"
        )
    log(f"total {total:.1f}s: {len(passed)} PASS / {len(failed)} FAIL")

    if args.report:
        report = {
            "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            "console": args.base_url,
            "host_core": args.host_core,
            "host_ip": args.host_ip,
            "clients": [args.client_core_a, args.client_core_b],
            "baseline_shared": baseline_shared,
            "total_elapsed_s": round(total, 2),
            "results": [r.as_dict() for r in results],
        }
        with open(args.report, "w", encoding="utf-8") as handle:
            json.dump(report, handle, indent=2, ensure_ascii=False)
        log(f"report written to {args.report}")

    return 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(main())
