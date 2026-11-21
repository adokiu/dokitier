#!/usr/bin/env bash
# Tunnel throughput bench: two core instances in Docker on the CI runner only
# (no external network access, no production nodes).
#
#   ci/throughput-bench.sh <dokitier-core binary> [schemes...]
#
# Each scheme recreates the containers so the pair is always fresh:
#   n1 172.31.0.2 / overlay 10.240.200.1  (dials the peer)
#   n2 172.31.0.3 / overlay 10.240.200.2  (listens, runs iperf3 -s)
# and measures iperf3 both directions, single and 4 parallel streams.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${1:?usage: throughput-bench.sh <dokitier-core binary> [schemes...]}
shift || true
SCHEMES=("${@:-tcp udp quic}")
# shellcheck disable=SC2206
SCHEMES=(${SCHEMES[@]})
DURATION=${DURATION:-15}
THRESHOLD_MBPS=${THRESHOLD_MBPS:-300}
IMAGE=${IMAGE:-dokitier-throughput:local}
NET=${NET:-dokitier-throughput-net}
N1=bench-n1
N2=bench-n2
LOG_DIR=${LOG_DIR:-$ROOT/bench-logs}
mkdir -p "$LOG_DIR"

case "$BIN" in /*) ;; *) BIN="$ROOT/$BIN" ;; esac
[ -x "$BIN" ] || { echo "binary not executable: $BIN" >&2; exit 1; }

cleanup() {
  docker rm -f "$N1" "$N2" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
}
trap cleanup EXIT

cleanup
docker network create --subnet 172.31.0.0/24 "$NET" >/dev/null
docker build -q -f "$ROOT/.ci/throughput/Dockerfile" -t "$IMAGE" "$ROOT" >/dev/null

start_node() {
  local name=$1 ip=$2 cfg=$3
  docker run -d --name "$name" --network "$NET" --ip "$ip" \
    --privileged --device /dev/net/tun \
    -v "$BIN:/usr/local/bin/dokitier-core:ro" \
    -v "$cfg:/etc/dokitier/config.toml:ro" \
    "$IMAGE" >/dev/null
}

dump_logs() {
  echo "---- $N1 log ----"; docker logs "$N1" 2>&1 | tail -60 || true
  echo "---- $N2 log ----"; docker logs "$N2" 2>&1 | tail -60 || true
}

wait_tunnel() {
  for _ in $(seq 1 90); do
    if docker exec "$N1" ping -c1 -W1 10.240.200.2 >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  return 1
}

# Print "<mbps>" for one iperf3 run.
iperf_run() {
  local parallel=$1 extra=${2:-}
  docker exec "$N1" iperf3 -c 10.240.200.2 -t "$DURATION" -P "$parallel" \
    $extra --json 2>/dev/null |
    python3 -c 'import sys,json; print(f"{json.load(sys.stdin)[\"end\"][\"sum_received\"][\"bits_per_second\"]/1e6:.1f}")'
}

FAIL=0
declare -a ROWS=()
for scheme in "${SCHEMES[@]}"; do
  echo "=== scheme: $scheme ==="
  docker rm -f "$N1" "$N2" >/dev/null 2>&1 || true
  start_node "$N1" 172.31.0.2 "$ROOT/.ci/throughput/n1-$scheme.toml"
  start_node "$N2" 172.31.0.3 "$ROOT/.ci/throughput/n2-$scheme.toml"
  docker exec -d "$N1" /usr/local/bin/dokitier-core -c /etc/dokitier/config.toml
  docker exec -d "$N2" /usr/local/bin/dokitier-core -c /etc/dokitier/config.toml

  if ! wait_tunnel; then
    echo "::error::tunnel did not come up for scheme $scheme"
    dump_logs
    FAIL=1
    continue
  fi
  docker exec -d "$N2" iperf3 -s -p 5201 >/dev/null 2>&1 || true
  sleep 1

  fwd1=$(iperf_run 1) || fwd1=0
  fwd4=$(iperf_run 4) || fwd4=0
  rev1=$(iperf_run 1 -R) || rev1=0
  rev4=$(iperf_run 4 -R) || rev4=0
  docker exec "$N2" pkill iperf3 >/dev/null 2>&1 || true

  printf '%-5s  fwd P1 %8s Mbps  fwd P4 %8s Mbps  rev P1 %8s Mbps  rev P4 %8s Mbps\n' \
    "$scheme" "$fwd1" "$fwd4" "$rev1" "$rev4"
  ROWS+=("$scheme $fwd1 $fwd4 $rev1 $rev4")

  if [ "$scheme" != "tcp" ]; then
    for value in "$fwd4" "$rev4"; do
      if python3 -c "import sys; sys.exit(0 if float('$value') >= $THRESHOLD_MBPS else 1)"; then
        :
      else
        echo "::error::$scheme direction throughput ${value} Mbps below ${THRESHOLD_MBPS} Mbps"
        FAIL=1
      fi
    done
  fi
  docker logs "$N1" 2>&1 | grep -E "ERROR|panic" | tail -5 || true
  docker logs "$N2" 2>&1 | grep -E "ERROR|panic" | tail -5 || true
  docker logs "$N1" > "$LOG_DIR/$scheme-n1.log" 2>&1 || true
  docker logs "$N2" > "$LOG_DIR/$scheme-n2.log" 2>&1 || true
done

echo
echo "=== summary (Mbps) ==="
printf '%-5s %10s %10s %10s %10s\n' scheme fwd_P1 fwd_P4 rev_P1 rev_P4
for row in "${ROWS[@]}"; do printf '%-5s %10s %10s %10s %10s\n' $row; done
echo "threshold (udp/quic P4 each direction): ${THRESHOLD_MBPS} Mbps"

exit "$FAIL"
