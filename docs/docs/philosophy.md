---
sidebar_position: 5
title: Philosophy
---

# Philosophy

systemg makes multi-service applications easy to run.

## Design principles

**Simple configuration** - YAML, not complex unit files
**Dependency aware** - Services start in the right order
**Single binary** - No runtime dependencies
**Dev to prod** - Same config works everywhere
**Least privilege** - Drop permissions by default

## Sweet spot

systemg works best for 5-50 interdependent services:
- Microservice applications
- Data pipelines
- Web apps with workers and cron jobs

## Not a fit for

- Single service apps (use systemd directly)
- 100+ services (use Kubernetes)
- Windows environments

## See also

- [Introduction](intro) - What systemg does
- [Configuration](how-it-works/configuration) - Define your system
