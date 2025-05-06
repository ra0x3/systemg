---
sidebar_position: 3
title: Usage
---

# Usage

Systemg uses a simple YAML-based configuration and supports the following commands:

## Start

```bash
sysg start
sysg start --config custom.yaml
sysg start --config systemg.yaml --daemonize
```

## Stop

```bash
sysg stop
sysg stop --service myapp
```

## Restart

```bash
sysg restart
sysg restart --config alt.yaml
```

## Status

```bash
sysg status
sysg status --service myapp
```

## Logs

```bash
sysg logs
sysg logs api-service
sysg logs database --lines 100
```

Elegant. Predictable. Fast.