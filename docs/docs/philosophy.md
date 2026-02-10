---
sidebar_position: 99
title: Philosophy
---

# Philosophy

## Design

systemg composes programs into systems with dependencies and health checks. Unlike systemd (complex unit files) or supervisord (isolated processes), systemg uses simple YAML to declare relationships.

## Architecture

- Single static binary (Rust)
- Persistent state in `~/.local/share/systemg/`
- Leverages OS primitives when available (systemd, cgroups)
- Privileged mode optional (`--sys` flag)
- Least-privilege by default

## Use Cases

Best for 5-50 interdependent programs:
- Microservices in monorepos
- Data pipelines
- Web apps with workers and scheduled jobs

Same config works dev to prod. No separate deployment tools needed.
