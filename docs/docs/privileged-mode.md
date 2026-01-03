---
sidebar_position: 12
title: Privileged Mode
---

# Privileged Mode

Systemg can manage operating-system level services when started with elevated privileges. Privileged mode is **opt-in**. By default the supervisor continues to run entirely in userspace under the invoking account.

## When to Enable

Use privileged mode when you need to:

- Bind to privileged ports (e.g. reverse proxies serving on ports < 1024)
- Run daemons as specific system users or groups
- Apply `setrlimit` resource caps (open files, processes, locked memory)
- Retain Linux capabilities after dropping root privileges
- Attach processes to dedicated cgroups or namespaces

## Starting the Supervisor

```sh
# Start the supervisor with system-level directories and default configuration
$ sudo sysg --sys start --daemonize

# Bind as root, then drop to the service's configured user once sockets are open
$ sudo sysg --sys --drop-privileges start --service web

# Status, logs, and stop commands continue working unprivileged
$ sysg status --service web
```

When `--sys` is supplied the runtime state moves to `/var/lib/systemg`, supervisor logs are written to `/var/log/systemg/supervisor.log`, and configuration files are resolved from `/etc/systemg` in addition to the working directory.

## Service Configuration Fields

The following optional fields can appear on each service definition:

```yaml
services:
  web:
    command: "./server"
    user: "www-data"                  # Target user (requires root)
    group: "www-data"                 # Target primary group
    supplementary_groups: ["www-logs"]
    limits:
      nofile: 65536
      nproc: 4096
      memlock: "unlimited"       # accepts numeric or K/M/G/T suffixes
      nice: -5
      cpu_affinity: [0, 1]
      cgroup:
        memory_max: "512M"
        cpu_max: "200000 100000"
        root: "/sys/fs/cgroup/systemg"  # optional override (use a temp dir for testing)
    capabilities:
      - CAP_NET_BIND_SERVICE
      - CAP_SYS_NICE
    isolation:
      network: true
      pid: true
```

### `user`, `group`, `supplementary_groups`

- Resolve to accounts via `/etc/passwd` and `/etc/group`
- Require the supervisor to run as root (otherwise a permission error is raised)
- Update `HOME`, `USER`, `LOGNAME`, and `SHELL` for the spawned process

### `limits`

Systemg applies resource limits before `exec` using `setrlimit`, `setpriority`, and `sched_setaffinity`:

| Field            | Description                                |
| ---------------- | ------------------------------------------ |
| `nofile`         | Maximum open file descriptors (`RLIMIT_NOFILE`) |
| `nproc`          | Maximum processes (`RLIMIT_NPROC`, Unix only) |
| `memlock`        | Locked memory in bytes (`RLIMIT_MEMLOCK`)   |
| `nice`           | Process scheduling priority (-20..19)       |
| `cpu_affinity`   | CPU cores to pin the process to (Linux)     |
| `cgroup.*`       | Optional cgroup v2 settings (Linux)         |

### `capabilities`

On Linux, capabilities are retained across the privilege drop by using the [`caps`](https://crates.io/crates/caps) crate. Systemg clears every capability set by default; explicitly listing values (for example `CAP_NET_BIND_SERVICE`) grants only those. Unsupported on macOS; a warning is emitted instead of failing compilation.

### `limits.cgroup`

- When running with `--sys`, systemg can attach services to cgroup v2 controllers before `exec`.
- `root` lets you direct systemg to an alternate mount point (useful for tests or chrooted environments).
- Writes are best-effort; if the kernel denies access, a warning is logged and startup continues.
- When launched by systemd socket activation (`LISTEN_FDS`), systemg preserves the provided descriptors so services can adopt them without reopening privileged sockets.
- Refer to `scripts/install-systemg.sh`, `examples/system-mode.yaml`, and [`security.md`](./security.md) for systemd packaging and the broader hardening roadmap.

### `isolation`

Namespace and sandbox toggles (Linux only): `network`, `mount`, `pid`, `user`, `private_devices`, `private_tmp`, as well as placeholders for `seccomp`, `apparmor_profile`, and `selinux_context`. Unsupported toggles emit warnings when the kernel lacks the feature.

- Namespaces leverage `unshare` before `exec`; combine them with capability configuration when a feature (e.g. `CLONE_NEWNET`) requires specific privileges.
- `private_devices` and `private_tmp` are advisory toggles today and emit warnings until full device/tmpfs remounting is implemented.

## Behaviour Without Root

- `sysg --sys` returns a permission error unless run as root.
- Services that request user/group switching return `PermissionDenied` when the supervisor lacks privileges.
- `--drop-privileges` is ignored when not running as root (a warning is logged).

Automated tests skip privileged scenarios when executed without root permissions, ensuring CI safety while still exercising the code paths.

## Cleaning Up

System mode stores state in `/var/lib/systemg` and logs in `/var/log/systemg`. Use `sudo sysg purge` to stop resident supervisors and remove both directories when you want a clean slate.
