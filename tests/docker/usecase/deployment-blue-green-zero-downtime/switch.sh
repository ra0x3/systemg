#!/usr/bin/env sh
set -e
n=$(($(cat /tmp/flip-count 2>/dev/null || echo 0) + 1))
echo "$n" > /tmp/flip-count
python3 /usecase/pin.py "$n" </dev/null >/dev/null 2>&1 &
i=0
until [ -f "/tmp/accepted-$n" ]; do
  i=$((i + 1))
  [ "$i" -le 50 ]
  sleep 0.1
done
echo "server 127.0.0.1:$1;" > /tmp/upstream.conf.next
mv /tmp/upstream.conf.next /tmp/upstream.conf
nginx -t -q -c /usecase/nginx.conf
nginx -s reload -c /usecase/nginx.conf
echo "$1" > /tmp/active-slot
