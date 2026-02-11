---
sidebar_position: 1
title: start
---

# start

Launch services defined in your configuration.

```sh
sysg start
```

## Options

### `--config`

Path to the configuration file. If not specified, systemg looks for `systemg.yaml` or `sysg.yaml` in the current directory.

```sh
sysg start --config /etc/myapp/services.yaml
```

### `--daemonize`

Run the supervisor as a background daemon. The supervisor continues running after you close your terminal, and subsequent commands communicate with it via Unix socket.

```sh
sysg start --daemonize
```

### `--log-level`

Set logging verbosity for this invocation. Accepts named levels (`trace`, `debug`, `info`, `warn`, `error`, `off`) or numeric values (5-0).

```sh
sysg start --log-level debug
```

## Examples

### Start with default configuration

```sh
sysg start
```

Looks for `systemg.yaml` or `sysg.yaml` in the current directory.

### Start with specific configuration

```sh
sysg start --config /etc/myapp/services.yaml
```

### Daemon mode

Run the supervisor in the background. Subsequent commands communicate with this long-lived process.

```sh
sysg start --daemonize
```

Check if the daemon is running:

```sh
sysg status
```

### Debug mode

See detailed output during startup:

```sh
sysg start --log-level debug
```

## What happens

1. Services start in dependency order
2. Each service gets its own process group
3. Logs are written to `~/.local/share/systemg/logs/`
4. PIDs are tracked for other commands

In daemon mode, the supervisor monitors services and handles restarts according to your configuration.

## See also

- [`stop`](stop) - Stop running services
- [`status`](status) - Check service health
- [`restart`](restart) - Restart services