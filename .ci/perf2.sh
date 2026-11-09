#!/bin/bash
# Two-node per-protocol bandwidth test on the CI docker network.
# Usage: bash /root/perf2.sh            # runs all protocols
#        bash /root/perf2.sh tcp 12401  # single protocol
# Each run: exactly ONE listener/peer protocol between perf-host and perf-client.
set -u
CFG=/root/dokitier-test/perf2
IMG=dokitier-core:local
NET=dokitier-ci
HOST_IP=10.210.0.101
CLI_IP=10.210.0.102

mkdir -p $CFG/host $CFG/client

write_configs() {
  local proto=$1 port=$2
  cat > $CFG/host/config.toml <<TOML
instance_name = "perf-host"
hostname = "perf-host"
ipv4 = "10.99.0.1/24"
listeners = ["${proto}://0.0.0.0:${port}"]

[network_identity]
network_name = "perf"
network_secret = "perf"

[flags]
no_tun = false
TOML
  cat > $CFG/client/config.toml <<TOML
instance_name = "perf-client"
hostname = "perf-client"
ipv4 = "10.99.0.2/24"

[network_identity]
network_name = "perf"
network_secret = "perf"

[[peer]]
uri = "${proto}://${HOST_IP}:${port}"

[flags]
no_tun = false
TOML
}

start_nodes() {
  docker rm -f perf-host perf-client >/dev/null 2>&1
  docker run -d --name perf-host --network $NET --cap-add NET_ADMIN --device /dev/net/tun \
    --ip $HOST_IP -v /etc/ssl/certs:/etc/ssl/certs:ro -v $CFG/host:/etc/dokitier $IMG \
    /usr/local/bin/dokitier-core -c /etc/dokitier/config.toml >/dev/null
  docker run -d --name perf-client --network $NET --cap-add NET_ADMIN --device /dev/net/tun \
    --ip $CLI_IP -v /etc/ssl/certs:/etc/ssl/certs:ro -v $CFG/client:/etc/dokitier $IMG \
    /usr/local/bin/dokitier-core -c /etc/dokitier/config.toml >/dev/null
}

wait_ready() {
  local HPID CPID
  for i in $(seq 1 30); do
    HPID=$(docker inspect -f "{{.State.Pid}}" perf-host 2>/dev/null)
    CPID=$(docker inspect -f "{{.State.Pid}}" perf-client 2>/dev/null)
    [ -n "$HPID" ] && [ -n "$CPID" ] || { sleep 1; continue; }
    nsenter -t $HPID -n ip addr show tun0 >/dev/null 2>&1 || { sleep 1; continue; }
    nsenter -t $CPID -n ip addr show tun0 >/dev/null 2>&1 || { sleep 1; continue; }
    if nsenter -t $HPID -n ping -c1 -W1 10.99.0.2 >/dev/null 2>&1; then
      echo "$HPID $CPID"
      return 0
    fi
    sleep 1
  done
  return 1
}

iperf_unidir() { # $1 server-pid, $2 client-pid, $3 label, $4 server-vip
  nsenter -t $1 -n /usr/bin/iperf3 -s -1 -p 5201 >/dev/null 2>&1 &
  local SPID=$!
  sleep 0.5
  local OUT RATE RETR
  OUT=$(nsenter -t $2 -n /usr/bin/iperf3 -c $4 -p 5201 -t 5 2>&1)
  kill $SPID 2>/dev/null
  RATE=$(echo "$OUT" | grep -E "sec.*sender" | awk '{for(i=1;i<=NF;i++) if($i ~ /bits\/sec$/) print $(i-1)}' | head -1)
  RETR=$(echo "$OUT" | grep -E "sec.*sender" | awk '{print $(NF-1)}' | head -1)
  echo "$3 RESULT ${RATE:-NA} retr=${RETR:-?}"
}

iperf_pair() { # $1 server-pid, $2 client-pid, $3 label, $4 server-vip
  nsenter -t $1 -n /usr/bin/iperf3 -s -1 -p 5201 >/dev/null 2>&1 &
  local SPID=$!
  sleep 0.5
  local OUT
  OUT=$(nsenter -t $2 -n /usr/bin/iperf3 -c $4 -p 5201 --bidir -t 5 2>&1)
  kill $SPID 2>/dev/null
  # iperf3 >=3.16 with --bidir labels roles TX-C / RX-C; the final "sender"
  # rows carry the retransmit counts, so parse those.
  local UP DL RETR_UP RETR_DL
  UP=$(echo "$OUT" | grep -E "TX-C.*sec.*sender" | awk '{for(i=1;i<=NF;i++) if($i ~ /bits\/sec$/) print $(i-1)}' | head -1)
  DL=$(echo "$OUT" | grep -E "RX-C.*sec.*sender" | awk '{for(i=1;i<=NF;i++) if($i ~ /bits\/sec$/) print $(i-1)}' | head -1)
  RETR_UP=$(echo "$OUT" | grep -E "TX-C.*sec.*sender" | awk '{print $(NF-1)}' | head -1)
  RETR_DL=$(echo "$OUT" | grep -E "RX-C.*sec.*sender" | awk '{print $(NF-1)}' | head -1)
  echo "$3 RESULT upload(client->server)=${UP:-NA} download(server->client)=${DL:-NA} retr=${RETR_UP:-?}/${RETR_DL:-?}"
  echo "$OUT" | grep -E "sec .*sender|sec .*receiver" | tail -4
}

run_proto() {
  local proto=$1 port=$2
  echo "================ $proto://0.0.0.0:$port ================"
  write_configs $proto $port
  start_nodes
  local PIDS
  if ! PIDS=$(wait_ready); then
    echo "$proto: nodes not ready (tun/ping timeout)"
    docker logs --tail 5 perf-host 2>&1 | head -5
    return
  fi
  read HPID CPID <<< "$PIDS"
  local MTU_H MTU_C
  MTU_H=$(nsenter -t $HPID -n ip link show tun0 | grep -o "mtu [0-9]*" | head -1)
  MTU_C=$(nsenter -t $CPID -n ip link show tun0 | grep -o "mtu [0-9]*" | head -1)
  echo "tun0 $MTU_H (host) / $MTU_C (client)"
  echo "--- unidir client->host (iperf server on HOST) ---"
  iperf_unidir $HPID $CPID "c2h" 10.99.0.1
  echo "--- unidir host->client (iperf server on CLIENT) ---"
  iperf_unidir $CPID $HPID "h2c" 10.99.0.2
  echo "--- bidir (both directions simultaneously) ---"
  iperf_pair $HPID $CPID "AB" 10.99.0.1
}

if [ $# -ge 2 ]; then
  run_proto $1 $2
else
  run_proto tcp 12401
  run_proto udp 12405
  run_proto ws 12402
  run_proto wss 12403
  run_proto quic 12404
fi
echo "ALL_DONE"
