---
sidebar_position: 1
title: Getting Started
---

# Getting Started

`systemg` is a lightweight, Rust-based process manager built for speed, simplicity, and clarity.
It manages long-running services, fires lifecycle [webhooks](./webhooks.md), and can operate
cron-style automation alongside your always-on workloads.

This guide walks you through installation and basic usage.

## System requirements

> `systemg` has minimal system dependencies, which is one of its key advantages over systemd.

**Currently supported:**

| OS | Distribution | Architecture | Supported |
|---|---|---|---|
| Linux | Generic | x86_64 | ✅ |
| Linux | Generic | aarch64 | ✅ |
| Linux | Debian | x86_64 | ✅ |
| Linux | Alpine | x86_64 | ✅ |
| Linux | Alpine | aarch64 | ✅ |
| macOS | - | x86_64 | ✅ |
| macOS | - | aarch64 (Apple Silicon) | ✅ |

**Installation requirements:**
- No additional packages or dependencies to install
- The binary includes all necessary system libraries
- Works on any Linux system with standard glibc

## System dependencies

The following system libraries are required (typically pre-installed on Linux systems):

| Library | Purpose |
|---------|---------|
| `glibc` | Standard C library for system calls |
| `libc` | Low-level system interface |
| `pthread` | POSIX threads support |
| `dl` | Dynamic linking support |

These dependencies are compiled into the binary, so no additional installation is required.
