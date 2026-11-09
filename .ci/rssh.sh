#!/usr/bin/env bash
# ssh/scp helper using the CI password (expect is available, sshpass is not)
# usage: rssh.sh "<remote command>"   |   rssh.sh --scp <local> <remote>
CI=10.240.200.2
PW='20060818Dd@'
if [ "$1" = "--scp" ]; then
  exec expect -c "
    set timeout 300
    spawn scp -o StrictHostKeyChecking=no \"$2\" root@$CI:\"$3\"
    expect {
      \"*assword:*\" { send \"$PW\r\"; exp_continue }
      eof
    }
  "
else
  exec expect -c "
    set timeout 600
    spawn ssh -o StrictHostKeyChecking=no root@$CI -- \"$1\"
    expect {
      \"*assword:*\" { send \"$PW\r\"; exp_continue }
      eof
    }
  "
fi
