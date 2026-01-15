#!/bin/bash
# Mock pg_isready for health checks
echo "localhost:5432 - accepting connections"
exit 0