---
sidebar_position: 1
title: Commands
---

# Commands

## Quick reference

```sh
sysg start                     # Launch services
sysg stop                      # Stop services
sysg restart                   # Restart services
sysg status                    # Check health
sysg logs                      # View output
sysg inspect api               # View metrics
sysg spawn --name w1 -- cmd    # Create child
sysg purge                     # Clear all state
```

All commands accept `--log-level` (`debug`, `info`, `warn`, `error`).

## Auto-discovery

After first start, systemg remembers your config:

```sh
sysg start --config app.yaml
sysg status                 # Uses app.yaml automatically
```

## Daemon mode

Run supervisor in background:

```sh
sysg start --daemonize
sysg status                 # Communicates with daemon
sysg stop                   # Stops services and daemon
```

## Service-specific operations

Most commands accept a service name:

```sh
sysg restart --service api
sysg logs worker
sysg stop --service redis
```

## See also

- [Configuration](../configuration) - Define services
- [Quickstart](../../quickstart) - First steps