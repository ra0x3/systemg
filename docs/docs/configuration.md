---
sidebar_position: 4
title: Configuration
---

Below is an example of a complete `systemg` configuration file.


```yaml
# Configuration file version
version: 1

services:
  # Name of the service
  postgres:
    # The command to start the service
    command: "postgres -D /var/lib/postgres"

    env:
      # Path to a file containing environment variables for this service
      file: "/etc/myapp/database.env"

    # Policy for restarting the service: "always", "on-failure", or "never"
    restart_policy: "always"

    # Time to wait before attempting a restart (backoff duration)
    backoff: "5s"

    hooks:
      # Command to run when the service starts successfully
      on_start: "echo 'Postgres started'"
      # Command to run if the service crashes or fails to start
      on_error: "echo 'Postgres crashed'"

  django:
    command: "python manage.py runserver"

    env:
      vars:
        # Inline environment variables for the service
        DEBUG: "true"
        DATABASE_URL: "postgres://user:password@localhost:5432/dbname"

    restart_policy: "on-failure"
    backoff: "5s"

    # List of services this one depends on (must be started before this)
    depends_on:
      - "postgres"

    hooks:
      on_start: "curl -X POST http://example.com/hook/django-start"
      on_error: "curl -X POST http://example.com/hook/django-error"

  ngrok:
    command: "ngrok http 8000"
    restart_policy: "on-failure"
    backoff: "3s"

    hooks:
      on_start: "echo 'ngrok started'"
      on_error: "echo 'ngrok crashed'"
```

## Service dependencies

Use the `depends_on` field to express service prerequisites. Systemg evaluates the dependency graph before launching processes and enforces the following rules:

- **Topological startup** – services are started in an order that guarantees every dependency is already running (or has exited successfully for one-shot jobs) before its dependents launch.
- **Fail fast on unhealthy prerequisites** – if a dependency fails to start, dependents are skipped and the failure is surfaced instead of allowing a partial boot.
- **Cascading shutdowns** – when a running service crashes, all services that depend on it are stopped automatically to keep the environment consistent.

```yaml title="Example"
services:
  redis:
    command: "redis-server"

  worker:
    command: "node worker.js"
    depends_on:
      - redis
```

If `redis` exits with a non-zero status, `worker` will not start (or will be stopped if it is already running) until `redis` is healthy again.
