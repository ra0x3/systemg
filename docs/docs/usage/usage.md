---
sidebar_position: 3
title: Usage
---

# Usage

Systemg uses a simple YAML-based configuration and supports the following commands:

## Start

Start the process manager with the given configuration:

```sh
# Start with default configuration file (systemg.yaml)
sysg start

# Start with a specific configuration file
sysg start --config systemg.yaml

# Start as a daemon process
sysg start --config systemg.yaml --daemonize
```

## Stop

Stop the process manager or a specific service:

```sh
# Stop all services
sysg stop

# Stop a specific service
sysg stop --service myapp
```

## Restart

Restart the process manager:

```sh
# Restart with current configuration
sysg restart

# Restart with a different configuration
sysg restart --config new-config.yaml
```

## Status

Check the status of running services:

```sh
# Show status of all services
sysg status

# Show status of a specific service
sysg status --service webserver
```

## Logs

View logs for a specific service:

```sh
# View the last 50 lines of logs for all services
sysg logs

# View logs for a specific service
sysg logs api-service

# View a custom number of log lines
sysg logs database --lines 100Predictable. Fast.
```

> Note that on Unix-like systems, the `logs` command is currently not supported.