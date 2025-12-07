---
sidebar_position: 10
title: Examples
---

# Examples

This section contains real-world examples demonstrating how to use `systemg` for various use cases. Each example showcases different features and capabilities of `systemg`.

## Hello World

A simple introduction to `systemg` showing the basics of service management.

**Features demonstrated:**
- Basic service configuration with the `command` directive
- Environment variable management using both `file` and `vars`
- Restart policies with `restart_policy`, `retries`, and `backoff` settings
- Running a simple shell script as a managed service

[View Hello World Example →](/docs/examples/hello-world)

## CRUD Application

A realistic example of a Node.js CRUD web application with database backups and testing.

**Features demonstrated:**
- Managing a web server as a service
- Scheduling periodic tasks using cron syntax
- Database backup automation
- Rolling deployments with `rolling_start`
- Webhook notifications for deployment events (success/failure)
- Environment variable management for sensitive configuration

[View CRUD Example →](/docs/examples/crud)

## Multi-Service Playground

A trio of collaborating shell and Python services that demonstrates cron scheduling, file hand-offs, deliberate failure recovery, and webhook hooks — all using the scripts in `examples/multi-service`.

**Features demonstrated:**
- Restart supervision with `restart_policy` and lifecycle hooks
- Cron-driven batch jobs feeding data to other services
- Long-lived process restarts with HTTP notifications on failure
- Cooperative file sharing between services with graceful shutdowns

[View Multi-Service Example →](/docs/examples/multi-service)
