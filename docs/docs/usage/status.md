---
sidebar_position: 5
title: status
---

Show the status of all currently running services.

```sh
$ sysg status
```

```
$ sysg status --help
Show the status of currently running services

Usage: sysg status [OPTIONS]

Options:
      --log-level <LEVEL>  Override the logging verbosity for this invocation only
  -s, --service <SERVICE>  Optionally specify a service name to check its status
  -h, --help               Print help
```

```sh

$ sysg status -s arb-rs

Active services:
● - arb-rs Running
   Active: active (running) since Tue 2025-11-04 11:30:52 UTC; Unknown
 Main PID: 138246
    Tasks: 1 (limit: N/A)
   Memory: 1.6M
      CPU: 0.000s
 Process Group: 138246
     |-138246 sh -c ./target/release/arb-rs -c config.toml
       ├─138253 ./target/release/arb-rs -c config.toml

           ├─138254
           ├─138255
```
