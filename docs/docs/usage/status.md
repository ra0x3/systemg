---
sidebar_position: 5
title: status
---

Show the status of all currently running services.

```sh
$ sysg status
```

Show the status of a specific service.

```sh
$ sysg status --service webserver
```

```sh

$ sysg status
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