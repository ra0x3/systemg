---
name: systemg
description: Use when working with systemg/sysg, the YAML-driven process manager — writing or editing sysg config files (services, depends_on, deployment, hooks, cron), starting/stopping/restarting sysg-managed stacks, or inspecting service status and logs.
---

# systemg (sysg)

systemg is a single-binary process composer: `sysg` starts, supervises, restarts,
inspects, and logs local service graphs defined in YAML. Full docs at
https://sysg.dev — fetch https://sysg.dev/llms-full.txt for the complete
LLM-oriented reference when anything here is insufficient.

## CLI

Always pass `--plain` in non-interactive contexts (scripts, agents, pipes):

```sh
sysg validate -c sysg.yaml       # check a config; exits non-zero on errors
sysg validate -c sysg.yaml --format json   # structured diagnostics for CI
sysg start -c sysg.yaml          # start the manager with a config
sysg restart                     # restart (optionally -c new-config.yaml)
sysg stop                        # stop the manager
sysg --plain status              # all units, non-interactive
sysg status --format json        # structured status for parsing
sysg inspect -s <unit> --format json
sysg logs -s <unit> --format json          # JSON-lines: {ts, stream, service, line}
sysg logs -s <unit> --raw                  # app lines without sysg prefixes
sysg logs -s <unit> --grep ERROR --since 2h
sysg logs --path                 # locate log files for external tooling
sysg purge                       # wipe all systemg state/runtime files
```

`sysg logs` never follows in pipes/agent sessions; it prints a snapshot and
exits. Use `--follow` only for an intentional long-running tail.

## Config skeleton

```yaml
version: "1"
project:
  id: myapp
  name: My App
env:
  file: ".env"
  vars:
    APP_ENV: "production"
logs:
  sink: file
  max_bytes: 10485760
  max_files: 5
services:
  postgres:
    command: "postgres -D /var/lib/postgresql/data"
    restart_policy: "always"
  api:
    command: "gunicorn app:application --bind 0.0.0.0:8000"
    depends_on: [postgres]
    restart_policy: "always"
    backoff: "10s"
    env:
      file: "/etc/myapp/production.env"
      vars:
        PORT: "8000"
    deployment:
      strategy: "rolling"
      pre_start: "python manage.py migrate"
      health_check:
        url: "http://localhost:8000/health"
        timeout: "45s"
        retries: 5
      grace_period: "5s"
    hooks:
      on_stop:
        error:
          command: "curl -X POST https://alerts.example.com/api/crash"
  backup:
    command: "pg_dump mydb > /backups/db.sql"
    cron:
      expression: "0 0 2 * * *"
```

## Service fields

- `command` (required) — shell command to run
- `depends_on` — services that must start first
- `restart_policy` — `always` | `on-failure` | `never`
- `backoff` — delay between restarts; `max_restarts` — restart cap
- `env` — `vars` (map), `file` (path), `inherit_env`, `strip`
- `deployment` — `strategy` (`rolling`|`immediate`), `pre_start` (command run
  before each (re)start — builds/migrations go here), `health_check`
  (`url` or `command`, `interval`, `timeout`, `retries`), `grace_period`,
  `blue_green` (`slots`, `switch_command`, `env_var`)
- `hooks` — `on_start`/`on_stop`/`on_restart`, each with `success`/`error`
  holding `{command, timeout}`; fire after lifecycle events (non-blocking),
  unlike `deployment.pre_start` which blocks the start
- `cron` — `expression` (6-field, seconds first), optional `timezone`; makes
  the unit scheduled instead of supervised
- `logs` — per-service `sink`, `max_bytes`, `max_files`
- `skip` — bool, or a command whose success skips the service
- Privileged mode only: `user`, `group`, `capabilities`, `limits`, `isolation`

## Conventions

- Run `sysg validate -c <file>` after writing or editing a manifest; it reports
  the exact line, why it's wrong, and a fix. Prefer `--format json` when parsing.
- Health checks live under `deployment.health_check`, never top-level.
- `depends_on` gates start order; a dependent starts only after its
  dependencies (including their `pre_start`) have started.
- Config docs: https://sysg.dev/how-it-works/configuration
- Commands: https://sysg.dev/how-it-works/commands
