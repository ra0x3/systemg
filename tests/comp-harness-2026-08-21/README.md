# Comparative benchmark harness — 2026-08-21

Everything behind [**How systemg compares to other process managers**](https://sysg.dev/blog/2026-08-21/how-systemg-compares).
Every number in that post came from a script in this directory. If one looks
wrong, re-run it.

## What was compared

| | version | installed via |
|---|---|---|
| sysg | 0.64.4 / 0.65.0 / 0.66.0 | `curl -fsSL https://sh.sysg.dev \| sh` |
| Supervisor | 4.3.0 | `pip install supervisor`, and `apt install supervisor` |
| Docker Compose | v2.32.4 | `docker-ce` + `containerd.io` + `docker-compose-plugin` |
| systemd | 252 (bookworm) | ships with the distro |

Units: MB is decimal (bytes / 1,000,000). `Installed-Size` and
`smaps_rollup` report KiB and are converted at x1024 first.

Host: macOS (Darwin 25.2.0) arm64, 10 CPU, 32 GiB, Docker Desktop. Linux figures from
`debian:bookworm` (pinned at `sha256:813017f3d62be4b5891a7acca6a01bdcd4b8513daa81b1ab99d3a50385b26931`)
and `ubuntu:22.04`; Docker's own figures from inside the Docker Desktop VM via
`--privileged --pid=host`.

## Setup

Both images install prerequisites **outside** any timed window, so no benchmark
measures the speed of `apt`.

```sh
cd tests/comp-harness-2026-08-21
docker build -t sysg-bench -f Dockerfile.bench .   # debian + curl, python, procps
docker build -t u22-base   -f Dockerfile.u22   .   # ubuntu 22.04, stock dev box
```

## Reading rules

1. **Quote the comparison, not the superlative.** "14x smaller than a Docker
   Compose install" is measured. "Smaller than anything else" is false —
   systemd's package is smaller than sysg on Linux, and on a machine that
   already has Python, Supervisor's install is smaller still.
2. **Name the platform.** sysg is 12.0 MB on macOS, 19.5–23.6 MB on Linux. An
   unqualified number is wrong somewhere.
3. **Clean machine for metrics 1–3, warm steady state for 4–8.** Install cost
   is a provisioning question and containers and CI runners start empty; every
   tool is costed with the runtime it needs to function (Python is part of
   Supervisor, the engine is part of Compose). Everything after installation is
   a behaviour question, measured warm.
4. **Fetch-time dependencies are not runtime dependencies.** `curl` and CA
   certs disappear if you `scp` the binary; CPython and containerd do not.
   sysg still needs a POSIX shell, so the accurate phrasing is "no interpreter,
   no VM", not "no dependencies".
5. **Ties are results.** A tuned Supervisor matches sysg exactly on readiness.
6. **Losses stay in the file.** Removing an unflattering number makes the rest
   untrustworthy.

Both configurations are charted wherever a setting changes the answer:
`stopasgroup`/`killasgroup`, `startsecs`, and `exec:` vs `command:`. Comparing a
tuned tool against a default one produced two wrong answers during collection
before it was caught. Docker's `live-restore` is the one setting still uncharted
— see metric 7.

## Metric index

| # | metric | status | headline |
|---|---|---|---|
| 1 | Installed size | done | Compose 14x; systemd is **smaller** than sysg |
| 2 | Install payload | done | Ubuntu cold: 6.8 vs 48 vs 172 MB |
| 3 | Install time | partial | sysg 1.65 s vs Supervisor 21.72 s (3.7x on the Ubuntu baseline); Docker pending |
| 4 | Dependency-graph start | partial | sysg 6.91 s vs Compose 8.31 s; environments differ |
| 5 | Resource overhead | partial | Compose 25x sysg; `exec:` form changes the slope |
| 6 | Descendant containment | done | sysg 0, Compose 0, tuned Supervisor 2 |
| 7 | Crash durability | partial | Supervisor duplicates; Compose row is default config |
| 8 | Readiness | done | tuned Supervisor ties sysg |
| 9+ | see [not yet collected](#not-yet-collected) | planned | |

**Partial** means collected but short of the reviewed protocol, or missing rows
that need a VM. Never quote a partial section without its caveat.

Three different things get called "install": metric 1 is what lands **on disk**,
metric 2 is what crosses the **network**, metric 3 is **nothing → a running
supervised service**, and metric 4 is an already-installed tool **starting
services**. They are not interchangeable.

---

## 1. Installed size

Minimum to run the tool at all, on a machine with nothing preinstalled:

| Linux x86_64 | bytes | MB | vs sysg |
|---|---:|---:|---:|
| systemd (pkg + libsystemd0) | 14,567,424 | 14.57 | 0.75x |
| sysg | 19,500,064 | 19.50 | 1.00x |
| Supervisor + Python | 26,810,121 | 26.81 | 1.37x |
| Docker Compose stack | 278,634,496 | 278.63 | 14.29x |

| macOS arm64 | bytes | MB | vs sysg |
|---|---:|---:|---:|
| sysg | 12,007,624 | 12.01 | 1.00x |
| Docker Desktop | 2,241,007,616 | 2,241.01 | 186.6x |

- Against Compose the gap is decisive — 14x on Linux, 187x on macOS. That is
  the comparison the size claim should be made against.
- Against Supervisor sysg is 27% smaller. Both carry a whole runtime: sysg's is
  compiled in at ~12 MB, Supervisor's is 23.08 MB of CPython (86% of its total).
- Against systemd sysg is **larger**, and systemd's marginal cost is zero
  because it is already PID 1.
- Where sysg wins is shape, not bytes: no interpreter, no VM, no engine/daemon
  pair, rootless install, uninstall by deleting a directory.

**sysg** (v0.64.4 release tarballs, stripped):

| target | bytes | MB | download |
|---|---:|---:|---:|
| aarch64-apple-darwin | 12,007,624 | 12.01 | 4,498,386 |
| x86_64-apple-darwin | 12,682,820 | 12.68 | 4,757,080 |
| aarch64-unknown-linux-gnu | 17,663,816 | 17.66 | 6,736,617 |
| x86_64-unknown-linux-gnu | 19,500,064 | 19.50 | 7,002,935 |
| aarch64-linux-musl | 21,138,952 | 21.14 | 7,986,317 |
| x86_64-linux-musl | 23,605,680 | 23.61 | 8,018,251 |

Linux is 1.5–2.0x macOS. Prime suspect is vendored OpenSSL, linked on Linux
only — it is C, so Rust-level size tools do not see it. Unverified.

An unstripped local build is 14,421,592 B (macOS arm64), for reference.

**systemd**: 12,879,810 B measured (`dpkg -L`), 13,556,736 B by Installed-Size,
plus libsystemd0 at 1,010,688 B → 14.57 MB. PID 1 is 133,464 B of that;
`systemctl` is 1,387,544 B and `journalctl` 133,624 B. Marginal install cost on
a normal Linux distro is zero — it is already PID 1, and not separately
removable in any practical sense.

**Supervisor**: site-packages 3,731,209 B + Python runtime 23,078,912 B =
26,810,121 B. The runtime is 6.2x the size of the supervisor it exists to run.
*Correction, 2026-08-21:* an earlier revision put the Python subtotal at
15.68 MB, omitting `python3.11-minimal` (the interpreter binary itself), plus
`python3.11` and `libpython3-stdlib`. `dpkg-query -W -f='${Installed-Size}\t${Package}\n' | grep python3`
totals 22,538 KiB = 23.08 MB. The error understated Supervisor by 7.4 MB and
produced a false tie.

<details>
<summary>Python runtime, by package (Installed-Size)</summary>

```text
  libpython3.11-stdlib     10,112,000     10.11
  python3.11-minimal        6,710,272      6.71   <- interpreter binary
  libpython3.11-minimal     5,366,784      5.37
  python3.11                  665,600      0.67
  python3-minimal             113,664      0.11
  python3                      82,944      0.08
  libpython3-stdlib            27,648      0.03
  ===
  subtotal                 23,078,912     23.08
```
</details>

**Docker Compose**: the plugin alone is 64,694,701 B (x86_64) / 62,902,454 B
(aarch64) / 74,866,784 B (macOS), but it cannot run anything without an engine.
Minimum working stack on Debian: docker-ce 105,640,960 + docker-ce-cli
43,390,976 + containerd.io 83,117,056 + docker-compose-plugin 46,485,504 =
278,634,496 B. (docker-buildx-plugin, 68,129,792 B, installs by default but is
not required by Compose — excluded.) Docker Desktop on macOS is 2.24 GB, plus a
41,292,240 B CLI.

<details>
<summary>Reproduction</summary>

```sh
# sysg
gh release download v0.64.4 -R ra0x3/systemg -p 'sysg-0.64.4-*.tar.gz'
for f in sysg-*.tar.gz; do tar xzf "$f" -C "${f%.tar.gz}"; done
find . -name sysg -type f -exec stat -f %z {} \;

# systemd + Supervisor
docker run --rm debian:bookworm bash -c '
  apt-get update -qq && apt-get install -y -qq systemd python3 python3-venv
  dpkg-query -W -f="\${Installed-Size}\t\${Package}\n" systemd libsystemd0
  dpkg -L systemd | while read p; do [ -f "$p" ] && stat -c %s "$p"; done |
    awk "{s+=\$1} END {print s}"
  python3 -m venv /tmp/v && /tmp/v/bin/pip install -q supervisor
  du -sb /tmp/v/lib/python3*/site-packages/supervisor'

# Docker packages
docker run --rm debian:bookworm bash -c '
  apt-get update -qq && apt-get install -y -qq curl ca-certificates gnupg
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://download.docker.com/linux/debian/gpg \
    -o /etc/apt/keyrings/docker.asc && chmod a+r /etc/apt/keyrings/docker.asc
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] \
    https://download.docker.com/linux/debian bookworm stable" \
    > /etc/apt/sources.list.d/docker.list
  apt-get update -qq
  apt-cache --no-all-versions show docker-ce docker-ce-cli containerd.io \
    docker-compose-plugin | grep -E "^(Package|Installed-Size|Size):"'
```
</details>

---

## 2. Install payload

Two numbers, because they answer different questions: **published artifact
bytes** (what the vendor ships) and **measured wire bytes** (eth0 rx delta
across a real cold install — repo metadata, index refreshes, TLS and transitive
dependencies included). The second is what a machine actually pays.

**Measured wire bytes**, fresh container, cold:

| tool | median bytes | MB | trials |
|---|---:|---:|---:|
| sysg | 6,812,042 | 6.81 | 5 |
| Supervisor (Python + pkg) | 119,753,511 | 119.75 | 3 |
| Docker + Compose | 117,943,013 | 117.94 | 3 |

- Supervisor pays 17.6x sysg — and marginally more than Docker. Of that,
  0.34 MB is the supervisor package; the other 119.41 MB is CPython and the apt
  metadata needed to fetch it.
- Docker pays 17.3x sysg **before pulling a single image**. That figure is the
  apt transaction only: keyring and `sources.list` setup happened before the
  counter was baselined.
- Published .debs for Docker total 64.33 MB but a real install moves 117.94 MB.
  Quoting only the published figure understates it by 1.8x.
- sysg's artifact (7.00 MB) and wire (6.81 MB) figures nearly agree, because
  there is one artifact and no dependency resolution.

**Published artifacts**: sysg tarballs 4.50–8.02 MB (per target, table in
metric 1); Supervisor's PyPI wheel is 320,736 B, which is *not* a comparable
figure since it does not run without Python; Docker's .debs are
docker-ce 20,527,660 + cli 15,293,944 + containerd.io 19,099,456 +
compose-plugin 9,408,596 = 64,329,656 B (docker-buildx-plugin, 14,717,684 B,
installs by default but is not required by Compose — excluded). systemd has no
artifact to fetch.

### Realistic baseline — stock Ubuntu 22.04

Added to answer "your clean machine is contrived" by measuring it. Baseline:
`ubuntu:22.04` with python3 3.10.12, curl 7.81.0 and ca-certificates present,
installed outside the timed window. Docker is not present, because a stock
Ubuntu box does not have it. Each tool installed the way its users install it.

| tool | install | to_ready | rx bytes |
|---|---|---|---:|
| sysg | 1.22 / 1.50 s | 1.49 / 1.78 s | 6,841,975 |
| Supervisor | 4.45 / 4.58 s | 5.53 / 5.66 s | 47,952,570 |
| Compose | 15.85 / 16.69 s | (pkg only) | 172,504,339 |

Decomposed, because charging Supervisor for Ubuntu's package index would be
dishonest: apt index metadata is 47,552,321 B of that total, the supervisor
.debs are 414,613 B, and installed size grows by 2,226 KB (2.28 MB) on top of
the Python already there.

- **Cold apt cache** (fresh box, CI runner, new container): sysg wins
  decisively — 1.49 s / 6.84 MB against 5.53 s / 47.95 MB and 15.85 s / 172.50 MB.
- **Warm apt cache**: Supervisor's marginal payload is 0.41 MB, 17x *smaller*
  than sysg's, and its install time drops to roughly sysg's. On that machine, in
  that state, Supervisor is the lighter install. That is a real result.
- Compose loses in every cache state by an order of magnitude.
- sysg's durable advantage here is **predictability of payload**: 6.84 MB
  whatever the machine's state, where the apt path varies between 0.41 MB and
  48 MB depending on state you do not control. Install *time* still depends on
  the network for every tool; what it does not depend on is what is already
  installed.

```sh
# realistic baseline: stock Ubuntu 22.04
docker run --rm -v "$PWD/u22bench.sh":/b.sh u22-base bash /b.sh sysg
docker run --rm -v "$PWD/u22bench.sh":/b.sh u22-base bash /b.sh supervisor
docker run --rm -v "$PWD/u22bench.sh":/b.sh u22-base bash /b.sh compose

# clean machine
docker run --rm -v "$PWD/bench.sh":/b.sh sysg-bench bash /b.sh sysg
docker run --rm -v "$PWD/bench.sh":/b.sh sysg-bench bash /b.sh supervisor        # pip step alone
docker run --rm -v "$PWD/bench2.sh":/b.sh debian:bookworm bash /b.sh supervisor-nopy
docker run --rm -v "$PWD/bench2.sh":/b.sh debian:bookworm bash /b.sh docker-pkg

# published artifact bytes
gh release view v0.64.4 -R ra0x3/systemg --json assets -q '.assets[] | "\(.size)  \(.name)"'
apt-cache --no-all-versions show docker-ce docker-ce-cli containerd.io \
  docker-compose-plugin | grep -E "^(Package|Size):"
pip download supervisor -d /tmp/w && ls -l /tmp/w
```

Wire bytes are an `rx_bytes` delta read either side of the timed window:
`cat /sys/class/net/eth0/statistics/rx_bytes`.

Gaps: cold only — warm rows outstanding. No image-pull row for Docker.
`rx_bytes` counts the container's interface, so Docker Desktop's NAT layer is
excluded (fine for relative comparison, not as an absolute). sysg's measured row
is the aarch64 tarball.

---

## 3. Install time

`T_pkg` = install command returns. `T_ready` = from start of install until the
tool reports a trivial service (`sleep 600`) running **via its own status
command**; this is the primary metric, and config authoring is excluded.

| tool | T_pkg | T_ready | rx (MB) |
|---|---|---|---:|
| sysg | 1.36 [1.22–3.15] | 1.65 [1.51–3.43] | 6.81 |
| Supervisor | 20.60 [18.6–31.6] | 21.72 [19.7–32.8] | 119.75 |
| Docker + Compose | 9.61 [9.34–14.7] | pending (VM) | 117.94 |

<details>
<summary>Raw trials (seconds; rx in bytes)</summary>

```text
  tool                T_pkg   T_cli  T_ready  svc_up            rx
  sysg                 1.33    0.00     1.61     yes     6,811,746
  sysg                 3.15    0.00     3.43     yes     6,860,851
  sysg                 1.40    0.01     1.69     yes     6,812,176
  sysg                 1.22    0.00     1.51     yes     6,813,723
  sysg                 1.36    0.00     1.65     yes     6,812,042

  supervisor          18.55    0.04    19.74      --   119,744,067
  (Python + pkg)      20.60    0.04    21.72      --   119,753,511
                      31.63    0.04    32.84      --   119,878,429

  pip step alone, on an image that already had Python -- a breakdown,
  NOT a comparison row:
                       0.49    0.04     1.62     yes       342,898
                       0.57    0.04     1.69     yes       342,458
                       0.56    0.04     1.73     yes       342,260
                       0.50    0.04     1.62     yes       343,363
                       0.52    0.03     1.73     yes       343,229

  docker+compose       9.34    0.02      n/a     n/a   117,903,979
  (dpkg only)          9.61    0.02      n/a     n/a   117,943,013
                      14.69    0.02      n/a     n/a   117,984,563
```

`svc_up` reads `--` for Supervisor because `pgrep` is absent from a bare
`debian:bookworm`; readiness was confirmed by `supervisorctl` reporting RUNNING.
</details>

- sysg reaches a supervised service in 1.65 s from nothing, with no package
  manager and no root — 13.2x faster than Supervisor, moving 17.6x fewer bytes.
- The pip step alone (0.52 s) is faster than sysg's install, but it presupposes
  a runtime already on the machine, so it is not a comparison.
- Docker's package transaction alone is ~6x sysg's entire time-to-ready, before
  the daemon starts or an image is pulled. Its true `T_ready` will be larger.
- Both outliers (sysg 3.15 s, Supervisor 31.63 s) are single-trial network
  variance. Reported, not discarded.

Trial counts are 5 / 3 / 3 against a reviewed protocol asking for 10 cold +
10 cached, so treat the medians as indicative. Container writes land on
overlayfs. Docker and systemd `T_ready` are absent by design: the daemon cannot
start in a plain container and systemd is not PID 1 there — both need the VM,
along with Docker Desktop's macOS install (interactive, so label it "not
benchmarked", never zero) and warm-cache rows for everything.

---

## 4. Dependency-graph start

A fixed 10-service DAG expressed identically in every tool. The **services** are
instrumented, not the supervisors: each runs the same body — `sleep 0.3`, write
`date +%s.%N` to a shared dir, hold — so per-service times are byte-identical
across tools instead of parsed out of four log formats.

```text
db    <- (none)        queue     <- (none)
cache <- db            ingest1/2 <- queue
worker1/2/3 <- cache
aggregator  <- worker1, worker2, worker3, ingest1, ingest2
reporter    <- aggregator
```

Five levels deep, two independent roots, a 5-wide fan-in. Ideal parallel is
5 x 0.3 = 1.5 s; fully serial is 10 x 0.3 = 3.0 s. The shape exposes one thing:
does the tool start independent branches in parallel?

| tool | total (3 trials) | median |
|---|---|---|
| sysg v0.65.0 | 6.901 / 6.912 / 6.910 | **6.91 s** |
| Docker Compose | 8.407 / 8.312 / 8.295 | 8.31 s |
| sysg v0.64.4 (superseded) | 13.839 / 13.827 / 13.782 | 13.83 s |
| Supervisor | cannot express this graph | — |
| systemd | pending (needs PID 1) | — |

<details>
<summary>Per-service times (seconds from command start)</summary>

```text
  sysg v0.65.0 -- total 6.91
     db 0.312, queue 0.309                       <- level 1, together
     cache 1.687, ingest1 1.687, ingest2 1.687   <- level 2, together
     worker1 3.067, worker2 3.066, worker3 3.066 <- level 3, together
     aggregator 4.449                            <- level 4
     reporter 5.819                              <- level 5

  Docker Compose -- total 8.31
     db 0.788, queue 0.790                       <- level 1, together
     cache 2.411, ingest1 2.411, ingest2 2.411   <- level 2, together
     worker1 3.990, worker2 3.997, worker3 3.995 <- level 3, together
     aggregator 5.593                            <- level 4
     reporter 7.158                              <- level 5

  sysg v0.64.4 -- total 13.83, strictly serial, ~1.38 s apart
     db 0.310 | cache 1.699 | queue 3.070 | ingest1 4.451 | ingest2 5.835
     worker1 7.228 | worker2 8.613 | worker3 10.005 | aggregator 11.382
     reporter 12.765
```
</details>

sysg v0.65.0 and Compose both complete in five clean level-waves — both roots
together, all three workers together. sysg pays ~1.38 s per level (0.3 s init +
~1.08 s health poll), Compose ~1.58 s, because each Compose unit is a container
create+start where sysg's is a process. Same shape, cheaper units.

- Supervisor's inability is a **result**, not a caveat: `priority` is start
  order only, with no dependency edges and no health gating, so "start B after A
  is healthy" is inexpressible. A priority-ordered approximation would measure
  something else and is not reported.
- v0.64.4 started services strictly one at a time — 10 units ~1.38 s apart. Not
  a dependency bug: a control run of 10 *flat, independent* services also
  started 0.27 s apart, and two code sites walked their order sequentially
  (`daemon.rs:5296`, `supervisor.rs:626`). Fixed in v0.65.0; the row is kept for
  the before/after record.
- Controls: same DAG without health checks → 2.752 s; ten flat services → 2.761 s.
- sysg's floor is 1 s: its duration parser (`daemon.rs:7659`) accepts only whole
  seconds, minutes or hours, so `interval: "100ms"` is rejected and health
  polling dominates per-level cost.

```sh
docker run --rm -v "$PWD":/svc sysg-bench bash /svc/run-sysg.sh
docker compose -p dagbench -f docker-compose.yml up -d --wait
docker run --rm -v "$PWD":/svc sysg-bench bash /svc/ctl.sh   # controls
```

`dag.json` is canonical; `systemg.yaml` and `docker-compose.yml` are generated
from it. Compose ran with images pre-pulled (`debian:bookworm-slim`),
`condition: service_healthy`, and a 1 s healthcheck interval to match sysg's floor.

**The largest threat to these numbers:** sysg was measured inside a container
while Compose ran against the host daemon. Those are not the same environment
and the 1.20x ratio inherits the difference — treat it as indicative until both
run in one Linux VM. The serialization finding does not depend on it; that is
confirmed in source and by the flat-10 control. Also outstanding: per-service
stamps conflate scheduling, spawn and init (an `entry_ns` stamp would separate
them); stamps use wall clock via `date +%s.%N`, where CLOCK_MONOTONIC with
atomic writes onto a bind-mounted tmpfs would remove clock-skew,
mtime-resolution and overlayfs concerns; command-return time is not recorded
separately from all-ready; 3 trials,
one host; Compose's per-service time includes container create and is not netted
out; Compose services have their own PID namespaces and sysg's do not.

---

## 5. Resource overhead

`RU(T, N) = resources(T supervising N services) - resources(the same N services
run bare)`. Services are near-zero cost so the tool's tax dominates. The
baseline is a one-shot launcher that forks N services with `setsid` and exits,
so the zero point contains no supervisor at all.

Memory is **PSS** from `/proc/*/smaps_rollup`, not RSS: RSS double-counts shared
pages, which would flatter a forked supervisor and penalise Python. PSS is
reported in KiB; MB figures are KiB x 1024 / 1e6.

### v0.66.0 and the `exec:` form

v0.66.0 adds an opt-in argv form — `exec: ["/usr/bin/myapp", "--port", "8080"]`
— that runs the program directly instead of through a shell. All four rows below
were re-measured together with the identical service body, so this table is
internally consistent:

| N | bare | sysg `exec:` | sysg `command:` | Supervisor |
|---:|---:|---:|---:|---:|
| 1 | 0.52 MB | 12.22 | 12.33 | 17.88 |
| 10 | 1.42 MB | 12.44 | 13.55 | 18.07 |
| 40 | 4.33 MB | 13.57 | 17.99 | 18.67 |

| fit | intercept | slope |
|---|---:|---:|
| sysg `exec:` | 12.18 MB | 0.0346 MB/service |
| sysg `command:` | 12.18 MB | 0.1451 MB/service |
| Supervisor | 17.86 MB | 0.0202 MB/service |

- The wrapper shell is gone under `exec:`. Processes per service drop from 2 to
  1, matching Supervisor exactly, and the slope falls 4.2x. At N=40: 45
  processes, against 85 under `command:`.
- Crossover with Supervisor moves from N≈46 to N≈395 — out of the range a single
  host realistically hits. Below it sysg is lighter at every N: 1.45x at N=10,
  1.38x at N=40.
- The residual 0.0346 MB/service is sysg's own bookkeeping (pid, status, health
  config, restart state, log buffers). Corroborated independently: the
  supervisor process grows ~34 KiB/service under both forms.
- **This is opt-in.** A manifest using `command:` pays the wrapper exactly as
  before. Quote it as "with the exec form", never as "sysg fixed it".

### Earlier tables, including Compose

Measured against a *different* service body, so not directly comparable to the
table above — kept because they carry the only Compose rows:

| N | bare PSS (KiB) | procs | sysg PSS | procs | Supervisor PSS | procs |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 1,063 | 5 | 13,834 | 7 | 18,755 | 6 |
| 10 | 2,291 | 14 | 16,126 | 25 | 20,091 | 15 |
| 40 | 5,086 | 44 | 22,444 | 85 | 23,391 | 45 |

| N | sysg | Supervisor | Docker Compose |
|---:|---:|---:|---:|
| 1 | 13.08 MB | 18.12 MB | ~289 MB (engine + 1 shim) |
| 10 | 14.17 MB | 18.23 MB | ~354 MB (engine + 10 shims) |
| 40 | 17.77 MB | 18.74 MB | ~572 MB (extrapolated) |

Docker's components, measured in-VM: dockerd 174.6 MB idle (177.5 with 10 containers), containerd 91.8 MB,
one idle shim 15.5 MB — an engine subtotal of 281.9 MB paid before you run
anything, plus 7.3 MB per container (measured: 10 shims = 72.6 MB). Process
count goes 188 → 209 for ten services. On macOS add Docker Desktop's host-side
476 MB (com.docker.backend 230.3, UI 166.1, virtualization 38.8, build 36.0),
which is the envelope the in-VM figures sit *inside* — do not add the two.

- Compose's overhead is ~25x sysg at N=10 and loses on both terms: a 282 MB
  engine before anything runs, and a per-container slope 60x sysg's.
- But Compose is not only supervising. Separate PID namespaces, network
  namespaces and filesystems are more **isolation**, not merely more overhead.
  Subtracting a bare-process baseline measures containerised composition cost.
- **sysg is not zero-overhead.** The measurement says a 12.96 MB resident
  intercept — the binary itself living in memory. Small and flat against a
  282 MB engine, but not zero.
- Those tables fit to `sysg 12.96 MB + 0.120 MB/service`, `Supervisor
  18.10 MB + 0.016 MB/service`, `Compose 281.9 MB + 7.256 MB/service`, putting
  the sysg/Supervisor crossover at N≈49. The cause of sysg's slope was the
  wrapper shell, and removing it (the `exec:` form above) is what moved that
  crossover to N≈395.
- Supervisor's 18.10 MB looked low for "an entire Python runtime" and was
  re-checked per process (supervisord PSS 17,644 kB, RSS 18,564 kB; sysg
  12,562 kB). It is correct: CPython
  mmaps libpython and imports only what supervisord uses; most of the stdlib is
  `.py` files never read into memory. sysg is structurally the same thing — a
  static binary carrying its runtime, compiled in.

<details>
<summary>Where the old 0.120 MB/service slope went (harness: split.sh; PSS in KiB)</summary>

```text
  component                 N=1      N=10    marginal/service
  sysg supervisor process  12,468   12,699      ~26 KiB
  wrapper shells              471    1,703     ~137 KiB
  leaf processes              304    1,337     ~115 KiB (bare pays this too)
```

N=40 was attempted for this decomposition and timed out at 10 minutes. ~84% of
the slope was the redundant wrapper; only ~26 KiB was real per-service
bookkeeping. PSS per identical process falls as N rises (more `sleep` processes
share more pages), so the components do not sum exactly to the fitted slope —
directionally right, not arithmetically exact. Under `exec:` the supervisor
process still grows ~34 KiB/service (11,922→13,275 KiB `command:`,
11,926→13,252 KiB `exec:`), which is the residual slope in the table above.
</details>

```sh
docker run --rm -v "$PWD":/h sysg-bench bash /h/bare2.sh 10      # baseline
docker run --rm -v "$PWD":/h sysg-bench bash /h/split2.sh 10 exec
docker run --rm -v "$PWD":/h sysg-bench bash /h/split2.sh 10 command
docker run --rm -v "$PWD":/h sysg-bench bash /h/sup2.sh 10
```

Gaps: Compose's **composer-only** tax (Compose vs the same containers started
with plain `docker run`) is not measured, and is expected to be near zero at
rest since the CLI exits after `up -d` — publishing only the stack tax overstates
Compose the tool while correctly stating Compose the stack. The engine was
measured warm and shared. No cgroup-v2 accounting, so kernel-side memory is
uncounted for every tool, which undercounts Docker most. systemd is not measured
and its marginal cost is not automatically zero. One trial per N, 3 s settle,
no confidence intervals. An earlier revision of this section read the
`smaps_rollup` values as kB rather than KiB and understated every memory figure
by 2.4%.

---

## 6. Descendant containment

Does stopping a service actually stop it? A service forks three shapes of child
at once — a plain background child, a grandchild under a shell, and a
double-forked child in its own session (`setsid`, which deliberately escapes the
process group). Stop by the tool's own documented command, then count survivors.

| tool | procs before | surviving after stop |
|---|---:|---:|
| sysg | 6 | **0** |
| Docker Compose | 6 | **0** |
| Supervisor, tuned (`stopasgroup`, `killasgroup`) | 6 | 2 |
| Supervisor, default | 6 | 5 |

- Quote this as "0 vs 2 against a tuned Supervisor", never "0 vs 5". The tuned
  row was added after review found the first version compared a configured sysg
  against a default Supervisor.
- Supervisor's survivors all reparent to PID 1:

  ```text
    80  1  sleep 3600
    81  1  sh -c sleep 3600
    82  1  sh -c sleep 3600
    83 81  sleep 3600
    84 82  sleep 3600
  ```

- Supervisor's remaining 2 under tuning are the `setsid` double-fork and its child. This is a
  ceiling, not a tuning oversight: `killasgroup` signals the process *group*, and
  an escaped session is not in it. Closing it needs session or cgroup teardown,
  which Supervisor does not have.
- The two zeros are not the same achievement. Compose gets it from the kernel —
  stopping a container tears down the PID namespace, so nothing inside survives
  however it forked. sysg has no namespace to lean on and does it deliberately,
  via session and provenance-based teardown.
- sysg cleaned up the `setsid` double-fork — the shape review predicted it would
  miss.
- Anything above 0 means the tool reports a state that is not true: the orphan
  still holds the port on restart, an orphaned worker keeps consuming the same
  queue, and the loss is per stop/restart *cycle*, so an hourly-restarting
  service accumulates orphans until the box runs out.

```sh
docker run --rm -v "$PWD":/h sysg-bench bash /h/orph.sh sysg
docker run --rm -v "$PWD":/h sysg-bench bash /h/orph.sh supervisor default
docker run --rm -v "$PWD":/h sysg-bench bash /h/orph.sh supervisor group
```

Gaps: no TERM-grace/KILL-deadline sweep (survivors should be counted after each).
One trial — the result is categorical, so repetition matters less here.

---

## 7. Control-plane crash durability

`kill -9` the supervisor itself, then restart it: do the services survive, is
the survivor re-adopted or **duplicated**, and is the reported status truthful
afterwards? Duplication is a hard failure — two copies competing for one resource.

| | sysg | Supervisor | Compose (default) |
|---|---|---|---|
| workload survives CP death | yes | yes | no |
| workload observable during | n/a | n/a | **no** |
| duplicate started on recovery | no | **yes** | no |
| workload lost on recovery | no | no | **yes** |
| automatic recovery | yes | yes | no (manual x2) |

- **sysg**: services reparent to init and are re-adopted on restart with
  unchanged pids (77, 79), with or without a health check. An earlier run
  appeared to show a pid change (75 → 138) with three matching processes; that
  was a harness fault — `pgrep -f` matched the measuring shell's own command
  line, and a concurrent `sysg logs` added the third. Recorded because the wrong reading
  would have been a false negative against sysg.
- **Supervisor**: the service survives, but supervisord has no record of it and
  starts a second copy.
- **Compose**: measured with `live-restore = false` (Docker's default) and
  restart policy `no`, by SIGKILL to dockerd (pid 300) inside the Docker
  Desktop VM. dockerd did not self-recover after 200 s; every container
  came back `exited`, needing a manual app relaunch *and* a manual
  `docker start`. While dockerd was down the workload could not even be
  inspected — every observation path goes through the daemon that was killed.
  Five unrelated containers on the machine were terminated too; that is the
  blast radius under these defaults.

Supervisor and Compose fail in opposite ways: Supervisor keeps the workload and
loses track of it; Compose never duplicates because it kills everything instead.

```sh
docker run --rm -v "$PWD":/h sysg-bench bash /h/crash.sh sysg
docker run --rm -v "$PWD":/h sysg-bench bash /h/crash.sh supervisor
docker run --rm -v "$PWD":/h sysg-bench bash /h/hc.sh hc     # health-check variant
```

> The Docker row came from `kill -9` on dockerd inside the Docker Desktop VM.
> **That stops every container on the machine** when `live-restore` is false,
> which is the default. Do not run it on a box you care about.

**`live-restore: true` is untested and is the largest hole in this dataset.**
Docker-in-Docker is not a valid harness for it — `dockerd` is the DinD
container's main process, so killing it kills the container and measures
nothing. Until it is measured on a real host, the Compose row must be quoted as
"in Docker's default configuration". Also unverified: log-pipe reattachment and
resumption of health probes after cold adoption; a service can be re-adopted by
pid yet have lost its log stream. One trial per tool per configuration. Note also that sysg's tracked pid is its
wrapper shell rather than the service itself, so adoption logic keyed on it
inherits the wrapper problem from metric 5.

---

## 8. Readiness semantics

A service is started but not usable for 5 seconds. What does the tool say during
those 5 seconds? **Lie window** = (time actually usable) − (time reported up).
Positive means the tool claimed readiness it did not have.

| tool | reports up | usable | lie window |
|---|---:|---:|---|
| sysg (health probe) | 5.64 s | 5.64 s | **0.00 s** |
| Docker Compose (healthcheck) | 5.73 s | 5.21 s | −0.52 s (conservative) |
| Supervisor `startsecs=1` (default) | 1.22 s | 5.10 s | **+3.88 s** |
| Supervisor `startsecs=5` (tuned) | 5.22 s | 5.22 s | **+0.00 s** — ties sysg |
| Supervisor `startsecs=5`, service takes 8 s | 5.19 s | 8.12 s | +2.93 s |

- The real difference is **open-loop vs closed-loop**, not accuracy.
  `startsecs` is a fixed timer: set it to the true startup time and Supervisor
  is exactly as accurate as sysg. The moment startup *varies* — cold cache,
  contended DB, slower disk — the timer is wrong by the variance.
- So the honest claim is not "Supervisor lies". It is that Supervisor can be
  accurate for a predictable service and cannot be for a variable one. RUNNING
  is an accurate liveness report; it is just not a readiness report.
- Compose is **conservative rather than accurate**: it declared ready 0.52 s after
  the service was usable because its healthcheck polls on a 1 s interval. Erring
  late costs time; erring early costs an outage.

**A real finding about sysg's defaults.** The first attempt failed the service
outright with [`SG0104`](https://sysg.dev/reference/dialog/codes#sg0104),
"service `w` failed to become healthy". sysg's
default health-check budget is 3 attempts; at a 1 s interval, anything needing
more than ~3 s is torn down at boot unless `retries`/`total_timeout` are raised.
The 5.64 s figure required `retries: 30, total_timeout: "60s"`. Fail-closed is
defensible and the diagnostic is good — it is also why sysg cannot lie here, it
would rather kill a service than report health on faith — but the default is
tight for JVMs, migrations, or anything touching a network.

```sh
docker run --rm -v "$PWD":/h sysg-bench bash /h/ready2.sh sysg
docker run --rm -v "$PWD":/h sysg-bench bash /h/ss.sh 5 1    # startsecs default
docker run --rm -v "$PWD":/h sysg-bench bash /h/ss.sh 5 5    # tuned to startup
docker run --rm -v "$PWD":/h sysg-bench bash /h/ss.sh 8 5    # startup varies
```

Gaps: one trial per tool; Compose's −0.52 s sits inside its own poll interval.
All tools were given a 1 s interval, which is sysg's floor — Compose can poll
faster. Only one readiness shape (a file appearing); TCP or HTTP probes may
differ, though Supervisor has no probe mechanism and would score the same on any.

---

## Harness bugs found during collection

All four produced plausible, wrong numbers before being caught.

1. **supervisord silently fails to start if a command contains `%`** — its
   config parser interpolates. The service body used `date +%s.%N`, so the first
   memory run reported 0.73 MB at every N. Escape as `%%`.
2. **A readiness poll that times out is not a readiness time.** The first
   Supervisor readiness run reported 15.4–17.0 s because `supervisorctl` cannot
   connect without a `[unix_http_server]` section, so the poll ran to its 10 s
   timeout while the service was already up. Real figure: 1.62–1.73 s. Cross-check
   with an independent signal before believing any number.
3. **busybox `date` has no `%N`.** The first Compose graph run produced
   unparseable timestamps; `debian:bookworm-slim` was substituted so the
   instrumentation matches the other tools byte for byte.
4. **`pgrep -f <pattern>` also matches the measuring shell.** This produced a
   false "sysg double-started after a crash" reading.

## Not yet collected

| metric | definition |
|---|---|
| Boot / cold start | installed tool, config on disk → all services healthy, at N = 1, 5, 40; first boot vs restart, and whether images were pre-pulled |
| Idle resident memory | supervisor plus every helper it keeps alive, at rest |
| Process count at rest | sysg should win outright — measure it rather than assert it |
| Idle CPU | mean CPU% over 60 s supervising 10 services; watch for polling loops |
| Uninstall | time to remove, plus a diff of files, units and sockets left behind (including the shell-rc PATH line sysg appends) |
| Config size | bytes and lines for one identical 5-service stack, all four configs published verbatim |
| Runtime dependency count | what must already exist on the box |
| Time to first diagnosis | broken service → cause on screen: bad binary path, port bound, permission denied, OOM |
| Restart correctness under flapping | does the tool honour its own backoff and give-up policy when a service crashes repeatedly |
| Dependency failure containment | a failed dependency's blast radius, and whether the tool self-heals when it recovers |

Also outstanding: `live-restore: true` (metric 7), which is the largest single
hole; a VM run for every systemd row and for Docker's `T_ready`; warm-cache
install rows; and 10-cold/10-cached trial counts throughout.

## Open items

- Linux sysg is 1.5–2.0x macOS; vendored OpenSSL is the hypothesis, not
  confirmed. Test: build the musl target with rustls instead and re-measure.
- apt's `Installed-Size` is the packager's own figure. For systemd it was
  cross-checked against a direct sum of `dpkg -L` files (13.56 vs 12.88 MB,
  agreeing within ~5%); other packages were not.
- Docker Engine figures are the Debian packages; other distros differ.
- Docker Desktop's 2.24 GB includes a Linux VM image and an Electron UI. It is
  the honest number for a macOS developer, not a like-for-like comparison
  against a single Linux binary.
- Supervisor is compared in its own venv; the distro package would share more
  with the system Python.
- The project README claims "Rootless: ~12 MB executable". That is the macOS
  figure — Linux is 17.7–23.6 MB. Needs correcting.
