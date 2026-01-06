---
sidebar_position: 3
title: Commands
---

# Commands

Systemg uses a simple YAML-based configuration and supports the following commands.
Beyond lifecycle management, you can attach [webhooks](../webhooks.md) to service
events or run [cron-scheduled](../cron.md) jobs alongside long-lived processes.

Every command accepts `--log-level <LEVEL>` (string names like `info`/`debug` or numbers 0-5) if you want to change tracing output for a single invocation. For example, `sysg status --log-level debug`.

## Configuration Auto-Discovery

One of systemg's ergonomic features is the [`config_hint`](../state.md#config_hint) file. When you start the supervisor with a specific configuration file, systemg remembers this path. This means subsequent commands (`stop`, `restart`, `status`, `logs`) can be run without specifying `--config`:

```sh
# Start with a specific config
$ sysg start --config /path/to/my-app.yaml --daemonize

# These commands now work without --config
$ sysg status
$ sysg logs web-server
$ sysg restart
$ sysg stop
```

This greatly improves the command-line experience, especially when working with configuration files outside the current directory.

## Start

Start with the default configuration file:

```sh
sysg start
``` 

Start with a specific configuration file:

```sh
sysg start --config systemg.yaml
```

Start the long-lived supervisor with a specific configuration file:

```sh
sysg start --config systemg.yaml --daemonize
```

Adjust the logging verbosity for the current invocation (string or numeric levels 0-5):

```sh
sysg start --log-level info
sysg start --log-level 4
```

Once the supervisor is running it stays alive after you log out and subsequent
commands (status, restart, logs, stop) communicate with the same background
process rather than re-spawning services.

## Stop

Stop the supervisor and all managed services:

```sh
sysg stop
```

Stop a specific service:

```sh
sysg stop --service myapp
```

## Restart

Restart all services managed by the supervisor:

```sh
sysg restart
```

Restart with a different configuration file:

```sh
sysg restart --config new-config.yaml
```

## Status

Show the status of all services (uses default `systemg.yaml` config):

```sh
sysg status
```

Show the status with a specific configuration file:

```sh
sysg status --config myapp.yaml
```

Show the status of a specific service:

```sh
sysg status --service webserver
```

Show all services including orphaned state (services removed from config):

```sh
sysg status --all
```

## Inspect

Inspect a specific service or cron unit in detail:

```sh
sysg inspect myservice
```

View metrics with live updates:

```sh
sysg inspect myservice --tail
```

View metrics in JSON format for programmatic access:

```sh
sysg inspect myservice --json
```

## Logs

View the last 50 lines of stdout logs (default):

```sh
sysg logs
```

View logs for a specific service:

```sh
sysg logs api-service
```

View a custom number of log lines for a service:

```sh
sysg logs database --lines 100
```

View stderr logs instead of stdout:

```sh
sysg logs api-service --kind stderr
```

## Purge

Remove all systemg state and runtime files for a fresh start:

```sh
sysg purge
```

This command permanently deletes:
- Service status history (`state.json`)
- Cron execution history (`cron_state.json`)
- All logs (supervisor and service logs)
- Runtime files (PIDs, sockets, locks)

**⚠️ Warning**: This action cannot be undone. Your configuration files are safe, but all historical data and logs will be deleted.

Use this after ungraceful shutdowns, state corruption, or when you need a clean slate:

```sh
sysg stop
sysg purge
sysg start --config myapp.yaml --daemonize
```
