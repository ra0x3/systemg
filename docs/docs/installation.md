---
sidebar_position: 2
title: Installation
---

# Installation

Choose the installation method that works best for your system.

## Quick Install (Recommended)

### Install Latest Version

The fastest way to get started with `systemg`:

```bash
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh
```

### Install Specific Version

To install a specific version of `systemg`:

```bash
# Install version 0.15.6
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- --version 0.15.6

# Short form
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- -v 0.15.6
```

### Switch Between Versions

If you have multiple versions installed, you can switch between them:

```bash
# Switch to version 0.15.5 (downloads if not already installed)
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- -v 0.15.5
```

### How It Works

The installation script:
- Downloads the `systemg` binary for your platform
- Installs to `~/.sysg/versions/VERSION/`
- Creates a symlink at `~/.sysg/bin/sysg` to the active version
- Adds `~/.sysg/bin` to your PATH (if not already present)
- Manages multiple versions side-by-side

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

## Next Steps

After installation, check out the [Examples](/docs/examples) to see `systemg` in action with real-world use cases.
