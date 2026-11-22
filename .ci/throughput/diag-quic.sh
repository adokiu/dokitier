#!/usr/bin/env bash
# One-off QUIC diagnostic on the CICD host: two containers, one transfer, then
# dump the per-second QUIC_STATS lines from both sides.
set -uo pipefail

ROOT=$(cd "$(dirname "$0")" && pwd)
MB=${MB:-512}
SCHEME=${SCHEME:-quic}
IMAGE=${IMAGE:-dokitier-test:local}
NET=${NET:-doki-bench}
N1=doki-n1
N2=doki-n2
VPN1=10.240.200.1
VPN2=10.240.200.2

cleanup() {
  docker rm -f "$N1" "$N2" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
}
cleanup
docker network create --subnet 172.31.0.0/24 "$NET" >/dev/null

for n in 1 2; do
  docker run -d --name "doki-n$n" --network "$NET" --ip "172.31.0.$((n + 1))" \
    --privileged --device /dev/net/tun \
    -v "$ROOT/dokitier-core:/usr/local/bin/dokitier-core:ro" \
    -v "$ROOT/n$n-$SCHEME.toml:/etc/dokitier/config.toml:ro" \
    "$IMAGE" sleep infinity >/dev/null
  docker exec -e DOKITIER_QUIC_STATS=1 -d "doki-n$n" \
    sh -c "/usr/local/bin/dokitier-core -c /etc/dokitier/config.toml >/tmp/core.log 2>&1"
done

p1=$(docker inspect -f '{{.State.Pid}}' "$N1")
p2=$(docker inspect -f '{{.State.Pid}}' "$N2")
for _ in $(seq 1 60); do
  nsenter -t "$p1" -n ping -c1 -W1 "$VPN2" >/dev/null 2>&1 && break
  sleep 1
done
echo "tunnel up" >&2

nsenter -t "$p2" -n python3 "$ROOT/tcp_server.py" 5201 >/tmp/srv.out 2>&1 &
sleep 1
nsenter -t "$p1" -n python3 "$ROOT/tcp_client.py" "$VPN2" 5201 "$MB"
sleep 2

echo "=== n1 stats"
docker exec "$N1" grep QUIC_STATS /tmp/core.log | tail -10
echo "=== n2 stats"
docker exec "$N2" grep QUIC_STATS /tmp/core.log | tail -10
echo "=== n1 errors"
docker exec "$N1" grep -E "ERROR|WARN" /tmp/core.log | tail -5
