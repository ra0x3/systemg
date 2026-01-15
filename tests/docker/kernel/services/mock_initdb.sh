#!/bin/bash
# Mock initdb for PostgreSQL initialization
echo "Mock initdb: Creating database cluster in $1"
mkdir -p "$1"
echo "Mock initdb: Database cluster initialized"
exit 0