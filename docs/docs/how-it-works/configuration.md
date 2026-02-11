---
sidebar_position: 0
title: Configuration
---

# Configuration

systemg uses YAML files to define services and their relationships.

## Complete example

```yaml
version: "1"
env:
  vars:
    APP_ENV: "production"
services:
  postgres:
    command: "postgres -D /var/lib/postgresql/data"
    restart_policy: "always"
  redis:
    command: "redis-server /etc/redis/redis.conf"
    restart_policy: "always"
  api:
    command: >
      gunicorn app:application
      --bind 0.0.0.0:8000
    env:
      file: "/etc/myapp/production.env"
      vars:
        PORT: "8000"
        DATABASE_URL: "postgres://localhost/myapp"
    depends_on:
      - postgres
      - redis
    restart_policy: "always"
    backoff: "10s"
    deployment:
      strategy: "rolling"
      pre_start: "python manage.py migrate"
      health_check:
        url: "http://localhost:8000/health"
    hooks:
      on_start:
        success:
          command: "echo 'API started'"
      on_stop:
        error:
          command: >
            curl -X POST
            https://alerts.example.com/api/crash
  worker:
    command: >
      celery -A tasks worker
      --loglevel=info
    depends_on:
      - redis
    restart_policy: "on-failure"
    max_restarts: 5
  backup:
    command: >
      pg_dump mydb >
      /backups/db-$(date +%Y%m%d).sql
    cron: "0 2 * * *"
```

## Configuration sections

### `version`

**Required**. Specifies the configuration schema version.

```yaml
version: "1"
```

### `env`

Optional environment variables shared by all services.

```yaml
env:
  vars:
    LOG_LEVEL: "info"
    APP_ENV: "production"
  file: "/etc/myapp/common.env"
```

### `services`

**Required**. Defines the services to manage.

```yaml
services:
  web:
    command: "python app.py"
```

## Service configuration

### `command`

**Required**. The command to execute.

```yaml
services:
  web:
    command: "python app.py"
```

### `depends_on`

Services that must start before this one.

```yaml
services:
  api:
    command: "python app.py"
    depends_on:
      - postgres
      - redis
```

### `env`

Service-specific environment configuration.

```yaml
services:
  api:
    command: "python app.py"
    env:
      vars:
        PORT: "8000"
        DATABASE_URL: "postgres://localhost/myapp"
      file: "/etc/myapp/production.env"
```

### `restart_policy`

Control how services recover from crashes.

```yaml
services:
  api:
    command: "python app.py"
    restart_policy: "always"
    backoff: "5s"
    max_restarts: 10
```

**Policies:**
- `always` - Restart on any exit
- `on-failure` - Restart only on non-zero exit codes
- `never` - Don't restart

### `hooks`

Run commands when services start or stop.

```yaml
services:
  api:
    command: "python app.py"
    hooks:
      on_start:
        success:
          command: >
            curl -X POST
            https://status.example.com/api/up
        error:
          command: >
            curl -X POST
            https://status.example.com/api/down
      on_stop:
        error:
          command: >
            curl -X POST
            https://alerts.example.com/crash
```

### `health_check`

Verify services are ready before marking them healthy.

```yaml
services:
  api:
    command: "python app.py"
    health_check:
      command: "curl -f http://localhost:8000/health"
      interval: "10s"
      timeout: "5s"
      retries: 3
```

### `cron`

Run services on a schedule instead of continuously.

```yaml
services:
  backup:
    command: >
      pg_dump mydb >
      /backups/db-$(date +%Y%m%d).sql
    cron: "0 2 * * *"
```

### `deployment`

Control how services update during restarts.

```yaml
services:
  api:
    command: "python app.py"
    deployment:
      strategy: "rolling"
      pre_start: "python manage.py migrate"
      health_check:
        url: "http://localhost:8000/health"
        timeout: "30s"
      grace_period: "5s"
```

Rolling deployments start the new instance, wait for health checks, then stop the old instance. The `grace_period` allows in-flight requests to complete.

## Field reference

### Service fields

| Field | Type | Description |
|-------|------|-------------|
| `command` | string | Command to execute (required) |
| `depends_on` | array | Services that must start first |
| `env` | object | Environment configuration |
| `restart_policy` | string | `always`, `on-failure`, or `never` |
| `backoff` | string | Time between restart attempts |
| `max_restarts` | number | Maximum restart attempts |
| `hooks` | object | Lifecycle event handlers |
| `health_check` | object | Service readiness probe |
| `cron` | string | Cron schedule expression |
| `deployment` | object | Update strategy configuration |

### Environment object

| Field | Type | Description |
|-------|------|-------------|
| `vars` | object | Key-value environment variables |
| `file` | string | Path to env file |

### Hooks object

| Field | Type | Description |
|-------|------|-------------|
| `on_start` | object | Commands for start events |
| `on_stop` | object | Commands for stop events |
| `on_restart` | object | Commands for restart events |

Each hook has `success` and `error` handlers with:
- `command` - Command to execute
- `timeout` - Maximum execution time

### Health check object

| Field | Type | Description |
|-------|------|-------------|
| `command` | string | Check command |
| `url` | string | HTTP endpoint (alternative to command) |
| `interval` | string | Time between checks |
| `timeout` | string | Check timeout |
| `retries` | number | Attempts before marking unhealthy |

### Deployment object

| Field | Type | Description |
|-------|------|-------------|
| `strategy` | string | `rolling` or `immediate` |
| `pre_start` | string | Command to run before starting |
| `health_check` | object | Health check configuration |
| `grace_period` | string | Time before stopping old instance |