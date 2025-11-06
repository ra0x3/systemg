---
sidebar_position: 2
title: stop
---


Stop all running services managed by `systemg`.

```sh
$ sysg stop
```

Stop a specific service by name.

```sh
$ sysg stop --service myapp
```

```
$ sysg stop --help
Stop the currently running process manager

Usage: sysg stop [OPTIONS]

Options:
  -c, --config <CONFIG>    Path to the configuration file (defaults to `systemg.yaml`) [default: systemg.yaml]
      --log-level <LEVEL>  Override the logging verbosity for this invocation only
  -s, --service <SERVICE>  Name of service to stop (optional)
  -h, --help               Print help
```
