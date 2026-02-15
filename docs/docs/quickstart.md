---
sidebar_position: 1
title: Quickstart
---

# Quickstart

Get systemg running in 60 seconds.

## Install

```bash
$ curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh
```

## Create a configuration

```yaml
version: "1"
services:
  hello:
    command: "python -m http.server 8080"
```

Save as `systemg.yaml`.

## Start your service

```bash
$ sysg start
```

Your web server is now running at http://localhost:8080.

## Check status

```bash
$ sysg status
```

```
SERVICE  STATUS   PID    UPTIME
hello    running  42309  3s
```

## View logs

```bash
$ sysg logs --service hello
```

## Stop everything

```bash
$ sysg stop
```

## What's next

- [Configure multiple services](how-it-works/configuration) with dependencies
- [Run services in the background](how-it-works/commands/start#daemon-mode) with `--daemonize`
- [Set up automatic restarts](how-it-works/configuration#restart-policies) for production
