#!/bin/bash
# Mock redis-cli for health checks
if [ "$1" = "ping" ]; then
    echo "PONG"
    exit 0
fi
echo "OK"
exit 0