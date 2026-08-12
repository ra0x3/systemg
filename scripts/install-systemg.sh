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

VERSION=$("$SOURCE" --version 2>/dev/null | awk 'NR == 1 { print $2; exit }' | sed 's/^v//')
if [ -z "$VERSION" ]; then
  echo "Could not determine the version of: $SOURCE" >&2
  exit 1
fi

VERSIONS_DIR="/usr/lib/systemg/versions"
VERSION_DIR="$VERSIONS_DIR/$VERSION"
TARGET="$VERSION_DIR/sysg"
STAGED="$VERSION_DIR/.sysg.$$"

install -d -m755 "$VERSIONS_DIR"
install -d -m755 "$VERSION_DIR"
install -m755 "$SOURCE" "$STAGED"
if [ "$("$STAGED" --version 2>/dev/null | awk 'NR == 1 { print $2; exit }' | sed 's/^v//')" != "$VERSION" ]; then
  echo "Staged binary failed version verification." >&2
  exit 1
fi
mv -f "$STAGED" "$TARGET"

OS="$(uname -s)"

if [ "$OS" = "Darwin" ]; then
  # macOS: native system locations under /Library; launchd owns the daemon.
  STATE_DIR="/Library/Application Support/systemg"
  LOG_DIR="/Library/Logs/systemg"
  CONFIG_DIR="$STATE_DIR/etc"
  install -d -m755 "$STATE_DIR" "$LOG_DIR" "$CONFIG_DIR"

  LINK_TMP="/usr/local/bin/.sysg-link.$$"
  install -d -m755 /usr/local/bin
  rm -f "$LINK_TMP"
  ln -s "$TARGET" "$LINK_TMP"
  mv -f "$LINK_TMP" /usr/local/bin/sysg

  PLIST="/Library/LaunchDaemons/dev.sysg.supervisor.plist"
  cat > "$PLIST" <<PLISTEOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>dev.sysg.supervisor</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/sysg</string>
    <string>--sys</string>
    <string>start</string>
    <string>--config</string>
    <string>$CONFIG_DIR/systemg.yaml</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <false/>
  <key>StandardOutPath</key>
  <string>$LOG_DIR/launchd.out.log</string>
  <key>StandardErrorPath</key>
  <string>$LOG_DIR/launchd.err.log</string>
</dict>
</plist>
PLISTEOF
  chmod 644 "$PLIST"

  echo "Installation complete. Load the daemon with:"
  echo "  sudo launchctl load -w $PLIST"
  echo "Unload with:"
  echo "  sudo launchctl unload -w $PLIST"
  exit 0
fi

install -d -m755 /etc/systemg
install -d -m755 /var/lib/systemg
install -d -m755 /var/log/systemg
install -d -m755 /etc/systemg/logrotate

if ! "$TARGET" --sys upgrade-supervisor --binary "$TARGET"; then
  echo "sysg $VERSION was installed but not activated." >&2
  echo "The existing /usr/bin/sysg target was left unchanged." >&2
  exit 1
fi

LINK_TMP="/usr/bin/.sysg-link.$$"
rm -f "$LINK_TMP"
ln -s "$TARGET" "$LINK_TMP"
mv -f "$LINK_TMP" /usr/bin/sysg

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
ExecStop=/usr/bin/sysg --sys stop --config /etc/systemg/systemg.yaml
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
