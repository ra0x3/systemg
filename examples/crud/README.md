# FastAPI CRUD Example

A minimal FastAPI CRUD application demonstrating `systemg` service management and automatic recovery from failures.

## Overview

This example showcases:
- **FastAPI** with modern Python async/await
- **uvicorn** ASGI server managed by `systemg`
- **uv** package manager for fast dependency installation
- **Automatic recovery** from service failures
- **Simple in-memory storage** using Python dict

## Quick Start

### 1. Install dependencies with uv

```bash
cd examples/crud
uv sync
```

### 2. Start the service with systemg

```bash
sysg start
```

### 3. Test the API

The API will be available at `http://localhost:8888`

Check the interactive docs at `http://localhost:8888/docs`

### 4. Stop the service

```bash
sysg stop
```

## API Endpoints

### Todo Model
```python
{
    "title": "string",
    "description": "string",
    "id": "integer",
    "timestamp": "datetime",
    "is_completed": "boolean"
}
```

### Endpoints

- `GET /` - Health check
- `POST /todos` - Create a new todo
- `GET /todos` - List all todos
- `GET /todos/{id}` - Get a specific todo
- `PUT /todos/{id}` - Update a todo
- `DELETE /todos/{id}` - Delete a todo
- `GET /chaos` - Random failure endpoint (70% failure rate)

## Testing All Endpoints

Run the included test script to verify all endpoints:

```bash
uv run python test_api.py
```

This will:
1. Create a new todo
2. Read all todos
3. Update the todo
4. Get a specific todo
5. Test the chaos endpoint (demonstrates recovery)
6. Delete the todo

## Demonstrating Recovery

The `/chaos` endpoint has a 70% chance of returning a 500 error. This demonstrates how `systemg` handles failures:

```bash
# Watch the service recover from failures
sysg logs --service fastapi_server

# In another terminal, hit the chaos endpoint
curl http://localhost:8888/chaos
```

With `restart_policy: "on_failure"` and `backoff: "5s"`, systemg will automatically restart the service if it crashes, with a 5-second delay between attempts.

## Configuration

The `crud.sysg.yaml` file configures:

```yaml
version: "1"

services:
  fastapi_server:
    command: "uv run uvicorn main:app --host 0.0.0.0 --port 8888"
    deployment_strategy: "rolling_start"  # Zero-downtime deployments
    restart_policy: "on_failure"          # Auto-restart on crashes
    retries: "10"                          # Max restart attempts
    backoff: "5s"                          # Delay between restarts
```

## Project Structure

```
crud/
├── main.py            # FastAPI application
├── test_api.py        # Test script for all endpoints
├── pyproject.toml     # Python dependencies (uv)
├── crud.sysg.yaml     # systemg configuration
└── README.md          # This file
```

## Example Usage

### Create a Todo
```bash
curl -X POST http://localhost:8888/todos \
  -H "Content-Type: application/json" \
  -d '{
    "title": "Learn systemg",
    "description": "Understand service management with systemg",
    "is_completed": false
  }'
```

### List Todos
```bash
curl http://localhost:8888/todos
```

### Update a Todo
```bash
curl -X PUT http://localhost:8888/todos/1 \
  -H "Content-Type: application/json" \
  -d '{
    "title": "Learn systemg",
    "description": "Master service management with systemg",
    "is_completed": true
  }'
```

### Delete a Todo
```bash
curl -X DELETE http://localhost:8888/todos/1
```

### Test Chaos Endpoint
```bash
# This will fail 70% of the time
curl http://localhost:8888/chaos
```

## Why This Example?

This example demonstrates:
- ✅ Modern Python web development with FastAPI
- ✅ Simple service management with `sysg start` and `sysg stop`
- ✅ Automatic recovery from failures
- ✅ Zero-downtime deployments with rolling strategy
- ✅ Minimal configuration for maximum clarity