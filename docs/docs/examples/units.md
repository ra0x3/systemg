---
sidebar_position: 4
title: Units
---

# Units

Use `sysg start -- <command...>` to create and run a single managed unit without
writing a full project config.

When `--name` is omitted, systemg auto-generates a unit name.

## Common examples

Keep a lightweight HTTP server alive.

```bash
$ sysg start --daemonize -- python3 -m http.server 8080
```

Keep a one-off script running.

```bash
$ sysg start --daemonize -- ./scripts/dev-health-check.sh
```

Tail application logs under supervision.

```bash
$ sysg start --daemonize -- tail -F ./logs/app.log
```

Run a frontend dev server.

```bash
$ sysg start --daemonize -- npm run dev
```

Run a backend API in reload mode.

```bash
$ sysg start --daemonize -- uv run uvicorn app.main:app --host 0.0.0.0 --port 8000 --reload
```

Run a worker with explicit queue and concurrency settings.

```bash
$ sysg start --daemonize -- sh -lc 'QUEUE=critical CONCURRENCY=4 ./bin/worker'
```

Run a live TypeScript watcher and build loop.

```bash
$ sysg start --daemonize -- sh -lc 'pnpm install && pnpm run dev:watch'
```

Run a periodic heartbeat loop.

```bash
$ sysg start --daemonize -- sh -lc 'while true; do date; sleep 30; done'
```

Run a composed multi-step local pipeline.

```bash
$ sysg start --daemonize -- sh -lc 'pnpm db:migrate && pnpm run seed && pnpm run start:prod'
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
