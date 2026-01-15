#!/bin/bash
# Mock Redis for testing systemg kernel mode
# Simulates redis-server behavior without requiring actual Redis installation

echo "Mock Redis Server v6.2.0 starting..."
echo "Mock Redis: Ready to accept connections on port ${REDIS_PORT:-6379}"

# Handle signals properly
trap 'echo "Mock Redis: Received shutdown signal, saving data..."; exit 0' SIGTERM SIGINT

# Keep running until terminated
while true; do
    sleep 10
    echo "Mock Redis: Processing commands..."
done