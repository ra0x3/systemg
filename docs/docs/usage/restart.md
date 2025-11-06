---
sidebar_position: 3
title: restart
---

Restart the process manager using the current configuration.

```sh
$ sysg restart
```

Restart the process manager using a different configuration file.

```sh
$ sysg restart --config new-config.yaml
```

```
$ sysg restart --help
Restart the process manager, optionally specifying a new configuration file

Usage: sysg restart [OPTIONS]

Options:
  -c, --config <CONFIG>    Path to the configuration file (defaults to `systemg.yaml`) [default: systemg.yaml]
      --log-level <LEVEL>  Override the logging verbosity for this invocation only
  -s, --service <SERVICE>  Optionally restart only the named service
      --daemonize          Start the supervisor before restarting if it isn't already running
  -h, --help               Print help
```
