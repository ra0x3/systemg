---
sidebar_position: 5
title: Privileged Mode
---

# Privileged Mode

Run services with system-level privileges when needed.

## When to use

Enable privileged mode to:
- Bind to ports < 1024
- Run services as different users
- Apply resource limits
- Use Linux capabilities
- Create cgroups or namespaces

## Start with privileges

```bash
sudo sysg --sys start --daemonize
```

The `--sys` flag moves state to system directories:
- `/var/lib/systemg/` - Runtime state
- `/var/log/systemg/` - Logs
- `/etc/systemg/` - Configuration

## Configuration

```yaml
services:
  web:
    command: "./server"
    user: "www-data"
    group: "www-data"
    supplementary_groups: ["www-logs"]
    limits:
      nofile: 65536
      nproc: 4096
      memlock: "512M"
      nice: -5
      cpu_affinity: [0, 1]
      cgroup:
        memory_max: "512M"
        cpu_max: "200000 100000"
    capabilities:
      - CAP_NET_BIND_SERVICE
      - CAP_SYS_NICE
    isolation:
      network: true
      pid: true
```

## User and groups

Drop privileges to specific users:

```yaml
services:
  nginx:
    command: "nginx -g 'daemon off;'"
    user: "www-data"
    group: "www-data"
```

Service runs as `www-data` after binding to port 80.

## Resource limits

Control system resources per service:

| Field | Description |
|-------|-------------|
| `nofile` | Max open files |
| `nproc` | Max processes |
| `memlock` | Locked memory |
| `nice` | Priority (-20 to 19) |
| `cpu_affinity` | Pin to CPU cores |

## Capabilities

Retain specific capabilities after dropping root:

```yaml
capabilities:
  - CAP_NET_BIND_SERVICE  # Bind to privileged ports
  - CAP_SYS_NICE          # Adjust process priority
```

## Cgroups v2

Limit memory and CPU usage:

```yaml
limits:
  cgroup:
    memory_max: "512M"
    cpu_max: "200000 100000"  # 2 CPUs
```

## Namespaces

Isolate services from the host:

```yaml
isolation:
  network: true  # Private network namespace
  pid: true      # Private PID namespace
  mount: true    # Private mount namespace
```

## Examples

### Web server on port 80

```yaml
services:
  web:
    command: "./myapp"
    user: "appuser"
    capabilities:
      - CAP_NET_BIND_SERVICE
```

### Database with resource limits

```yaml
services:
  postgres:
    command: "postgres"
    user: "postgres"
    limits:
      nofile: 100000
      cgroup:
        memory_max: "4G"
```

## See also

- [Security](../security) - Security considerations
- [Configuration](configuration) - Service definitions