---
sidebar_position: 6
title: Webhooks
---

# Webhooks

Systemg can run arbitrary commands when services transition through key lifecycle
moments. These webhook hooks make it easy to notify external systems, trigger
incident automations, or run cleanup tasks without embedding that logic directly
in your services.

## Lifecycle Overview

Each webhook belongs to a **lifecycle stage** and fires with a distinct
**outcome**:

| Stage | Triggered When | Outcomes |
|-------|----------------|----------|
| `on_start` | Systemg attempts to spawn the service process | `success`, `error` |
| `on_stop` | A service exits (cleanly or unexpectedly) | `success`, `error` |
| `on_restart` | Systemg automatically restarts a crashed service | `success`, `error` |

- `success` means the lifecycle completed as expected (e.g. the process started
  or was stopped intentionally).
- `error` indicates the lifecycle failed (e.g. spawn failed, the process
  crashed, or the restart attempt errored).

`on_start.success` fires once Systemg observes the service remain running across
consecutive readiness probes (or when a one-shot service exits cleanly during
startup). If the spawn fails outright or the process terminates before reaching
that ready state, `on_start.error` is emitted instead.

Manual `sysg stop --service foo` invocations count as an `on_stop.success`
because the supervisor observed a clean, operator-initiated shutdown. Crashes or
non-zero exit codes generate `on_stop.error` before the restart workflow runs.

`on_restart` hooks only fire when Systemg restarts a service because of a crash
and the service’s restart policy allows it. They are not invoked for manual
restarts (`sysg restart`) or configuration reloads.

## Configuration

Add a `hooks` block to a service to opt into lifecycle webhooks. Each lifecycle
stage can define `success` and/or `error` commands with an optional timeout:

```yaml
version: "1"
services:
  postgres:
    command: "postgres -D /var/lib/postgres"
    env:
      file: "/etc/myapp/database.env"
      vars:
        MY_TOKEN: "override-token"
    hooks:
      on_start:
        success:
          command: "curl -X POST https://api.example.com/hooks/start --data '{\"service\":\"postgres\",\"status\":\"ok\"}' -H 'Authorization: Bearer $MY_TOKEN'"
          timeout: "10s"
        error:
          command: "curl -X POST https://api.example.com/hooks/start --data '{\"service\":\"postgres\",\"status\":\"error\"}'"
      on_stop:
        success:
          command: "curl -X POST https://api.example.com/hooks/stop --data '{\"service\":\"postgres\",\"status\":\"ok\"}'"
        error:
          command: "curl -X POST https://api.example.com/hooks/stop --data '{\"service\":\"postgres\",\"status\":\"crashed\"}'"
      on_restart:
        success:
          command: "logger 'postgres recovered after crash'"
          timeout: "5s"
```

### Environment Precedence

Webhook commands inherit the service environment that Systemg prepares before
launching the main process:

1. Variables loaded from the optional `env.file` form the base environment.
2. Inline key/value pairs in `env.vars` then override any duplicates.
3. The supervisor’s own process environment provides final fallbacks.

This order means inline overrides in `env.vars` win over values defined in the
`.env` file, matching the behavior of the services themselves. Values are also
available for `${VAR}` expansions during configuration parsing, so hooks can
safely reference the same secrets and settings as the main service command.

### Timeouts and Execution Model

- **Execution shell**: Hooks run via `sh -c '…'`. Commands are launched as
  discrete child processes so they never block the supervisor loop.
- **Timeouts**: If a timeout is specified and the command is still running when
  it expires, Systemg sends SIGKILL to the hook process and logs a warning.
- **Logging**: stdout/stderr from hooks follow the parent process’ logging
  configuration. Failures are reported through the supervisor logs.
- **Isolation**: Hooks are fire-and-forget; they do not retry automatically and
  their exit status does not affect service state apart from logging.

## Behavior Matrix

| Scenario | Hooks Fired (in order) |
|----------|-----------------------|
| Service starts successfully | `on_start.success` |
| Service fails to spawn | `on_start.error` |
| Service stops via `sysg stop` | `on_stop.success` |
| Service exits with non-zero status | `on_stop.error`, then restart policy applies |
| Service restarts after crash and succeeds | `on_stop.error`, `on_start.success`, `on_restart.success` |
| Service restart attempt fails | `on_stop.error`, `on_restart.error` |

## Best Practices

- Keep webhook commands short-lived. Long-running scripts should be moved into
  dedicated services.
- Use environment variables for sensitive tokens instead of embedding them in
  shell commands.
- Prefer idempotent endpoints; retries are not automatic unless you add them in
  the script or remote system.
- Log hook output server-side (e.g. with `logger` or a structured logging tool)
  if you need post-mortem traces.
- Combine with [Cron Scheduling](./cron.md) for heartbeat pings or periodic
  validation that the hooks themselves are healthy.

## Troubleshooting

- **Hooks not firing**: Confirm the relevant lifecycle event is occurring (e.g.
  the service is actually crashing) and check supervisor logs for hook errors.
- **Wrong environment values**: Remember `env.vars` overrides `.env` file values
  when expanding configuration. If you see stale data, make sure the inline
  values are up to date.
- **Timeouts**: Increase the `timeout` or break the work into a message queued
  task if commands exceed the allotted time.
- **Permission errors**: Hooks run with the same user as the supervisor process.
  Ensure scripts and destinations are accessible under that account.

For a deeper look at the configuration options, see the
[configuration reference](./configuration.md#hooks-optional).
