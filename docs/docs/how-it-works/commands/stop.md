---
sidebar_position: 2
title: stop
---

# stop

Stop running services.

```sh
sysg stop
```

## Options

### `--config`

Path to the configuration file. Used when no supervisor is running to locate services. When a supervisor is running, it uses the supervisor's configuration.

```sh
sysg stop --config /etc/myapp/services.yaml
```

### `--service`

Name of a specific service to stop. If not specified, all services are stopped. Leaves other services running.

```sh
sysg stop --service api
```

### `--log-level`

Set logging verbosity for this invocation. Accepts named levels (`trace`, `debug`, `info`, `warn`, `error`, `off`) or numeric values (5-0).

```sh
sysg stop --log-level debug
```

## Examples

### Stop all services

```sh
sysg stop
```

### Stop a specific service

```sh
sysg stop --service api
```

Leaves other services running.

## What happens

1. Services stop in reverse dependency order
2. Each service receives `SIGTERM`
3. After 10 seconds, `SIGKILL` is sent if needed
4. The supervisor exits (unless other services are running)

When stopping a single service, its dependents keep running. Only crashes trigger cascading stops.

## See also

- [`start`](start) - Launch services
- [`restart`](restart) - Restart services
