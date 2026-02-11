---
sidebar_position: 0
title: Introduction
---

# systemg

Run multi-service applications with one command.

## What systemg does

Suppose you're building an API that needs PostgreSQL, Redis, and a background worker. Instead of starting each service manually in different terminals, you define them once in a YAML file. systemg starts everything in the right order, restarts crashed services, and provides unified logging.

```yaml
version: "1"
services:
  postgres:
    command: "postgres -D /var/lib/postgresql/data"
  redis:
    command: "redis-server"
  api:
    command: "python app.py"
    depends_on: ["postgres", "redis"]
  worker:
    command: "celery worker -A tasks"
    depends_on: ["redis"]
```

Run `sysg start` and your entire stack is running. Run `sysg stop` and everything shuts down cleanly.

## Built for production

systemg handles the complexity of process management so you don't have to. Services that depend on databases wait for them to start. Crashed processes restart automatically with exponential backoff. Each service gets isolated logging. Zero external dependencies—just a single binary.

## Next steps

[Install systemg](installation) and have your first service running in under a minute.