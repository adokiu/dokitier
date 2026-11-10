#!/bin/sh
# dokitier core one-line installer (POSIX sh; works on systemd distros and Alpine/OpenRC)
# Usage: wget -qO- <this-url> | sudo sh -s -- -e wss://console/api/agent/ws -t <token> -n <node-id>
set -eu

REPO="adokiu/dokitier"
DEFAULT_INSTALL_DIR="/opt/dokitier"
DEFAULT_SERVICE="dokitier-core"
ENDPOINT=""
TOKEN=""
NODE_ID=""
VERSION="latest"
GH_PROXY=""
INSTALL_DIR="$DEFAULT_INSTALL_DIR"
SERVICE="$DEFAULT_SERVICE"
INTERVAL="1"
INSECURE=0
NO_SERVICE=0

usage() {
  cat <<'USAGE'
  -e <endpoint>        console agent endpoint, e.g. wss://host/api/agent/ws or quic://host:53
  -t <token>           node token
  -n <node_id>         node id registered in console
  --version <ver>      release tag or "latest" (default: latest)
  --github-proxy <url> proxy prefix for github downloads
  --install-dir <dir>  install directory (default: /opt/dokitier)
  --service-name <n>   service name for systemd/OpenRC (default: dokitier-core)
  -i <seconds>         report interval seconds (default: 1)
  --insecure           skip TLS certificate verification when downloading
  --no-service         only install the binary and config (no service)
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    -e) ENDPOINT="$2"; shift 2 ;;
    -t) TOKEN="$2"; shift 2 ;;
    -n) NODE_ID="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --github-proxy) GH_PROXY="$2"; shift 2 ;;
    --install-dir) INSTALL_DIR="$2"; shift 2 ;;
    --service-name) SERVICE="$2"; shift 2 ;;
    -i) INTERVAL="$2"; shift 2 ;;
    --insecure) INSECURE=1; shift ;;
    --no-service) NO_SERVICE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1"; usage; exit 1 ;;
  esac
done

if [ -z "$ENDPOINT" ] || [ -z "$TOKEN" ] || [ -z "$NODE_ID" ]; then
  echo "missing required options: -e endpoint, -t token, -n node_id"
  usage
  exit 1
fi

arch="$(uname -m)"
case "$arch" in
  x86_64|amd64) asset="dokitier-core-amd64" ;;
  aarch64|arm64) asset="dokitier-core-arm64" ;;
  *) echo "unsupported architecture: $arch"; exit 1 ;;
esac

if [ "$VERSION" = "latest" ]; then
  url="https://github.com/${REPO}/releases/latest/download/${asset}"
else
  url="https://github.com/${REPO}/releases/download/${VERSION}/${asset}"
fi
if [ -n "$GH_PROXY" ]; then
  url="${GH_PROXY%/}/${url}"
fi

download() {
  src="$1"; dst="$2"
  if command -v curl >/dev/null 2>&1; then
    if [ "$INSECURE" = "1" ]; then
      curl -kfsSL "$src" -o "$dst"
    else
      curl -fsSL "$src" -o "$dst"
    fi
  elif command -v wget >/dev/null 2>&1; then
    if [ "$INSECURE" = "1" ]; then
      wget -q --no-check-certificate -O "$dst" "$src"
    else
      wget -q -O "$dst" "$src"
    fi
  else
    echo "curl or wget is required"; exit 1
  fi
}

echo "==> downloading $asset ($VERSION)"
mkdir -p "$INSTALL_DIR"
shift_tmp="$(mktemp)"
download "$url" "$shift_tmp"
install -m0755 "$shift_tmp" "$INSTALL_DIR/dokitier-core"
rm -f "$shift_tmp"

echo "==> writing $INSTALL_DIR/config.toml"
cat > "$INSTALL_DIR/config.toml" <<CONF
[controller]
url = "${ENDPOINT}"
node_id = "${NODE_ID}"
token = "${TOKEN}"
interval_secs = ${INTERVAL}

[flags]
no_tun = false
CONF

if [ "$NO_SERVICE" = "1" ]; then
  echo "==> --no-service: binary and config installed in ${INSTALL_DIR}"
  exit 0
fi

# Detect the init system: systemd, OpenRC (Alpine), or none.
detect_init() {
  if [ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1; then
    echo systemd
  elif command -v rc-service >/dev/null 2>&1 && command -v rc-update >/dev/null 2>&1; then
    echo openrc
  else
    echo none
  fi
}

INIT="$(detect_init)"
echo "==> detected init system: ${INIT}"

if [ "$INIT" = "systemd" ]; then
  echo "==> installing systemd service ${SERVICE}"
  cat > "/etc/systemd/system/${SERVICE}.service" <<UNIT
[Unit]
Description=dokitier core (${NODE_ID})
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
WorkingDirectory=${INSTALL_DIR}
ExecStart=${INSTALL_DIR}/dokitier-core -c ${INSTALL_DIR}/config.toml
Restart=always
RestartSec=2
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
UNIT

  systemctl daemon-reload
  systemctl enable "$SERVICE" >/dev/null 2>&1 || true
  systemctl restart "$SERVICE"
  sleep 2
  systemctl is-active "$SERVICE" && echo "==> dokitier core installed and running (${SERVICE})"
elif [ "$INIT" = "openrc" ]; then
  echo "==> installing OpenRC service ${SERVICE}"
  # OpenRC does not accept dots in service names.
  RC_SERVICE="$(echo "$SERVICE" | tr . -)"
  cat > "/etc/init.d/${RC_SERVICE}" <<UNIT
#!/sbin/openrc-run
description="dokitier core (${NODE_ID})"
command="${INSTALL_DIR}/dokitier-core"
command_args="-c ${INSTALL_DIR}/config.toml"
command_background="yes"
directory="${INSTALL_DIR}"
pidfile="/run/${RC_SERVICE}.pid"
output_log="/var/log/${RC_SERVICE}.log"
error_log="/var/log/${RC_SERVICE}.log"
rc_ulimit="-n 65536"

depend() {
  need net
  use dns
}
UNIT
  chmod 0755 "/etc/init.d/${RC_SERVICE}"
  rc-update add "${RC_SERVICE}" default >/dev/null 2>&1 || true
  rc-service "${RC_SERVICE}" restart
  sleep 2
  if rc-service "${RC_SERVICE}" status >/dev/null 2>&1; then
    echo "==> dokitier core installed and running (${RC_SERVICE})"
  else
    echo "==> WARNING: ${RC_SERVICE} did not report running; check /var/log/${RC_SERVICE}.log" >&2
    exit 1
  fi
else
  echo "==> no systemd or OpenRC detected; installed binary and config only in ${INSTALL_DIR}" >&2
  echo "==> create a service definition for ${INSTALL_DIR}/dokitier-core -c ${INSTALL_DIR}/config.toml manually" >&2
  exit 0
fi
