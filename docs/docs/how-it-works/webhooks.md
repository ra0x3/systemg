---
sidebar_position: 2
title: Webhooks
---

# Webhooks

Run commands on service lifecycle events.

## Events

| Stage | When | Outcomes |
|-------|------|----------|
| `on_start` | Service spawn | `success`, `error` |
| `on_stop` | Service exit | `success`, `error` |
| `on_restart` | Auto-restart after crash | `success`, `error` |

- Manual stops = `success`
- Crashes = `error`
- Only auto-restarts trigger `on_restart`

## Configuration

```yaml
services:
  postgres:
    command: "postgres -D /var/lib/postgres"
    hooks:
      on_start:
        success:
          command: "curl --request POST https://api.example.com/start"
          timeout: "10s"
        error:
          command: "curl --request POST https://api.example.com/failed"
      on_stop:
        error:
          command: "curl --request POST https://api.example.com/crashed"
```

Hooks inherit service environment variables.

## Execution

- Run via `sh -c`
- Fire-and-forget (no retries)
- Timeout kills with SIGKILL
- Failures logged but don't affect service

## Behavior

| Scenario | Hooks |
|----------|-------|
| Start success | `on_start.success` |
| Start failure | `on_start.error` |
| Manual stop | `on_stop.success` |
| Crash | `on_stop.error` → restart |
| Restart after crash | `on_stop.error`, `on_start.success`, `on_restart.success` |

## Tips

- Keep commands short
- Use env vars for secrets
- Make endpoints idempotent
