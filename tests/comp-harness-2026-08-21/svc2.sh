#!/bin/sh
echo r > /ready/w
while :; do echo "tick $(date +%s)"; sleep 1; done
