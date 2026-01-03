---
sidebar_position: 11
title: Philosophy
---

# Philosophy

## Positioning Against Process Managers

Systemg occupies the sweet spot between heavyweight system managers and simplistic process runners—it's a **userspace process supervisor** designed for modern application architectures. Unlike `systemd` which requires root privileges and deep OS integration, systemg runs entirely in userspace with a single YAML file (`sysg start --config app.yaml`), making it perfect for containers, development environments, and situations where you lack system access. Unlike `supervisord` which hasn't evolved much since 2004, systemg provides modern deployment strategies (rolling restarts with health checks), native webhook support, and cron scheduling without external dependencies or Python runtime overhead.

## Technical Architecture

Where `systemd` manages the entire system boot and `supervisor` manages legacy daemon processes, systemg focuses exclusively on **application service orchestration** with zero assumptions about your environment. It's built in Rust as a single static binary with no runtime dependencies, contrasting sharply with `PM2`'s Node.js requirement or `supervisor`'s Python dependency—you get predictable memory usage, instant startup, and deployment simplicity. The persistent state design (`~/.local/share/systemg/`) means your services survive supervisor restarts, unlike `foreman`/`overmind` which lose all state on exit, while the [`config_hint`](./state.md#config_hint) mechanism eliminates the need to specify configuration paths repeatedly (a common frustration with `supervisor`'s `supervisorctl -c`).

When you graduate from userspace requirements into system-level responsibilities, systemg keeps the same philosophy: **privileged mode is opt-in and least-privilege by default**. You can start the supervisor with `--sys` to relocate state into `/var/lib/systemg`, bind privileged ports, or attach cgroups/namespaces, yet services only retain the Linux capabilities you explicitly list. If you omit them, systemg clears every capability set before dropping root so the process stays as unprivileged as possible. This lets teams adopt kernel-space integrations incrementally without giving up the simplicity that makes userspace deployments predictable.

## Service Model Philosophy

Systemg treats services as **first-class declarative resources** with explicit dependencies, not just background processes to keep alive—imagine Docker Compose's service definitions but for native processes instead of containers. Each service gets automatic log rotation, structured process groups for clean termination (solving `supervisor`'s infamous orphaned subprocess problem), and lifecycle hooks that actually work without shell scripting gymnastics. The topological dependency resolution ensures services start in the correct order and cascade failures appropriately, unlike `supervisor`'s manual priority system or `systemd`'s complex `After=/Wants=/Requires=` semantics that often surprise users.

## Development & Production Convergence

The same `systemg.yaml` that orchestrates your local development environment scales to production deployments, eliminating the dev/prod divergence that plagues teams using `foreman` locally but `systemd` in production. Built-in deployment strategies like rolling restarts with health checks mean you don't need a separate deployment tool—systemg handles the blue-green swap that would require custom `systemd` units or `supervisor` event listeners to achieve. The cron integration runs jobs in the same environment as your services (unlike system `cron` which runs in a minimal environment), while webhook support enables GitOps workflows without additional middleware.

## When to Choose Systemg

Choose systemg when you need more than `foreman` but less than Kubernetes—it excels at orchestrating 5-50 services in a monorepo, managing microservices on a single host, or providing process supervision inside containers where `systemd` isn't available. It's particularly powerful for polyglot environments where `PM2`'s Node.js focus or `supervisor`'s Python requirement creates friction, and for teams who want Heroku Procfile simplicity with production-grade features like health checks and zero-downtime deployments. If you're fighting `systemd`'s complexity for application services, debugging `supervisor`'s Python tracebacks, or wishing `foreman` had state persistence and proper logging, systemg provides the modern alternative you've been seeking.
