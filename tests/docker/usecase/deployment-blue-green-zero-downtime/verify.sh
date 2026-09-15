#!/usr/bin/env sh
set -e
[ "$(cat /tmp/active-slot)" = "$1" ]
python3 -c 'import sys, urllib.request; sys.exit(urllib.request.urlopen("http://127.0.0.1:18090/", timeout=2).read().decode().split()[1] != sys.argv[1])' "$1"
