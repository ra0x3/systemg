#!/usr/bin/env bash
set -euo pipefail

# Simple helper to install the sysg binary system-wide.
# Usage: ./scripts/install-systemg.sh /path/to/sysg

if [ "${UID}" -ne 0 ]; then
  echo "This installer must be run as root (sudo)." >&2
  exit 1
fi

if [ "${#}" -ne 1 ]; then
  echo "Usage: $0 /path/to/sysg" >&2
  exit 1
fi

SOURCE="$1"
if [ ! -f "$SOURCE" ]; then
  echo "Binary not found: $SOURCE" >&2
  exit 1
fi

install -Dm755 "$SOURCE" /usr/bin/sysg

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

echo "Installation complete. Enable the service with:"
echo "  systemctl enable sysg.service"
echo "  systemctl start sysg.service"
