# systemg

An agent-friendly general process composer.

<br/><br/>

<div align="center" >
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://i.imgur.com/lkKPMoX.png" />
    <source media="(prefers-color-scheme: light)" srcset="https://i.imgur.com/13cCBze.png" />
    <img src="https://i.imgur.com/13cCBze.png" alt="systemg" width="320" />
  </picture>
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

For system-wide deployments, `scripts/install-systemg.sh` sets up `/usr/bin/sysg`, `/etc/systemg`, and `/var/lib/systemg` — see the [security guide](docs/security.mdx).

### Usage

Describe your system in a `systemg.yaml`:

```yaml
version: "1"
services:
  postgres:
    command: "postgres -D ./data"
    restart_policy: "always"

  api:
    command: "gunicorn app:application --bind 0.0.0.0:8000"
    depends_on:
      - postgres
    restart_policy: "on-failure"
    max_restarts: 5
    backoff: "5s"
    deployment:
      strategy: "rolling"
      health_check:
        command: "curl --fail http://localhost:8000/health"

  backup:
    command: "sh backup.sh"
    cron:
      expression: "0 0 2 * * *"
```

Then run it:

```sh
sysg start --daemonize      # start everything, in dependency order
sysg status                 # see what's running
sysg logs --service api -f  # follow one service's logs
sysg restart --service api  # bounce one service, not the world
```

That's the whole workflow. Log rotation, output sinks, and status-snapshot tuning are covered in the [configuration docs](docs/how-it-works/configuration.mdx).

## Why systemg

You declare your processes, their dependencies, and their health checks in one file. systemg starts them in topological order, restarts them according to policy, and won't call a rolling deploy done until the new process passes its health check.

It sits in the gap between systemd and Docker Compose. systemd wants to own the whole machine. Compose has the right composition model but makes you adopt containers to get it. Supervisor is close, but has no dependency graph and needs a Python runtime. systemg is one static binary that runs the same in a VM, a container, or on a Raspberry Pi — and everything it knows is readable back out through `sysg status` and `sysg inspect`, so scripts and coding agents can drive it as easily as you can.

### Features

- Dependency-ordered startup, gated on health checks
- Rolling deployments: blue-green process swap, health-validated
- Restart policies with backoff
- Cron jobs with overlap detection
- Lifecycle [hooks](docs/how-it-works/webhooks.mdx) on start/stop
- `.env` file propagation
- Tracks child processes your services spawn
- CPU/RSS metrics built into `status` and `inspect`
- [Privileged mode](docs/how-it-works/privileged-mode.mdx): per-service user/group, capabilities, rlimits, namespaces
- Uses systemd/cgroups when present; needs neither

## How systemg Compares

| | systemg | systemd | Supervisor | Docker Compose |
|---------|---------|---------|------------|----------------|
| **Focus** | Program composition | System management | Process supervision | Container orchestration |
| **Config** | YAML | Unit files | INI | YAML |
| **Dependencies** | Topological, health-aware | Unit chains | Manual ordering | Service links |
| **Deployments** | Built-in rolling | External tooling | Manual restarts | Recreate/rolling |
| **Runtime deps** | None | DBus, journal | Python | Docker daemon |

Full documentation lives at [sysg.dev](https://sysg.dev).
