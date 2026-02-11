---
sidebar_position: 1
title: Hello World
---

# Hello World

Minimal systemg service.

## Configuration

```yaml
version: "1"
services:
  counter:
    command: "sh counter.sh"
```

## Script

```bash
#!/bin/sh
i=1
while true; do
    echo "Count: $i"
    i=$((i + 1))
    sleep 1
done
```

## Run it

```bash
sysg start
sysg logs --service counter
sysg stop
```

You'll see:
```
Count: 1
Count: 2
Count: 3
```

## Next steps

Add a restart policy to handle crashes:

```yaml
version: "1"
services:
  counter:
    command: "sh counter.sh"
    restart_policy: "always"
```
