#!/bin/bash
# Compare per-second syscall rates of dokitier instances during ws vs tcp
# tunnel iperf runs. Writes results to /root/io_rates.txt
set -u
CFG=/root/dokitier-test/perf2
OUT=/root/io_rates.txt
: > $OUT

set_listeners() {
  local proto=$1 port=$2
  sed -i "s|listeners = .*|listeners = [\"${proto}://0.0.0.0:${port}\"]|" $CFG/host/config.toml
  sed -i "s|uri = .*|uri = \"${proto}://10.210.0.101:${port}\"|" $CFG/client/config.toml
  for f in host client; do sed -i "s/multi_thread_count = .*/multi_thread_count = 2/" $CFG/$f/config.toml; done
}

io_snap() { awk '/^syscr|^syscw/{printf "%s ", $2}' /proc/$1/io; }

test_proto() {
  local proto=$1 port=$2
  set_listeners $proto $port
  docker restart perf-host perf-client >/dev/null
  sleep 12
  local HPID CPID
  HPID=$(docker inspect -f "{{.State.Pid}}" perf-host)
  CPID=$(docker inspect -f "{{.State.Pid}}" perf-client)
  nsenter -t $HPID -n ping -c1 -W1 10.99.0.2 >/dev/null 2>&1 || { echo "$proto NOT-READY" >> $OUT; return; }
  nsenter -t $HPID -n /usr/bin/iperf3 -s -1 -p 5201 >/dev/null 2>&1 &
  sleep 0.4
  nsenter -t $CPID -n /usr/bin/iperf3 -c 10.99.0.1 -p 5201 -t 10 > /tmp/o.out 2>&1 &
  local IP=$!
  sleep 1
  local A AH AC
  A=$(io_snap $HPID)
  AC=$(io_snap $CPID)
  sleep 5
  local B BC
  B=$(io_snap $HPID)
  BC=$(io_snap $CPID)
  wait $IP
  local RATE
  RATE=$(grep sender /tmp/o.out | head -1 | awk '{for(i=1;i<=NF;i++) if($i ~ /bits\/sec$/) print $(i-1)}')
  python3 - "$proto" "$RATE" "$A" "$B" "$AC" "$BC" >> $OUT << 'PYEOF'
import sys
proto, rate = sys.argv[1], sys.argv[2]
def parse(s):
    parts = s.split()
    return float(parts[0]), float(parts[1])  # syscr, syscw
ah, aw = parse(sys.argv[3]); bh, bw = parse(sys.argv[4])
ac, cw = parse(sys.argv[5]); bc, dw = parse(sys.argv[6])
print(f"{proto} rate={rate}Mbps | host read/s={bh-ah:.0f} write/s={bw-aw:.0f} | client read/s={bc-ac:.0f} write/s={dw-cw:.0f}")
PYEOF
}

test_proto ws 12402
test_proto tcp 12401
test_proto wss 12403
echo IO_DONE >> $OUT
