---
sidebar_position: 4
title: Configuration
---

Below is an example of a complete `systemg` configuration file.


```yaml
# Configuration file version
version: "1"

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

    deployment:
      # Keep the existing instance running until a replacement passes its checks
      strategy: "rolling"
      # Optional build or migration step executed before the new process launches
      pre_start: "python manage.py migrate"
      # Optional health probe the new instance must satisfy
      health_check:
        url: "http://localhost:8000/health"
        timeout: "45s"
        retries: 4
      # Optional grace window before the old instance is terminated
      grace_period: "5s"

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

## Deployment strategies

Systemg supports two deployment strategies per service:

- `immediate` *(default)* – stop the running instance and start a fresh copy right away. This matches the behaviour in earlier releases and requires no additional configuration.
- `rolling` – launch a replacement alongside the existing instance, verify it is healthy, optionally wait for a grace period, and only then terminate the previous process. This keeps services available throughout a restart.

Enable rolling restarts by adding a `deployment` block to a service definition:

```yaml
services:
  api:
    command: "./target/release/api"
    restart_policy: "always"
    deployment:
      strategy: "rolling"
      pre_start: "cargo build --release"
      health_check:
        url: "http://localhost:8080/health"
        timeout: "60s"
        retries: 5
      grace_period: "5s"
```

### Rolling restart settings

- **`strategy`** – set to `rolling` to opt in; omit or set to `immediate` to keep the classic stop/start cycle.
- **`pre_start`** *(optional)* – shell command executed before the new process launches. Useful for builds, migrations, or asset preparation. Non-zero exit codes abort the deployment and preserve the old instance.
- **`health_check`** *(optional)* – HTTP probe the new instance must pass. Systemg retries based on `retries` (default 3) until the total elapsed time exceeds `timeout` (default 30s).
- **`grace_period`** *(optional)* – additional delay to keep the old instance alive after the new one passes health checks. Handy for draining load balancer connections.

If any step of the rolling restart fails, the new process is halted and the previous instance is restored automatically. This ensures unhealthy builds never displace a working service.
