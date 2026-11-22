#!/usr/bin/env bash
# Simultaneous bidirectional throughput between the two CICD bench containers.
#   .ci/throughput/bidir.sh <scheme> [MB per direction]
set -uo pipefail

ROOT=$(cd "$(dirname "$0")" && pwd)
SCHEME=${1:-quic}
MB=${2:-768}
IMAGE=${IMAGE:-dokitier-test:local}
NET=${NET:-doki-bench}
N1=doki-n1
N2=doki-n2
VPN1=10.240.200.1
VPN2=10.240.200.2
PORT_A=5201 # server on n1, client on n2 -> VPN1
PORT_B=5202 # server on n2, client on n1 -> VPN2

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
  docker exec -d "doki-n$n" /usr/local/bin/dokitier-core -c /etc/dokitier/config.toml
done

p1=$(docker inspect -f '{{.State.Pid}}' "$N1")
p2=$(docker inspect -f '{{.State.Pid}}' "$N2")
for _ in $(seq 1 60); do
  nsenter -t "$p1" -n ping -c1 -W1 "$VPN2" >/dev/null 2>&1 && break
  sleep 1
done
echo "tunnel up ($SCHEME)" >&2
sleep 1

nsenter -t "$p1" -n python3 "$ROOT/tcp_server.py" "$PORT_A" >/tmp/srv_a.out 2>&1 &
nsenter -t "$p2" -n python3 "$ROOT/tcp_server.py" "$PORT_B" >/tmp/srv_b.out 2>&1 &
sleep 1
nsenter -t "$p1" -n python3 "$ROOT/tcp_client.py" "$VPN2" "$PORT_B" "$MB" >/tmp/cli_a.out 2>&1 &
cli_a=$!
nsenter -t "$p2" -n python3 "$ROOT/tcp_client.py" "$VPN1" "$PORT_A" "$MB" >/tmp/cli_b.out 2>&1 &
cli_b=$!
wait "$cli_a" 2>/dev/null || true
wait "$cli_b" 2>/dev/null || true

a=$(cat /tmp/cli_a.out 2>/dev/null)
b=$(cat /tmp/cli_b.out 2>/dev/null)
echo "$SCHEME simultaneous: n1->n2 ${a:-0} Mbps   n2->n1 ${b:-0} Mbps"
python3 - "$a" "$b" <<'PY'
import sys
a, b = (float(x or 0) for x in sys.argv[1:3])
print(f"{a:.1f} {b:.1f} total={a + b:.1f} Mbps")
PY
