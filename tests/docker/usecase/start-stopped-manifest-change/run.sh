#!/usr/bin/env bash
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml
VERSION=/usecase/api-version

section "boot the original manifest"
sysg start --config "$CONFIG" --daemonize
check "$?" "initial start exits 0"
sleep 3
S1="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
API1="$(unit_field "$S1" api pid demo)"
WORKER1="$(unit_field "$S1" worker pid demo)"
pid_alive "$API1"
check "$?" "api is running on V1"
pid_alive "$WORKER1"
check "$?" "worker is running"

section "stop the project and change one service"
sysg stop --config "$CONFIG"
check "$?" "project stop exits 0"
sleep 1
! pid_alive "$API1"
check "$?" "api stopped"
! pid_alive "$WORKER1"
check "$?" "worker stopped"
cat > "$CONFIG" <<'EOF'
version: "2"
projects:
  demo:
    name: Demo
    services:
      api:
        command: "sh -c 'echo V2 > /usecase/api-version && exec sleep 3000'"
        restart_policy: "always"
      worker:
        command: "sleep 3000"
        restart_policy: "always"
EOF

section "start the changed manifest"
timeout 20 sysg start --config "$CONFIG" --daemonize
check "$?" "start after manifest change exits 0"
sleep 3
S2="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
API2="$(unit_field "$S2" api pid demo)"
WORKER2="$(unit_field "$S2" worker pid demo)"
[ "$API2" != "$API1" ] && pid_alive "$API2"
check "$?" "changed api starts on V2"
[ "$(cat "$VERSION" 2>/dev/null)" = "V2" ]
check "$?" "changed api command ran"
[ "$WORKER2" != "$WORKER1" ] && pid_alive "$WORKER2"
check "$?" "unchanged worker starts again"

sysg stop --supervisor >/dev/null 2>&1
finish
