# stale-daemon repro

Reproduces (and guards against) the bug where **`sysg restart` never upgrades
(or downgrades) the resident supervisor itself**.

## The bug

`sysg restart` is an IPC message to the running supervisor process. The daemon
never re-executes its binary, so after installing a new sysg the daemon keeps
running the previous build forever — only the CLI and the re-spawned child
services are current. A drifted CLI talking to a stale daemon is also a
mixed-version IPC conversation.

ef4304e added a recycle path (stop + fresh start with the installed binary),
but it only fires on IPC *schema* mismatch. Version drift with a compatible
schema — the common case — left the stale daemon in place silently.

## The fix

- New `ControlCommand::Version` / `ControlResponse::DaemonVersion` handshake.
- On a full `sysg restart --daemonize`, the CLI compares the daemon's version
  to its own. Any difference (upgrade **or** downgrade) triggers the existing
  recycle path, so the supervisor process is restarted onto the installed
  binary.
- Daemons too old to know `Version` reply with a deserialize error, which the
  existing protocol-mismatch detector classifies — they get recycled too.
- Scoped restarts (`-s`/`-p`) never recycle; they print a drift warning
  instead.

## Run it

```sh
# from the repo root (build context is the repo)
docker build -f tests/docker/repro/stale-daemon/Dockerfile -t sysg-stale-daemon .
docker run --rm sysg-stale-daemon
```

The image builds the current source twice as versions `0.0.1` (`sysg-old`) and
`0.0.2` (`sysg-new`), boots the daemon with the old binary, and restarts with
the new CLI.

Exit `0` = GREEN (fixed). Exit `1` = RED (reproduced / still broken).
