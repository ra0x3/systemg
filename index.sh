#!/usr/bin/env bash
set -euo pipefail

# Bootstrap installer for systemg.
# Usage: curl -fsSL https://sh.sysg.dev/ | sh

if [ "${UID}" -ne 0 ]; then
  echo "This installer must be run as root (sudo)." >&2
  exit 1
fi

if [ "${#}" -gt 1 ]; then
  echo "Usage: sh index.sh [path-to-sysg-binary]" >&2
  exit 1
fi

BINARY="${1:-sysg}"

if ! command -v "$BINARY" >/dev/null 2>&1; then
  echo "systemg binary not found: $BINARY" >&2
  exit 1
fi

install -Dm755 "$BINARY" /usr/bin/sysg

install -d -m755 /etc/systemg
install -d -m755 /var/lib/systemg
install -d -m755 /var/log/systemg
install -d -m755 /etc/systemg/logrotate

cat <<'LOGROTATE' > /etc/logrotate.d/systemg
/var/log/systemg/supervisor.log {
    weekly
    rotate 8
    compress
    missingok
    notifempty
    copytruncate
}
LOGROTATE

cat <<'SERVICE' > /etc/systemd/system/sysg.service
[Unit]
Description=Systemg Supervisor
After=network.target

[Service]
ExecStart=/usr/bin/sysg --sys start --config /etc/systemg/systemg.yaml --daemonize
ExecStop=/usr/bin/sysg stop --config /etc/systemg/systemg.yaml
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
SERVICE

mkdir -p /etc/systemd/system/sysg.service.d

cat <<'OVERRIDE' > /etc/systemd/system/sysg.service.d/socket-activation.conf
[Service]
Environment=LISTEN_FDS=0
OVERRIDE

cat <<'CONFIG' > /etc/systemg/systemg.yaml
version: "1"

services:
  web:
    command: "/usr/local/bin/web"
    user: "www-data"
    group: "www-data"
    restart_policy: "always"
    limits:
      nofile: 65536
      memlock: "512M"
    capabilities:
      - CAP_NET_BIND_SERVICE
    isolation:
      network: true
      pid: true

cron:
  nightly_backup:
    command: "/usr/local/bin/backup"
    cron:
      expression: "0 0 * * * *"
CONFIG

echo "Installation complete. Enable the service with:"
echo "  systemctl enable sysg.service"
echo "  systemctl start sysg.service"
