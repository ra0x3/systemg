---
sidebar_position: 1
title: Hello World
---

# Hello World

Basic service with restart policy.

## Files

**`hello-world.sh`**:
```bash
#!/bin/sh
i=1
while true; do
    echo "Line number: $i"
    i=$((i + 1))
    sleep 2
done
```

**`hello-world.sysg.yaml`**:
```yaml
version: "1"
services:
  sh__hello_world:
    command: "sh hello-world.sh"
    env:
      file: ".env"
      vars:
        FOO: "foo"
    restart_policy: "on_failure"
    retries: "5"
    backoff: "5s"
```

## Usage

```bash
cd examples/hello-world
sysg start
sysg status
sysg logs sh__hello_world
sysg stop
```

Output:
```
Line number: 1
Line number: 2
Line number: 3
...
```
