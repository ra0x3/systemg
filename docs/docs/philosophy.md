---
sidebar_position: 99
title: Philosophy
---

# Philosophy

## systemg as a Program Composer

systemg represents a fundamental shift from process management to **program composition**. While traditional service managers focus on keeping individual processes alive, systemg focuses on how programs work together as coherent systems. It's not about managing daemons or containers—it's about composing arbitrary programs with explicit lifecycles, dependencies, and health semantics into systems you can reason about and evolve.

Unlike `systemd` which manages the entire OS boot sequence with complex unit files, systemg lets you declare program relationships in simple YAML. Unlike `supervisord` which treats each process in isolation, systemg understands how programs depend on and affect each other. The result is a tool that runs in userspace with zero dependencies (`sysg start --config app.yaml`), perfect for containers, development environments, and production deployments alike.

## Technical Architecture

systemg's architecture embodies the principle of **composition over configuration**. Where `systemd` requires understanding unit files, targets, and system states, systemg presents programs as composable units with clear relationships. Built in Rust as a single static binary, it leverages existing OS primitives (systemd, cgroups) when available while maintaining complete independence—no DBus, no journal, no PID 1 takeover.

The persistent state design (`~/.local/share/systemg/`) ensures your composed systems survive supervisor restarts, preserving the runtime context that makes debugging and evolution possible. The [`config_hint`](./state.md#config_hint) mechanism remembers your system configuration, eliminating repetitive path specifications. This isn't just convenience—it's recognition that a composed system has identity beyond its individual programs.

When you graduate from userspace requirements into system-level responsibilities, systemg keeps the same philosophy: **privileged mode is opt-in and least-privilege by default**. You can start the supervisor with `--sys` to relocate state into `/var/lib/systemg`, bind privileged ports, or attach cgroups/namespaces, yet services only retain the Linux capabilities you explicitly list. If you omit them, systemg clears every capability set before dropping root so the process stays as unprivileged as possible. This lets teams adopt kernel-space integrations incrementally without giving up the simplicity that makes userspace deployments predictable.

## Composition Model Philosophy

systemg treats programs as **composable building blocks** rather than isolated services. Each program declaration includes not just how to run it, but how it relates to other programs—dependencies, health criteria, deployment strategies. This creates systems where the whole is genuinely greater than the sum of its parts.

The topological dependency resolution ensures programs start in meaningful order and respond appropriately to failures, but this isn't just about startup sequencing. It's about declaring intent: "this API depends on this database" becomes an enforceable contract, not a deployment note. Rolling deployments with health checks aren't bolted on—they're natural consequences of understanding programs as parts of a larger system.

## Development & Production Convergence

The same `systemg.yaml` that orchestrates your local development environment scales to production deployments, eliminating the dev/prod divergence that plagues teams using `foreman` locally but `systemd` in production. Built-in deployment strategies like rolling restarts with health checks mean you don't need a separate deployment tool—systemg handles the blue-green swap that would require custom `systemd` units or `supervisor` event listeners to achieve. The cron integration runs jobs in the same environment as your services (unlike system `cron` which runs in a minimal environment), while webhook support enables GitOps workflows without additional middleware.

## When to Choose systemg

Choose systemg when you need to **compose programs into systems**, not just keep processes alive. It excels when you have 5-50 interdependent programs that need to work together coherently—whether that's microservices in a monorepo, a data pipeline with multiple stages, or a web application with background workers and scheduled jobs.

systemg shines in environments where the relationships between programs matter as much as the programs themselves. If you're manually orchestrating startup order, implementing health checks in bash, or wishing your process manager understood that your API shouldn't start without its database, systemg provides the composition layer you need. It transforms a collection of programs into a system with predictable behavior, making complex deployments as simple as `sysg start --config system.yaml`.
