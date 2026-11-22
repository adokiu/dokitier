#!/usr/bin/env bash
# Throughput under an emulated WAN: netem loss + delay on the container underlay
# interface (eth0). Mirrors the "link cap, then collapse" behaviour reported on
# real links so CUBIC/BBR and MTU choices can be compared on the CICD host.
#   ./lossy.sh <scheme> [MB] [LOSS] [DELAY_MS]
set -uo pipefail

ROOT=$(cd "$(dirname "$0")" && pwd)
SCHEME=${1:-quic}
MB=${2:-512}
LOSS=${3:-1%}
DELAY=${4:-15ms}
CORE_ENV=${CORE_ENV:-}
LIMIT=${LIMIT:-1000}
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
  nsp=$(docker inspect -f '{{.State.Pid}}' "doki-n$n")
  # A deep netem queue turns the loss test into a bufferbloat test (RTT grows
  # to hundreds of ms); keep it small so loss/congestion-control is what is
  # actually measured.
  nsenter -t "$nsp" -n tc qdisc add dev eth0 root netem limit "$LIMIT" loss "$LOSS" delay "$DELAY" \
    >/dev/null 2>&1 || echo "netem failed for doki-n$n" >&2
  # shellcheck disable=SC2086
  docker exec $CORE_ENV -d "doki-n$n" /usr/local/bin/dokitier-core -c /etc/dokitier/config.toml
done

p1=$(docker inspect -f '{{.State.Pid}}' "$N1")
p2=$(docker inspect -f '{{.State.Pid}}' "$N2")
for _ in $(seq 1 120); do
  nsenter -t "$p1" -n ping -c1 -W1 "$VPN2" >/dev/null 2>&1 && break
  sleep 1
done
echo "tunnel up ($SCHEME) netem loss=$LOSS delay=$DELAY" >&2
sleep 2

fwd=$( (nsenter -t "$p2" -n python3 "$ROOT/tcp_server.py" 5201 >/tmp/srv.out 2>&1 &) ; sleep 1; nsenter -t "$p1" -n python3 "$ROOT/tcp_client.py" "$VPN2" 5201 "$MB" 2>/dev/null || echo 0)
rev=$( (nsenter -t "$p1" -n python3 "$ROOT/tcp_server.py" 5202 >/tmp/srv2.out 2>&1 &) ; sleep 1; nsenter -t "$p2" -n python3 "$ROOT/tcp_client.py" "$VPN1" 5202 "$MB" 2>/dev/null || echo 0)
echo "$SCHEME loss=$LOSS delay=$DELAY: fwd ${fwd:-0} Mbps  rev ${rev:-0} Mbps"
cleanup
