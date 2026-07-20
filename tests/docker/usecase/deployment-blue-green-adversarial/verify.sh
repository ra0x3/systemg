#!/usr/bin/env sh
set -e
[ ! -f /tmp/verify-fail ]
[ "$(cat /tmp/active-slot)" = "$1" ]
python3 -c 'import sys, urllib.request; urllib.request.urlopen(sys.argv[1], timeout=2).read()' "http://127.0.0.1:$1/health"
