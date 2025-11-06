---
sidebar_position: 1
title: start
---


Start the process manager with the default configuration file.

```sh
$ sysg start
```

Start the process manager with a specific configuration file.

```sh
$ sysg start --config systemg.yaml
```

Increase or decrease verbosity for the current invocation. Named levels are case-insensitive and numbers map to TRACE=5 down to OFF=0.

```sh
$ sysg start --log-level debug
$ sysg start --log-level 4
```

The same `--log-level` flag works with every `sysg` subcommand if you need different verbosity elsewhere.

When configuration files declare `depends_on` entries, `sysg start` bootstraps services in dependency order. If a prerequisite fails to start, its dependents are skipped and the command exits with a dependency error instead of leaving the system half-running.

```
$ sysg start --help
Start the process manager with the given configuration

Usage: sysg start [OPTIONS]

Options:
  -c, --config <CONFIG>    Path to the configuration file (defaults to `systemg.yaml`) [default: systemg.yaml]
      --log-level <LEVEL>  Override the logging verbosity for this invocation only
      --daemonize          Whether to daemonize systemg
  -h, --help               Print help
```
