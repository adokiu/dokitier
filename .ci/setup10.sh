#!/usr/bin/env bash
# (Re)create the 10 core containers with persistent, writable config dirs.
# Master config per core: uuid + hostname + listeners (single listener field;
# no shared_listeners / mapped_listeners — advertisement is runtime-only).
set -euo pipefail
WS=${WS:-ws://10.240.13.5:13110/api/agent/ws}
IMAGE=${IMAGE:-dokitier-core:local}
NET=${NET:-dokitier-ci}
CFG=${CFG:-/root/dokitier-test/cfg}
CONSOLE=${CONSOLE:-http://10.240.13.5:13110}
ADMIN_PASS=${ADMIN_PASS:-fmj2BvyY34LoRJo2ar}

for i in $(seq 1 10); do docker rm -f "dokitier-ci-core-$i" >/dev/null 2>&1 || true; done
docker network rm "$NET" >/dev/null 2>&1 || true
docker network create --subnet 10.210.0.0/24 "$NET" >/dev/null
rm -rf "$CFG"
for i in $(seq 1 10); do
  mkdir -p "$CFG/core$i"
  cat > "$CFG/core$i/config.toml" <<TOML
instance_name = "ci-core$i"
instance_id = "00000000-0000-4000-8000-0000000000$(printf '%02d' "$i")"
hostname = "core$i"
listeners = ["tcp://0.0.0.0:12401", "ws://0.0.0.0:12402", "wss://0.0.0.0:12403", "quic://0.0.0.0:12404"]

[controller]
url = "$WS"
node_id = "core$i"
token = "ci-token-$i"

[flags]
no_tun = false
TOML
  docker run -d --name "dokitier-ci-core-$i" --network "$NET" --cap-add NET_ADMIN --device /dev/net/tun \
    --ip "10.210.0.$((10 + i))" -v /etc/ssl/certs:/etc/ssl/certs:ro -v "$CFG/core$i:/etc/dokitier" "$IMAGE" \
    /usr/local/bin/dokitier-core -c /etc/dokitier/config.toml >/dev/null
done

TMP=$(mktemp -d)
curl -fsS -c "$TMP/cookie" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$ADMIN_PASS\"}" "$CONSOLE/api/login" >/dev/null
for n in $(seq 1 60); do
  c=$(curl -fsS -b "$TMP/cookie" "$CONSOLE/api/board/nodes" | \
    python3 -c 'import sys,json;print(sum(1 for n in json.load(sys.stdin)["nodes"] if n.get("status")=="online" and n["node_id"].startswith("core")))')
  if [ "$c" -ge 10 ]; then echo "10 cores online after ${n}s"; break; fi
  [ "$n" = 60 ] && { echo "only $c online"; docker logs --tail 20 dokitier-ci-core-1; exit 1; }
  sleep 1
done
rm -rf "$TMP"
echo "containers ready; config changes from now on are hot pushes"
