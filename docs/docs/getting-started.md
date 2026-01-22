---
sidebar_position: 1
title: Getting Started
---

# Getting Started

systemg is a **general-purpose program composer** that transforms arbitrary programs into coherent systems. Built in Rust with no runtime dependencies, it composes programs with explicit lifecycles, dependencies, and health monitoring—turning collections of processes into systems you can reason about, evolve, and deploy cleanly.

This guide walks you through installation and basic usage of systemg's composition capabilities.

## System requirements

> systemg runs as a single binary with no external dependencies, leveraging existing OS primitives when available while maintaining complete independence.

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
