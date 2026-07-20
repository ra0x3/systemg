# Live-stack testing

**The live stack is the source of truth.** Prove a fix against the real projects
first; write the docker use case afterwards to lock it in.

This is not a staging approximation. It is **two real multi-config applications
put through their paces** — a 13-unit data pipeline with a deep dependency chain,
and a 5-unit web/worker stack with cron — run the way the user actually runs
them: real ports, real databases, real proxy env, real build times, real
terminals, started and stopped in every combination of foreground and daemon.

That is why it finds what containers cannot. Nearly every serious bug in this
project was found here and did **not** reproduce on a synthetic config:

| bug | why a toy config missed it |
|---|---|
| one-shots flipped `done`→`stopped` on restart | needs a deep `condition: completed` chain |
| health checks hung at attempt 3/5 | needs `.gamecast.env`'s real HTTP proxy vars |
| foreground start streamed 6 bytes and ignored Ctrl-C | needs a genuinely slow (~50s) boot |
| `start` reported "loaded" + exit 0 while 3 services failed | needs a *second* project attaching to a live supervisor, with a real failing dependency |
| terminal left stair-stepping diagonally | needs a real tty and a killed child |
| supervisor froze ~2h on a wedged pre-start | needs a pre-start that really blocks |

Container use cases are the regression net — they stop a fixed bug from coming
back. They are not the proof that it is fixed.

---

## The two projects

| | gamecast | arbitration |
|---|---|---|
| repo | `~/dev/repos/gamecast` | `~/dev/repos/arbitration` |
| config | `sysg.dev.yaml` | `sysg.dev.yaml` |
| project id | `gamecast-dev` | `arbitration-dev` |
| units | 13 | 5 |

**gamecast-dev** — `redis`, `postgres`, `gamecast_build`, `gamecast_migrations`,
`gamecast_agent`, `gamecast_observability_rollup`, `gamecast_api`,
`gamecast_ingest`, `gamecast_live_sweep`, `gamecast_reconcile`,
`gamecast_draftkings_ingest`, `gamecast_match`, `gamecast_draftkings_live_odds`

**arbitration-dev** — `arb_rs__dev`, `arb_rs__warm_data`, `arb_www__dev`,
`arb_py__ingest_curate_instagram`, `arb_py__ingest_curate_tiktok`

### Why these two are worth testing against

They are not interchangeable — each exercises something a toy config cannot:

- **gamecast has a deep dependency chain.** `postgres` → `gamecast_build` →
  `gamecast_migrations` (`condition: completed`) → the long-running services.
  Boot takes ~50s. This is what surfaced the one-shot `done`→`stopped`
  regression, which did **not** reproduce on a minimal config.
- **gamecast has probe-style services.** `redis: redis-cli ping` and
  `postgres: psql -c 'SELECT 1;'` exit 0 immediately and declare **no**
  `restart_policy`. Any "is this a one-shot?" logic keyed only on
  `restart_policy: never` misses them.
- **gamecast has a health check** on `gamecast_api`, with retries — the only
  place the `(attempt N/5)` progress detail appears.
- **gamecast's env sets HTTP proxies** (`.gamecast.env` / `DECODO_PROXY_*`).
  This is what caught health probes being proxied to death; a container has no
  such env.
- **arbitration boots fast and streams heavily** (`cargo watch`, Vite). Good for
  log-streaming and terminal-handling checks.
- **arbitration has cron units** (`arb_py__*`), which behave differently from
  services on restart and status.

---

## Ground rules

1. **Never leave services running.** Tear down when finished (see below).
2. **`pkill -x sysg` kills the user's supervisor.** It is unscoped. Never prefix
   a docker suite run with it — containers are isolated and need no host
   cleanup. This mistake killed a live supervisor mid-session.
3. **Ask before restarting the user's stack.** Bringing it back up is their call.
4. **`ps`/`pgrep -f` self-matches.** A `pgrep -f 'some command'` also matches the
   shell line running it, producing phantom survivors. Count PIDs from
   `sysg status --format json` instead, then `kill -0` them.

---

## Setup

Build and install the local binary over the one on `PATH`:

```sh
cargo build --release
./scripts/test-local.sh --no-build     # installs + adhoc-codesigns
sysg --version
```

`test-local.sh` codesigns because on macOS **overwriting a signed Mach-O breaks
its signature and the next run dies with SIGKILL (exit 137)**. Revert any time
with `./scripts/test-local.sh --revert`.

Re-install after **every** rebuild, or you are testing the old binary.

> **Never silence the installer.** From `0.56.0` onward, compatible patch
> releases request same-PID live re-execution and only repoint PATH after the
> resident supervisor reports the new version. An incompatible or unsafe
> handoff exits nonzero with `SG0501`-`SG0505` and leaves the old version active.
> The bootstrap from `0.55.x` to `0.56.0` still requires stopping the old
> supervisor first. Always read the result and confirm the new code is present:
>
> ```sh
> ./scripts/test-local.sh --no-build          # do not silence this
> sysg --version
> strings ~/.sysg/versions/*/sysg | grep -c SG0106   # some string only the new build has
> ```

---

## Teardown (run this when finished)

```sh
sysg stop --supervisor >/dev/null 2>&1; sleep 2
pkill -x sysg 2>/dev/null
pkill -f 'target/release/gamecast' 2>/dev/null
pkill -f 'cargo watch -C arb-rs' 2>/dev/null
pkill -f 'arb-www-app' 2>/dev/null
sleep 2

# verify clean
echo "sysg=$(pgrep -x sysg | wc -l | tr -d ' ')" \
     "gc=$(pgrep -f '/target/release/gamecast ' | wc -l | tr -d ' ')" \
     "arb=$(pgrep -f 'cargo watch -C arb-rs' | wc -l | tr -d ' ')"
```

All three must read `0`.

---

## The mode matrix

`scripts/live-matrix-test.sh` runs the whole thing. It covers four combinations,
because bugs hide in the asymmetries between them:

| | gamecast | arbitration |
|---|---|---|
| A | daemon | daemon |
| B | foreground | foreground |
| C | foreground | daemon |
| D | daemon | foreground |

Plus section E for status/logs edge conditions (no supervisor, empty JSON).

```sh
./scripts/live-matrix-test.sh
```

It tears down between sections and at the end.

### Why the matrix matters

Several real bugs appeared in **only one cell**:

- The foreground boot-window bug hit gamecast (slow boot, 1st fg project) but
  not arbitration (fast boot, 2nd fg project) — 6 bytes captured vs 131 KB.
- `sysg start` shows a progress spinner when it **forks** a supervisor, but the
  **attach** path returned "loaded" before the boot ran. You only see this by
  starting a second project while the first is up.

---

## Foreground testing needs a real PTY

A foreground start owns the terminal, so it cannot be driven by a plain pipe.

**`kill -INT <pid>` is NOT Ctrl-C.** A keyboard Ctrl-C makes the tty driver send
SIGINT to the whole foreground **process group**; signalling one pid does not
reproduce that and makes a correct teardown look broken. This produced a false
failure that cost real debugging time.

Use `scripts/fgpty.py`, which runs the start under a PTY and writes `0x03` into
the pty master when you touch its control file:

```sh
python3 scripts/fgpty.py "$(command -v sysg)" ~/dev/repos/gamecast/sysg.dev.yaml /tmp/fg.out /tmp/fg.pid 300 &
sleep 50
wc -c /tmp/fg.out                 # streaming?  (6 bytes == broken, ~1.6 MB == healthy)
: > /tmp/fg.pid.ctl               # a REAL terminal Ctrl-C
sleep 15
pgrep -f '/target/release/gamecast ' | wc -l   # must be 0
```

Also set a real window size when capturing under a PTY, or width-dependent
output (the progress spinner) misreads the terminal:

```python
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))
```

---

## What to check, and what "correct" looks like

### status

```sh
sysg status                                   # renders, no stair-stepping
sysg status -p gamecast-dev                   # excludes arbitration units
sysg status --format json | python3 -m json.tool   # always parses
sysg status -s no_such_unit --format json     # {"units": []}, NOT prose
```

**Exit code reflects health.** `rc=2` with a genuinely failed unit is *correct* —
do not assert `rc == 0`. Assert on content.

After a full gamecast boot, expect **overall healthy**, with `gamecast_build`,
`gamecast_migrations`, `postgres`, `redis` as `done`/`healthy` and
`gamecast_observability_rollup` as `skipped`/`idle`.

### logs

```sh
sysg logs -p gamecast-dev -s gamecast_api --lines 20 --no-follow
sysg logs -p arbitration-dev --lines 20 --no-follow
sysg logs --supervisor --lines 20 --no-follow
sysg logs                                     # refused: needs a target
```

Cross-project bleed is a bug: gamecast's logs must contain no `arb_rs` lines and
vice versa.

**Post-mortem is the important case.** With the supervisor *down*, from a
directory with **no manifest**, logs must still read from disk:

```sh
cd /tmp && sysg logs -p gamecast-dev --lines 5 --no-follow
```

Files live under `~/.local/share/systemg/logs/<project-id>/`. If this errors with
SG0203 telling you to "target the project by id with -p" — which is what you
just did — that is the circular-diagnostic regression.

### start (fork vs attach)

`sysg start` takes two very different paths, and only the live stack shows it:

```sh
# 1. no supervisor -> FORKS one, stays attached, shows boot progress
cd ~/dev/repos/gamecast && sysg start -c sysg.dev.yaml --daemonize
#    spinner shows: health check for 'gamecast_api' (attempt 1/5)

# 2. supervisor UP -> ATTACHES; must still wait and report the truth
cd ~/dev/repos/arbitration && sysg start -c sysg.dev.yaml --daemonize; echo "rc=$?"
```

Arbitration is the better test here because `arb_rs__dev`'s pre-start waits for a
**db-tunnel on 127.0.0.1:5433**. With the tunnel down it genuinely cannot start:

```sh
lsof -nP -iTCP:5433        # nothing listening == arb_rs__dev will fail
```

That is a *feature* for testing — a real, reproducible startup failure. Expect
`SG0106` naming `arb_rs__dev` and **rc=1**. A `Project 'arbitration-dev' loaded`
with **rc=0** while services are down is the regression.

### restart

```sh
sysg restart -p gamecast-dev -s gamecast_api   # pid changes, arbitration untouched
sysg restart -p gamecast-dev                   # must stay HEALTHY, not WARN
```

A `restart -p` that leaves one-shots as `stopped`/`warn` is the regression that
made completed builds read as **failed dependencies**.

### stop

```sh
sysg stop -p gamecast-dev
```

Then verify with the OS, not with sysg: no `gamecast` processes may survive, and
ports must be free. A stop reporting success while processes still hold ports is
the worst failure mode — the user moves on believing the port is free.

The supervisor stays **warm** after a project stops (that is intended).

### terminal state

After any interactive view — especially `sysg status` → `L` (logs) — and after
killing a follow, the terminal must be back in cooked mode:

```sh
stty -a | tr ' ' '\n' | grep -c '^-icanon$'    # 0 == cooked (good)
```

If output starts stair-stepping diagonally across the screen, raw mode leaked.

---

## Timings

Do not mistake slowness for a hang:

| | |
|---|---|
| gamecast full boot | ~50s |
| arbitration boot | ~15s |
| gamecast restart | ~60s |
| teardown settle | ~3s |

---

## After the live stack proves it

Write the container use case in `tests/docker/usecase/<name>/`
(`Dockerfile` + `run.sh` + config), then:

```sh
tests/docker/usecase/run_all.sh <name>     # one case
tests/docker/usecase/run_all.sh            # all of them (~4 min)
```

The `run.sh` header should record **what really broke on the live stack** and why
it matters — that context is the difference between a test someone maintains and
one they delete.

`unit_field` takes the JSON blob first: `unit_field "$SNAPSHOT" <unit> <field>`.
