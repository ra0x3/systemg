---
sidebar_position: 13
title: Security Hardening
---

# Security Hardening

Systemg's privileged features are intentionally opt-in, enabling operators to layer additional kernel protections only when required. This page summarises the current implementation status and outlines the roadmap for upcoming security features.

## Capabilities

Implemented via the [`caps`](https://crates.io/crates/caps) crate on Linux:

- Capabilities default to none; specifying a list retains only those entries across the privilege drop.
- Ambient capabilities are set after the UID switch so services can bind privileged ports without running as root.
- Non-Linux targets log warnings when capabilities are requested.

## Resource Limits & Cgroups

- `limits` maps to `setrlimit`, `setpriority`, and `sched_setaffinity`.
- `limits.cgroup` writes to cgroup v2 controllers (memory/cpu); the optional `root` field helps in chroot/testing scenarios.
- Writes are best-effort: the supervisor logs warnings when the kernel denies access instead of failing the start.

## Namespace Isolation

- `isolation` toggles (`network`, `pid`, `mount`, `user`) call `unshare` on Linux. Unsupported kernels or missing privileges produce warnings.
- `private_devices` and `private_tmp` are placeholders; they warn until full device / tmpfs remounting is implemented.

## Upcoming Features

| Feature                | Status           | Notes                                             |
| ---------------------- | ---------------- | ------------------------------------------------- |
| `seccomp`              | Planned          | Will integrate filter profiles prior to `exec`.   |
| `apparmor_profile`     | Planned          | Requires operating system policy support.         |
| `selinux_context`      | Planned          | Will rely on `setfscreatecon` where available.    |
| Device isolation       | Planned          | `private_devices` toggle currently logs warnings. |
| Temporary filesystem isolation | Planned  | `private_tmp` toggle currently logs warnings.     |

Future updates will extend the CLI and configuration to surface kernel-space enforcement more explicitly. Track progress via the GitHub issue queue.


## Troubleshooting

- If namespace unshare fails with `EPERM`, ensure the service retains the required capabilities (e.g. `CAP_SYS_ADMIN` for `CLONE_NEWNS`).
- Cgroup writes may fail inside containers; adjust `limits.cgroup.root` to point to a writable hierarchy (e.g. `/sys/fs/cgroup/user.slice/...`).
- Socket activation requires clearing `LISTEN_PID/LISTEN_FDS` once the supervisor has captured them; systemg does this automatically.

## References

- [systemd Socket Activation](https://www.freedesktop.org/software/systemd/man/systemd.socket.html)
- [man capabilities](https://man7.org/linux/man-pages/man7/capabilities.7.html)
- [man namespaces](https://man7.org/linux/man-pages/man7/namespaces.7.html)
- [Kernel cgroup v2](https://www.kernel.org/doc/html/latest/admin-guide/cgroup-v2.html)

