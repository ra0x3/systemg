---
sidebar_position: 4
title: Units
---

# Units

Use `sysg start -- <command...>` to create and run a single managed unit without
writing a full project config.

## Common examples

```bash
# Keep a lightweight HTTP server alive
$ sysg start --daemonize --name docs-server -- python3 -m http.server 8080

# Tail a log file under supervision
$ sysg start --daemonize --name api-tail -- tail -F /var/log/api.log

# Run a periodic shell loop
$ sysg start --daemonize --name heartbeat -- sh -lc 'while true; do date; sleep 30; done'
```

## Where unit files are stored

Generated unit configs are saved under:

```bash
~/.local/share/systemg/units/*.yaml
```

systemg automatically prunes this folder to prevent unbounded growth.

## Applying a newly staged unit

If a supervisor is already running, `sysg start --daemonize -- <command...>` stages
the unit YAML and prints the explicit restart command. This keeps restart decisions
under user control.

```bash
$ sysg restart --daemonize --config ~/.local/share/systemg/units/<unit>.yaml
```

## Inspect and stop

```bash
$ sysg status
$ sysg logs --service <unit-name>
$ sysg stop --service <unit-name>
```
