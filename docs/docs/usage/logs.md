---
sidebar_position: 4
title: logs
---

| ⚠️ Note that on Unix-like systems, the logs command is currently not supported.   |
|----------------------------------------------|

View the last 50 lines of logs for all services.

```sh
sysg logs
```

View the logs for a specific service.

```sh
sysg logs api-service
```

View a custom number of log lines for a specific service.

```sh
sysg logs database --lines 100
```
