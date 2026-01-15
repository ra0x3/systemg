#!/bin/bash
# Mock PostgreSQL for testing systemg kernel mode
# Simulates postgres behavior without requiring actual PostgreSQL installation

echo "Mock PostgreSQL starting - Data directory: ${PGDATA:-/var/lib/postgresql/data}"
echo "Mock PostgreSQL server started on port 5432"

# Handle signals properly
trap 'echo "Mock PostgreSQL received shutdown signal"; exit 0' SIGTERM SIGINT

# Keep running until terminated
while true; do
    sleep 10
    echo "Mock PostgreSQL: Processing queries..."
done