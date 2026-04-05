# systemg

A general-purpose program orchestrator.

<br/><br/>

<div align="center" >
  <img src="https://i.imgur.com/13cCBze.png" alt="systemg" width="320" />
</div>

<br/><br/>

<div align="center">

[![CI](https://img.shields.io/github/actions/workflow/status/ra0x3/systemg/ci.yaml?branch=main&style=flat-square&logo=github&label=CI)](https://github.com/ra0x3/systemg/actions/workflows/ci.yaml)
[![GitHub branch status](https://img.shields.io/github/checks-status/ra0x3/systemg/main?style=flat-square&label=checks)](https://github.com/ra0x3/systemg/actions)

[![docs.rs (with version)](https://img.shields.io/docsrs/systemg/latest)](https://docs.rs/systemg)
[![GitHub tag](https://img.shields.io/github/v/tag/ra0x3/systemg?style=flat-square&logo=github&label=version)](https://github.com/ra0x3/systemg/releases)
[![Crate size](https://img.shields.io/crates/size/systemg?style=flat-square&logo=rust&label=size)](https://crates.io/crates/systemg)
![Crates.io Total Downloads](https://img.shields.io/crates/d/systemg)

[![Deps.rs Crate Dependencies (specific version)](https://img.shields.io/deps-rs/systemg/latest)](https://deps.rs/crate/systemg)
[![License](https://img.shields.io/crates/l/systemg?style=flat-square)](LICENSE)

</div>

<div align="center">
    <img src="https://img.shields.io/badge/Rust-000000?style=for-the-badge&logo=rust&logoColor=white" />
    <img src="https://img.shields.io/badge/ts--node-3178C6?style=for-the-badge&logo=ts-node&logoColor=white" />
    <img src="https://img.shields.io/badge/Vite-B73BFE?style=for-the-badge&logo=vite&logoColor=FFD62E" />
    <img src="https://img.shields.io/badge/mac%20os-000000?style=for-the-badge&logo=apple&logoColor=white" />
    <img src="https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black" />
    <img src="https://img.shields.io/badge/OpenAI-412991?style=for-the-badge&logo=openai&logoColor=white" />
    <img src="https://img.shields.io/badge/Anthropic-D97757?style=for-the-badge&logo=anthropic&logoColor=white" />
</div>

[//]: # (<img height="500px" src="https://i.imgur.com/MHXfe9T.png" />)

## Table of Contents

1. [Read the Docs](https://sysg.dev)
2. [Getting Started](#getting-started)
   - 2.1 [Installation](#installation)
   - 2.2 [Usage](#usage)
3. [Why systemg](#why-systemg)
   - 3.1 [Features](#features)
4. [How systemg Compares](#how-systemg-compares)

## Getting Started

### Installation

![Installation](https://i.imgur.com/6d2aq0U.gif)

```sh
$ curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh
```

System deployments: `scripts/install-systemg.sh` sets up `/usr/bin/sysg`, `/etc/systemg`, `/var/lib/systemg`. See [security guide](docs/docs/security.md).

### Usage

| Command | Description |
|---------|-------------|
| `sysg start` | Start the default `systemg.yaml` in the foreground. |
| `sysg start --config my.yaml` | Start a specific config file. |
| `sysg start --daemonize` | Launch the supervisor in the background. |
| `sysg status` | Check current service state. |
| `sysg logs --service api` | View logs for a specific service. |
| `sysg restart --service api` | Restart one service without restarting everything. |

> **Tip:** `--stderr` redirects stderr from supervised processes to stdout with a `[service_name:stderr]` prefix, which is useful for debugging and CI pipelines.

---

## Why systemg

Compose programs into systems with explicit dependencies and health checks.

### Features

- **Dependencies** - Topological startup order with health-aware cascading
- **Rolling Deployments** - Blue-green swaps with health validation
- **Environment** - `.env` file propagation
- **Webhooks** - Event notifications ([docs](docs/docs/webhooks.md))
- **Cron** - Scheduled tasks with overlap detection
- **Spawning** - Dynamic child process tracking
- **OS Integration** - systemd/cgroups when available
- **Single Binary** - No runtime dependencies

## How systemg Compares

| Feature | systemg | systemd | Supervisor | Docker Compose |
|---------|---------|---------|------------|----------------|
| **Focus** | Program composition | System management | Process supervision | Container orchestration |
| **Configuration** | Declarative YAML | Unit files | INI files | YAML |
| **Dependencies** | Topological with health checks | Complex unit chains | Manual ordering | Service links |
| **Deployments** | Built-in rolling workflows | External tooling | Manual restarts | Recreate/rolling |
| **Runtime deps** | None | DBus, journal | Python | Docker daemon |
| **OS integration** | Optional | Required | None | Container runtime |
