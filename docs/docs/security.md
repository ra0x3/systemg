---
sidebar_position: 4
title: Security
---

# Security

Defense-in-depth security features for production deployments.

## Principle of least privilege

Services drop to minimum required permissions:

```yaml
services:
  web:
    command: "./server"
    user: "www-data"
    capabilities:
      - CAP_NET_BIND_SERVICE  # Only what's needed
```

## Current features

### Capabilities (Linux)

Services retain only specified capabilities:

```yaml
capabilities:
  - CAP_NET_BIND_SERVICE  # Bind ports < 1024
  - CAP_SYS_NICE          # Adjust priority
  - CAP_DAC_READ_SEARCH   # Read any file
```

### Resource limits

Prevent resource exhaustion:

```yaml
limits:
  nofile: 65536      # Max file descriptors
  nproc: 1024        # Max processes
  memlock: "100M"    # Locked memory
  cgroup:
    memory_max: "2G"
    cpu_max: "100000 50000"  # 1 CPU
```

### Namespace isolation

Isolate from host system:

```yaml
isolation:
  network: true  # Private network
  pid: true      # Private process tree
  mount: true    # Private mounts
  user: true     # User namespace
```

## Upcoming features

| Feature | Status | Purpose |
|---------|--------|---------|
| `seccomp` | Planned | System call filtering |
| `apparmor_profile` | Planned | Mandatory access control |
| `selinux_context` | Planned | SELinux labels |
| `private_devices` | In progress | Device isolation |
| `private_tmp` | In progress | Temp directory isolation |

## Best practices

### Run unprivileged when possible

```bash
# User mode (default)
sysg start

# System mode (only when needed)
sudo sysg --sys start
```

### Drop privileges immediately

```yaml
services:
  nginx:
    command: "nginx"
    user: "www-data"  # Drops root after binding port 80
```

### Isolate untrusted workloads

```yaml
services:
  untrusted:
    command: "./third-party-app"
    user: "nobody"
    isolation:
      network: true
      pid: true
    limits:
      cgroup:
        memory_max: "100M"
```

## Troubleshooting

**Permission denied on namespace creation**
- Add `CAP_SYS_ADMIN` capability

**Cgroup write failures in containers**
- Set `limits.cgroup.root` to writable path

**Socket activation with systemd**
- systemg preserves `LISTEN_FDS` automatically

## See also

- [Privileged Mode](how-it-works/privileged-mode) - System-level features
- [Configuration](how-it-works/configuration) - Security options