---
sidebar_position: 3
title: Commands
---

# Commands

All commands accept `--log-level <LEVEL>` (debug/info/warn/error or 0-5).

## Auto-Discovery

Config path saved after first `start`. No need to repeat `--config`:

```sh
sysg start --config app.yaml
sysg status  # Uses app.yaml
```

## Start

```sh
sysg start                        # Default config
sysg start --config app.yaml      # Specific config
sysg start --daemonize            # Background
```

## Stop

```sh
sysg stop                         # All services
sysg stop --service myapp         # Specific service
```

## Restart

```sh
sysg restart
sysg restart --config new.yaml
```

## Status

```sh
sysg status
sysg status --service web
sysg status --all                 # Include orphaned
```

## Inspect

```sh
sysg inspect myservice
sysg inspect myservice --window 2m
sysg inspect myservice --json
```

## Logs

```sh
sysg logs                         # Last 50 lines
sysg logs api --lines 100
sysg logs api --kind stderr
```

## Spawn

```sh
sysg spawn --name worker -- python worker.py
sysg spawn --name temp --ttl 3600 -- ./task.sh
```

## Purge

```sh
sysg purge   # Deletes all state/logs (irreversible)
```
