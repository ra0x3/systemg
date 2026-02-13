#!/bin/sh

echo "flushall" | redis-cli
rm -r orchestrator-ui/
