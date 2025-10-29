---
sidebar_position: 3
title: Usage
---

# Usage

Systemg uses a simple YAML-based configuration and supports the following commands:

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

Show the status of all services:

```sh
sysg status
```

Show the status of a specific service:

```sh
sysg status --service webserver
```

## Logs

View the last 50 lines of logs for all services:

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

> ⚠️ Note: On Unix-like systems, the logs command is currently not supported.
