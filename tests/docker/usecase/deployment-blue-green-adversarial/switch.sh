#!/usr/bin/env sh
echo "$2" > /tmp/active-slot
if [ -f /tmp/switch-fail-once ]; then
  rm /tmp/switch-fail-once
  exit 1
fi
