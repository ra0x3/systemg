# systemg

![CI](https://github.com/ra0x3/systemg/actions/workflows/ci.yaml/badge.svg)

<div display="flex" align-items="center"> 
    <img src="https://img.shields.io/badge/Rust-000000?style=for-the-badge&logo=rust&logoColor=white" />  
    <img src="https://img.shields.io/badge/ts--node-3178C6?style=for-the-badge&logo=ts-node&logoColor=white" />  
    <img src="https://img.shields.io/badge/Vite-B73BFE?style=for-the-badge&logo=vite&logoColor=FFD62E" />
    <img src="https://img.shields.io/badge/mac%20os-000000?style=for-the-badge&logo=apple&logoColor=white" />  
    <img src="https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black" />  
    <img src="https://img.shields.io/badge/ChatGPT-74aa9c?style=for-the-badge&logo=openai&logoColor=white" />  
</div>

# Systemg - A Lightweight Process Manager

Systemg is a **simple, fast, and dependency-free process manager** written in Rust.  
It aims to provide **a minimal alternative to systemd** and other heavyweight service managers, focusing on **ease of use**, **clarity**, and **performance**.

## Why Systemg?

Traditional process managers like **systemd** are complex, heavy, and introduce unnecessary dependencies.  
Systemg offers a **lightweight**, **configuration-driven** solution that's **easy to set up and maintain**.

## Features

- **Declarative YAML Configuration** - Define services, dependencies, and restart policies easily.
- **Automatic Process Monitoring** - Restart crashed services based on custom policies.
- **Environment Variable Support** - Load variables from `.env` files and per-service configurations.
- **Minimal & Fast** - Built with Rust, designed for performance and low resource usage.
- **No Root Required** - Unlike systemd, it doesn't take over PID 1.

---

## Comparison vs Alternatives

| Feature            | Systemg       | systemd         | Supervisor   | Docker Compose  |
|--------------------|-----------------|-----------------|-----------------|------------------|
| **Lightweight**    | Yes           | No (Heavy)   | No (Python)  | No (Containers) |
| **No Dependencies**| Yes           | No (DBus, etc.) | No (Python)  | No (Docker)    |
| **Simple Config**  | YAML          | Complex Units | INI          | YAML          |
| **Process Monitoring** | Yes      | Yes         | Yes         | Yes          |
| **PID 1 Required?**| No            | Yes         | No          | No           |
| **Handles Dependencies?** | Yes  | Yes         | No          | Yes          |

---

## Getting Started

### Installation

Install systemg using cargo:

```sh
cargo install sysg
```

Or download the pre-built binary from the releases page.

### Basic Commands

The `sysg` command-line interface provides several subcommands for managing processes.

#### Start

Start the process manager with the given configuration:

```sh
# Start with default configuration file (systemg.yaml)
sysg start

# Start with a specific configuration file
sysg start --config my-services.yaml

# Start as a daemon process
sysg start --daemonize
```

#### Stop

Stop the process manager or a specific service:

```sh
# Stop all services
sysg stop

# Stop a specific service
sysg stop --service myapp
```

#### Restart

Restart the process manager:

```sh
# Restart with current configuration
sysg restart

# Restart with a different configuration
sysg restart --config new-config.yaml
```

#### Status

Check the status of running services:

```sh
# Show status of all services
sysg status

# Show status of a specific service
sysg status --service webserver
```

#### Logs

View logs for a specific service:

```sh
# View the last 50 lines of logs for all services
sysg logs

# View logs for a specific service
sysg logs api-service

# View a custom number of log lines
sysg logs database --lines 100
```

## Testing

To run the test suite:

```sh
# Run all tests
cargo test

# Run specific test
cargo test test_service_lifecycle
```

## Build from Source

To build systemg from source:

```sh
# Clone the repository
git clone https://github.com/ra0x3/systemg.git
cd systemg

# Build the project
cargo build --release

# The binary will be available at target/release/sysg
```

## Contributing

Contributions to systemg are welcome! Please see the [CONTRIBUTING.md](CONTRIBUTING.md) file for guidelines.
