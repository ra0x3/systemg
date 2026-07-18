#!/usr/bin/env bash
# USE CASE: an HTTP health check ignores HTTP_PROXY and probes the service DIRECTLY.
#
# WHAT THIS TESTS (real dogfooding bug)
#   The user's env exported a proxy. reqwest reads HTTP_PROXY/ALL_PROXY by default,
#   so sysg routed the localhost health probe THROUGH the proxy — which cannot reach
#   127.0.0.1 — and every attempt hung the full attempt_timeout, stranding `start`
#   for minutes while `curl` (which bypasses the proxy for localhost) got 200 at once.
#   The health-check client must use no_proxy() so a proxy in the environment never
#   blackholes a direct liveness probe.
#
# EXPECTED OUTCOME
#   - With a BOGUS HTTP_PROXY/ALL_PROXY exported (a dead address), `sysg start`
#     still passes web's health check QUICKLY and exits 0 — it does not hang.
set -u
. /usecase/lib.sh

CONFIG=/usecase/stack.yaml

# A proxy pointing at a black hole: if the probe honored it, every attempt would
# hang for the full attempt_timeout (30s x 5). With no_proxy() it is ignored.
export HTTP_PROXY="http://127.0.0.1:9"
export HTTPS_PROXY="http://127.0.0.1:9"
export ALL_PROXY="http://127.0.0.1:9"
export http_proxy="http://127.0.0.1:9"
export all_proxy="http://127.0.0.1:9"

section "start with a bogus proxy in the env — health check must not be proxied"
START_T="$(date +%s)"
sysg start --config "$CONFIG" --daemonize
RC=$?
END_T="$(date +%s)"
ELAPSED=$((END_T - START_T))
echo "start rc=$RC elapsed=${ELAPSED}s"
[ "$RC" = "0" ]
check "$?" "start exits 0 (health check passed, not blackholed by the proxy)"
# A proxied probe would burn one full attempt_timeout (30s) or more before failing.
# A direct probe passes in a couple seconds. Assert it did NOT hang.
[ "$ELAPSED" -lt 20 ]
check "$?" "start completed quickly (<20s) — the probe went direct, not via proxy"

section "web is healthy per status"
S="$(sysg status --config "$CONFIG" --format json 2>/dev/null)"
[ "$(unit_field "$S" web state demo)" = "running" ]
check "$?" "web is running and healthy"

sysg stop --supervisor >/dev/null 2>&1
finish
