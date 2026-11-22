#!/usr/bin/env bash
# Runs on the CICD host (10.240.200.2) entirely inside Docker: two core
# instances (no console, manual TOML, network key) and a TCP throughput test
# between their overlay/VPN IPs (10.240.200.2 from 10.240.200.1).
#
# Host tooling is used only through `nsenter -n -t <pid>`: the host python3
# performs the transfer inside each container's network namespace, so the
# container image needs nothing but the static core binary.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")" && pwd)
SCHEMES=${SCHEMES:-"tcp udp quic"}
MB=${MB:-512}
IMAGE=${IMAGE:-dokitier-test:local}
NET=${NET:-doki-bench}
N1=doki-n1
N2=doki-n2
VPN1=10.240.200.1
VPN2=10.240.200.2
LOG_DIR=${LOG_DIR:-/root/doki-bench/logs}

cleanup() {
  docker rm -f "$N1" "$N2" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
  pkill -f "tcp_server.py" >/dev/null 2>&1 || true
}
trap cleanup EXIT

mkdir -p "$LOG_DIR"
# Remove leftovers from an interrupted previous run (containers, helper procs).
docker ps -aq --filter name=doki- | xargs -r docker rm -f >/dev/null 2>&1 || true
pkill -f 'doki-bench/tcp_server.py' >/dev/null 2>&1 || true
pkill -f 'doki-bench/tcp_client.py' >/dev/null 2>&1 || true
docker network rm "$NET" >/dev/null 2>&1 || true
docker network create --subnet 172.31.0.0/24 "$NET" >/dev/null

start_node() {
  local name=$1 ip=$2 cfg=$3
  docker run -d --name "$name" --network "$NET" --ip "$ip" \
    --privileged --device /dev/net/tun \
    -v "$ROOT/dokitier-core:/usr/local/bin/dokitier-core:ro" \
    -v "$ROOT/$cfg:/etc/dokitier/config.toml:ro" \
    "$IMAGE" sleep infinity >/dev/null
}

pid_of() { docker inspect -f '{{.State.Pid}}' "$1"; }

wait_tunnel() {
  local pid
  pid=$(pid_of "$N1")
  for _ in $(seq 1 90); do
    if nsenter -t "$pid" -n ping -c1 -W1 "$VPN2" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  return 1
}

vpn_of() {
  case "$1" in
    "$N1") echo "$VPN1" ;;
    "$N2") echo "$VPN2" ;;
  esac
}

# throughput_mbps <server_node> <client_node> -> prints Mbps (empty on error)
throughput_mbps() {
  local srv=$1 cli=$2 spid cpid target
  spid=$(pid_of "$srv")
  cpid=$(pid_of "$cli")
  target=$(vpn_of "$srv")
  nsenter -t "$spid" -n python3 "$ROOT/tcp_server.py" 5201 >"$LOG_DIR/server.out" 2>&1 &
  local server_job=$!
  sleep 1
  local result
  result=$(nsenter -t "$cpid" -n python3 "$ROOT/tcp_client.py" "$target" 5201 "$MB" 2>"$LOG_DIR/client.err" || echo 0)
  wait "$server_job" 2>/dev/null || true
  if [ -z "$result" ] || [ "$result" = "0" ]; then
    echo "  (client->$target failed: $(tail -2 "$LOG_DIR/client.err" 2>/dev/null | tr '\n' ' '); server: $(tail -1 "$LOG_DIR/server.out" 2>/dev/null))" >&2
  fi
  echo "$result"
}

FAIL=0
declare -a ROWS=()
for scheme in $SCHEMES; do
  echo "=== scheme: $scheme ===" >&2
  docker rm -f "$N1" "$N2" >/dev/null 2>&1 || true
  start_node "$N1" 172.31.0.2 "n1-$scheme.toml"
  start_node "$N2" 172.31.0.3 "n2-$scheme.toml"
  docker exec -d "$N1" /usr/local/bin/dokitier-core -c /etc/dokitier/config.toml
  docker exec -d "$N2" /usr/local/bin/dokitier-core -c /etc/dokitier/config.toml

  if ! wait_tunnel; then
    echo "::error::tunnel did not come up for $scheme" >&2
    docker logs "$N1" >"$LOG_DIR/$scheme-n1.log" 2>&1 || true
    docker logs "$N2" >"$LOG_DIR/$scheme-n2.log" 2>&1 || true
    FAIL=1
    continue
  fi
  sleep 2
  fwd=$(throughput_mbps "$N2" "$N1")
  rev=$(throughput_mbps "$N1" "$N2")
  ROWS+=("$scheme $fwd $rev")
  printf '%-5s fwd %10s Mbps   rev %10s Mbps\n' "$scheme" "$fwd" "$rev"
  docker logs "$N1" >"$LOG_DIR/$scheme-n1.log" 2>&1 || true
  docker logs "$N2" >"$LOG_DIR/$scheme-n2.log" 2>&1 || true
done

echo
printf '%-5s %12s %12s\n' scheme fwd_Mbps rev_Mbps
for row in "${ROWS[@]}"; do printf '%-5s %12s %12s\n' $row; done

exit "$FAIL"
