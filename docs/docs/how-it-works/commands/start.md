---
sidebar_position: 1
title: start
---

# start

Launch managed processes in one of three modes:
- manifest services (`sysg start`)
- ad-hoc units (`sysg start <command...>`)
- child units (`sysg start --child --parent-pid <pid> -- <command...>`)

```sh
$ sysg start
```

## Options

### `--config`

Path to the configuration file. If not specified, systemg looks for `systemg.yaml` or `sysg.yaml` in the current directory.

```sh
$ sysg start --config /etc/myapp/services.yaml
```

### `--service`

Optionally start only the named service instead of all services.

```sh
$ sysg start --service api
```

### `--name`

Optional name for ad-hoc units or child-start units.

```sh
$ sysg start --name cleanup -- ./cleanup.sh
```

### `--daemonize`

Run the supervisor as a background daemon. The supervisor continues running after you close your terminal, and subsequent commands communicate with it via Unix socket.

```sh
$ sysg start --daemonize
```

### `--parent-pid`

Run `start` in child mode by attaching the process to a parent service PID.

```sh
$ sysg start --parent-pid 4242 --name worker-1 -- python worker.py
```

### `--ttl`

Optional time-to-live in seconds for child mode.

```sh
$ sysg start --parent-pid 4242 --ttl 300 --name temp-worker -- ./job.sh
```

### `--child`

Explicit child-mode marker. Requires `--parent-pid`.

```sh
$ sysg start --child --parent-pid 4242 --name worker-1 -- python worker.py
```

### `--sys`

Opt into privileged system mode. Requires running as root.

```sh
$ sudo sysg start --sys
```

### `--drop-privileges`

Drop child service privileges during spawn. When enabled in root/system mode,
services without an explicit `user` run as `nobody`.

```sh
$ sudo sysg start --sys --drop-privileges
```

### `--log-level`

Set logging verbosity for this invocation. Accepts named levels (`trace`, `debug`, `info`, `warn`, `error`, `off`) or numeric values (5-0).

```sh
$ sysg start --log-level debug
```

## Examples

### Start with default configuration

```sh
$ sysg start
```

Looks for `systemg.yaml` or `sysg.yaml` in the current directory.

### Start with specific configuration

```sh
$ sysg start --config /etc/myapp/services.yaml
```

### Daemon mode

Run the supervisor in the background. Subsequent commands communicate with this long-lived process.

```sh
$ sysg start --daemonize
```

Check if the daemon is running:

```sh
$ sysg status
```

### Debug mode

See detailed output during startup:

```sh
$ sysg start --log-level debug
```

### Ad-hoc supervision (no config file)

```sh
$ sysg start --daemonize -- sleep 30
```

### Child mode for orchestrators (replacement for `spawn`)

```sh
$ sysg start --parent-pid 4242 --name worker-1 --ttl 900 -- python worker.py
```

## What happens

1. Manifest mode starts services in dependency order
2. Ad-hoc mode creates a single managed unit from the command
3. Child mode attaches a managed child to the parent process tree
4. Logs are written to `~/.local/share/systemg/logs/`
5. PIDs are tracked for other commands

In daemon mode, the supervisor monitors services and handles restarts according to your configuration.

## See also

- [`stop`](stop) - Stop running services
- [`status`](status) - Check service health
- [`restart`](restart) - Restart services
