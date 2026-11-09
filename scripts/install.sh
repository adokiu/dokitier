#!/bin/bash
# dokitier core one-line installer
# Usage: wget -qO- <this-url> | sudo bash -s -- -e wss://console/api/agent/ws -t <token> -n <node-id>
set -euo pipefail

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

usage() {
  cat <<'USAGE'
  -e <endpoint>        console agent endpoint, e.g. wss://host/api/agent/ws or quic://host:53
  -t <token>           node token
  -n <node_id>         node id registered in console
  --version <ver>      release tag or "latest" (default: latest)
  --github-proxy <url> proxy prefix for github downloads
  --install-dir <dir>  install directory (default: /opt/dokitier)
  --service-name <n>   systemd service name (default: dokitier-core)
  -i <seconds>         report interval seconds (default: 1)
  --insecure           skip TLS certificate verification when downloading
USAGE
}

while [[ $# -gt 0 ]]; do
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
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1"; usage; exit 1 ;;
  esac
done

if [[ -z "$ENDPOINT" || -z "$TOKEN" || -z "$NODE_ID" ]]; then
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

if [[ "$VERSION" == "latest" ]]; then
  url="https://github.com/${REPO}/releases/latest/download/${asset}"
else
  url="https://github.com/${REPO}/releases/download/${VERSION}/${asset}"
fi
if [[ -n "$GH_PROXY" ]]; then
  url="${GH_PROXY%/}/${url}"
fi

download() {
  local src="$1" dst="$2"
  if command -v curl >/dev/null 2>&1; then
    local args=(-fsSL)
    [[ "$INSECURE" == "1" ]] && args+=(-k)
    curl "${args[@]}" "$src" -o "$dst"
  elif command -v wget >/dev/null 2>&1; then
    local args=(-q -O "$dst")
    [[ "$INSECURE" == "1" ]] && args+=(--no-check-certificate)
    wget "${args[@]}" "$src"
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
