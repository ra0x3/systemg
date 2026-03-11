# Units Examples

This folder documents useful command-based units you can stage with `sysg start`
without maintaining a full project config file.

When `--name` is omitted, systemg auto-generates a unit name.

## Quick examples

Keep a lightweight HTTP server alive.

```bash
sysg start --daemonize -- python3 -m http.server 8080
```

Keep a one-off script running.

```bash
sysg start --daemonize -- ./scripts/dev-health-check.sh
```

Tail application logs under supervision.

```bash
sysg start --daemonize -- tail -F ./logs/app.log
```

Run a frontend dev server.

```bash
sysg start --daemonize -- npm run dev
```

Run a backend API in reload mode.

```bash
sysg start --daemonize -- uv run uvicorn app.main:app --host 0.0.0.0 --port 8000 --reload
```

Run a worker with explicit queue and concurrency settings.

```bash
sysg start --daemonize -- sh -lc 'QUEUE=critical CONCURRENCY=4 ./bin/worker'
```

Run a live TypeScript watcher and build loop.

```bash
sysg start --daemonize -- sh -lc 'pnpm install && pnpm run dev:watch'
```

Run a periodic heartbeat loop.

```bash
sysg start --daemonize -- sh -lc 'while true; do date; sleep 30; done'
```

Run a composed multi-step local pipeline.

```bash
sysg start --daemonize -- sh -lc 'pnpm db:migrate && pnpm run seed && pnpm run start:prod'
```

## Unit files

Generated YAML files are stored in:

```bash
~/.local/share/systemg/units/
```

## Restart behavior

If the supervisor is already running, starting a new unit command stages a YAML
file and prints an explicit restart command. Restart is never implicit.

Example:

```bash
sysg restart --daemonize --config ~/.local/share/systemg/units/<unit>.yaml
```
