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

When configuration files declare `depends_on` entries, `sysg start` bootstraps services in dependency order. If a prerequisite fails to start, its dependents are skipped and the command exits with a dependency error instead of leaving the system half-running.
