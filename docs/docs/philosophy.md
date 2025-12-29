---
sidebar_position: 11
title: Philosophy
---

# Philosophy

Systemg embraces simplicity through declarative YAML configuration that makes service management immediately understandable. Rather than opaque scripts or complex APIs, every service is defined explicitly—one command, one purpose—giving you full visibility into what's running and how it starts. The configuration-driven approach eliminates guesswork and makes the system state auditable at a glance.

The single-responsibility model extends from individual services to the tool itself: systemg manages processes, nothing more. This makes it ideal for microservices architectures where each service does one thing well, and for monorepos where multiple services coexist but remain independently manageable. Services declare their dependencies explicitly, coordinate through well-defined contracts, and fail independently without cascading surprises.

Built in Rust with zero runtime dependencies, systemg runs anywhere—bare metal, VMs, containers, or development machines—without requiring root privileges or system-level integration. Whether orchestrating scripts in a monorepo or deploying containerized microservices, systemg stays out of your way while keeping your processes reliable.
