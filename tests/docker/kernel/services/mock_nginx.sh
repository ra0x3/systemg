#!/bin/bash
# Mock Nginx for testing systemg kernel mode
# Simulates nginx behavior without requiring actual Nginx installation

# Handle nginx -t test command
if [ "$1" = "-t" ]; then
    echo "nginx: configuration file /etc/nginx/nginx.conf test is successful"
    exit 0
fi

echo "Mock nginx: master process nginx -g 'daemon off;'"
echo "Mock nginx: worker process started"

# Handle signals properly
trap 'echo "Mock nginx: Shutting down gracefully"; exit 0' SIGTERM SIGINT

# Keep running until terminated
while true; do
    sleep 10
    echo "Mock nginx: Handling requests on port 80"
done