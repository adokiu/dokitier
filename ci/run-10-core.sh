#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
IMAGE=${IMAGE:-dokitier-ci:local}
NET=dokitier-ci
PG=dokitier-ci-pg
CONSOLE=dokitier-ci-console
TMP=$(mktemp -d)
cleanup() {
  for i in $(seq 1 10); do docker rm -f "dokitier-ci-core-$i" >/dev/null 2>&1 || true; done
  docker rm -f "$CONSOLE" "$PG" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
  rm -rf "$TMP"
}
trap cleanup EXIT

docker build -f "$ROOT/docker/Dockerfile" -t "$IMAGE" "$ROOT"
docker network create "$NET" >/dev/null
docker run -d --name "$PG" --network "$NET" -e POSTGRES_PASSWORD=dokitier -e POSTGRES_DB=dokitier postgres:16-alpine >/dev/null

ADD_ARGS=()
for i in $(seq 1 10); do ADD_ARGS+=(--add-node "core$i" "ci-token-$i"); done
docker run -d --name "$CONSOLE" --network "$NET" -p 13110:13110 \
  "$IMAGE" /usr/local/bin/dokitier-console \
  --listen 0.0.0.0:13110 --agent-quic 0.0.0.0:13113 \
  --db 'postgres://postgres:dokitier@dokitier-ci-pg:5432/dokitier?sslmode=disable' \
  --webui /opt/webui "${ADD_ARGS[@]}" >/dev/null

for _ in $(seq 1 40); do
  curl -fsS http://127.0.0.1:13110/healthz >/dev/null 2>&1 && break
  sleep 1
done
PASSWORD=$(docker logs "$CONSOLE" 2>&1 | sed -n 's/.*initial admin: admin \/ //p' | tail -1)
[ -n "$PASSWORD" ] || { docker logs "$CONSOLE"; exit 1; }

# board mode: core only knows controller url/token; all network config is pushed later
for i in $(seq 1 10); do
  cfg="$TMP/min-core$i.toml"
  cat > "$cfg" <<TOML
instance_name = "ci-core$i"
instance_id = "00000000-0000-4000-8000-0000000000$(printf '%02d' "$i")"

[controller]
url = "ws://dokitier-ci-console:13110/api/agent/ws"
node_id = "core$i"
token = "ci-token-$i"

[flags]
no_tun = true
TOML
  docker run -d --name "dokitier-ci-core-$i" --network "$NET" \
    -v "$cfg:/etc/dokitier/config.toml:ro" "$IMAGE" \
    /usr/local/bin/dokitier-core -c /etc/dokitier/config.toml >/dev/null
done

login() {
  curl -fsS -c "$TMP/cookie" -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$PASSWORD\"}" \
    http://127.0.0.1:13110/api/login >/dev/null
}
online_count() {
  curl -fsS -b "$TMP/cookie" http://127.0.0.1:13110/api/board/nodes | \
    python3 -c 'import sys,json;print(sum(1 for n in json.load(sys.stdin)["nodes"] if n.get("status")=="online"))'
}

login
for _ in $(seq 1 60); do
  [ "$(online_count)" -ge 10 ] && break
  sleep 1
done

cn=(1 2 3 4); outside=(5 6 7 8)
peer_lines() {
  local self=$1; shift
  for p in "$@"; do
    [ "$p" = "$self" ] && continue
    echo "[[peer]]"
    echo "uri = \"tcp://core${p}:12401\""
  done
}

# console pushes the whole network config to each core
for i in $(seq 1 10); do
  case $i in
    1|2|3|4) group=cn; peers=("${cn[@]}" 9 10) ;;
    5|6|7|8) group=outside; peers=("${outside[@]}" 9 10) ;;
    9) group=relay; peers=(1 5) ;;
    10) group=relay; peers=(2 6) ;;
  esac
  toml="$TMP/push-core$i.toml"
  cat > "$toml" <<TOML
hostname = "core$i"
instance_name = "ci-core$i"
instance_id = "00000000-0000-4000-8000-0000000000$(printf '%02d' "$i")"
p2p_group = "$group"
ipv4 = "10.144.0.$i/24"
listeners = ["tcp://0.0.0.0:12401"]

[network_identity]
network_name = "net-ci"
network_secret = "ci-secret"

$(peer_lines "$i" "${peers[@]}")

[flags]
no_tun = true
TOML
  python3 - "$toml" > "$TMP/payload.json" <<'PY'
import json, sys
toml = open(sys.argv[1]).read()
print(json.dumps({"command": {"kind": "apply_config", "toml": toml}}))
PY
  curl -fsS -b "$TMP/cookie" -H 'Content-Type: application/json' \
    --data @"$TMP/payload.json" \
    "http://127.0.0.1:13110/api/board/nodes/core$i/command" | tee "$TMP/push-$i.json"
  echo
  python3 - "$TMP/push-$i.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r.get("ok"), r
PY
done

echo "waiting for topology..."
for _ in $(seq 1 60); do
  curl -fsS -b "$TMP/cookie" http://127.0.0.1:13110/api/board/topology > "$TMP/topo.json"
  edges=$(python3 -c 'import json,sys;print(len(json.load(open(sys.argv[1]))["graph"]["edges"]))' "$TMP/topo.json")
  [ "$edges" -ge 1 ] && break
  sleep 1
done

curl -fsS -b "$TMP/cookie" http://127.0.0.1:13110/api/board/nodes > "$TMP/nodes.json"
curl -fsS -m 5 -b "$TMP/cookie" http://127.0.0.1:13110/api/board/events > "$TMP/events.txt" || true

python3 - "$TMP/nodes.json" "$TMP/topo.json" "$TMP/events.txt" <<'PY'
import json, sys
nodes = json.load(open(sys.argv[1]))["nodes"]
topo = json.load(open(sys.argv[2]))["graph"]
events = open(sys.argv[3]).read()
online = [n for n in nodes if n.get("status") == "online"]
assert len(nodes) == 10, f"expected 10 nodes, got {len(nodes)}"
assert len(online) == 10, f"expected 10 online, got {len(online)}"
assert len(topo["nodes"]) >= 10, f"topology nodes {len(topo['nodes'])}"
assert len(topo["edges"]) >= 1, "no topology edges"
assert "event: topology" in events, "SSE topology missing"
by_id = {n["peer_id"]: n for n in topo["nodes"]}
names = {n.get("hostname") for n in topo["nodes"]}
for core in [f"core{i}" for i in range(1, 11)]:
    assert core in names, f"missing {core}"
relay_edges = [e for e in topo["edges"] if by_id.get(e["a"], {}).get("hostname") in {"core9", "core10"} or by_id.get(e["b"], {}).get("hostname") in {"core9", "core10"}]
assert relay_edges, "no relay edges"
print(json.dumps({"nodes": len(nodes), "online": len(online), "topology_nodes": len(topo["nodes"]), "edges": len(topo["edges"]), "relay_edges": len(relay_edges)}, ensure_ascii=False))
PY

echo "10-core integration test passed"
