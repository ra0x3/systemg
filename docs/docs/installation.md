---
sidebar_position: 2
title: Installation
---

# Installation

Choose the installation method that works best for your system.

## Quick Install (Recommended)

The fastest way to get started with `systemg`:

```bash
curl -fsSL https://sh.sysg.dev | sh
```

This script will:
- Download the latest `systemg` binary
- Add `sysg` to your system PATH
- Make the command available system-wide

## Install from Source

If you prefer to build from source or want the latest development version:

```bash
cargo install sysg
```

**Requirements:**
- Rust toolchain (install via [rustup](https://rustup.rs))
- Git (to clone the repository)

## Verify Installation

After installation, verify that `systemg` is working correctly:

```bash
sysg --version
```

You should see output similar to:
```
systemg 0.6.8
```
