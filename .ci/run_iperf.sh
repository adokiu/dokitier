#!/bin/bash
# net-ci intra-group four-protocol bidir iperf (recreated; matches /tmp/run_iperf.sh baseline)
set -u
CFG=/root/dokitier-test/cfg
get_vip() {  # $1 = container name
  local PID=$(docker inspect -f "{{.State.Pid}}" "$1")
  nsenter -t $PID -n ip -4 addr show tun0 2>/dev/null | grep -oE "inet [0-9.]+" | head -1 | cut -d" " -f2
}
iperf_link() {  # $1 src-container, $2 dst-container, $3 label
  local SPID DPID SVIP
  SPID=$(docker inspect -f "{{.State.Pid}}" "$2")
  DPID=$(docker inspect -f "{{.State.Pid}}" "$1")
  SVIP=$(get_vip "$2")
  nsenter -t $SPID -n /usr/bin/iperf3 -s -1 -p 5201 >/dev/null 2>&1 &
  local SP=$!
  sleep 0.5
  local OUT
  OUT=$(nsenter -t $DPID -n /usr/bin/iperf3 -c $SVIP -p 5201 --bidir -t 5 2>&1)
  kill $SP 2>/dev/null
  local BU BD
  BU=$(echo "$OUT" | grep -E "TX-C.*sec.*sender" | awk '{for(i=1;i<=NF;i++) if($i ~ /bits\/sec$/) print $(i-1)}' | head -1)
  BD=$(echo "$OUT" | grep -E "RX-C.*sec.*sender" | awk '{for(i=1;i<=NF;i++) if($i ~ /bits\/sec$/) print $(i-1)}' | head -1)
  echo "$3 RESULT c2s=${BU:-NA} s2c=${BD:-NA} Mbps"
}
echo "=== net-ci intra-group four-protocol bidir (build92) ==="
iperf_link dokitier-ci-core-1 dokitier-ci-core-2 "tcp  core1->core2"
iperf_link dokitier-ci-core-1 dokitier-ci-core-3 "ws   core1->core3"
iperf_link dokitier-ci-core-1 dokitier-ci-core-4 "wss  core1->core4"
iperf_link dokitier-ci-core-2 dokitier-ci-core-3 "quic core2->core3"
iperf_link dokitier-ci-core-2 dokitier-ci-core-4 "quic core2->core4"
echo DONE
