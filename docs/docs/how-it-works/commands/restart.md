---
sidebar_position: 3
title: restart
---

# restart

Restart services with zero downtime.

```sh
$ sysg restart
```

## Options

### `--config`

Path to the configuration file. When specified, reloads the configuration and restarts all services with the new settings.

```sh
$ sysg restart --config production.yaml
```

### `--service`

Name of a specific service to restart. If not specified, all services are restarted.

```sh
$ sysg restart --service api
```

### `--daemonize`

Start the supervisor before restarting if it isn't already running.

```sh
$ sysg restart --daemonize
```

### `--sys`

Opt into privileged system mode. Requires running as root.

```sh
$ sudo sysg restart --sys
```

### `--drop-privileges`

Drop privileges after performing privileged setup.

```sh
$ sudo sysg restart --sys --drop-privileges
```

### `--log-level`

Set logging verbosity for this invocation. Accepts named levels (`trace`, `debug`, `info`, `warn`, `error`, `off`) or numeric values (5-0).

```sh
$ sysg restart --log-level debug
```

## Examples

### Restart all services

```sh
$ sysg restart
```

### Restart specific service

```sh
$ sysg restart --service api
```

### Restart with new configuration

```sh
$ sysg restart --config production.yaml
```

Reloads configuration and restarts all services.

## Deployment strategies

Services configured with `deployment.strategy: rolling` get zero-downtime restarts:

1. New instance starts
2. Health checks pass
3. Old instance receives `SIGTERM`
4. Grace period allows requests to complete
5. Old instance stops

Services without rolling deployment stop then start.

## See also

- [`start`](start) - Launch services
- [`stop`](stop) - Stop services
- [Deployment strategies](../configuration#deployment-strategies)
