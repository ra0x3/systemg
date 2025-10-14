---
sidebar_position: 1
title: Getting Started
---

# Getting Started

`systemg` is a lightweight, Rust-based process manager built for speed, simplicity, and clarity.

This guide walks you through installation and basic usage.

## System requirements

`systemg` has minimal system dependencies, which is one of its key advantages over systemd.

**Currently supported:**
- Linux (all major distributions)

**Coming soon:**
- Alpine Linux
- macOS

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
