# Comparative benchmark harness — 2026-08-21

Everything behind [**How systemg compares to other process managers**](https://sysg.dev/blog/2026-08-21/how-systemg-compares):
the scripts, the manifests, the raw output, and the methodology.

Nothing in that post came from a command that is not in this directory. If a
number looks wrong, the fastest way to prove it is to re-run the script that
produced it.

Re-running steps are below; the **full results** — every trial, every caveat,
and the decisions behind each metric — follow after them.

---

## What was compared

| | version | how it was installed |
|---|---|---|
| sysg | 0.64.4 / 0.65.0 / 0.66.0 | `curl -fsSL https://sh.sysg.dev \| sh` |
| Supervisor | 4.3.0 | `pip install supervisor` and `apt install supervisor` |
| Docker Compose | v2.32.4 | `docker-ce` + `containerd.io` + `docker-compose-plugin` |
| systemd | 252 (bookworm) | ships with the distribution |

Host: macOS 15.2 arm64, 10 CPU, 32 GiB, Docker Desktop.
Linux figures from `debian:bookworm` and `ubuntu:22.04` containers; Docker's own
figures from inside the Docker Desktop VM via `--privileged --pid=host`.

---

## Setup

Two base images. Both install prerequisites **outside** any timed window, so no
benchmark measures the speed of `apt`.

```sh
cd tests/comp-harness-2026-08-21
docker build -t sysg-bench -f Dockerfile.bench .   # debian + curl, python, procps
docker build -t u22-base   -f Dockerfile.u22   .   # ubuntu 22.04, stock dev box
```

`debian:bookworm` is pinned at
`sha256:813017f3d62be4b5891a7acca6a01bdcd4b8513daa81b1ab99d3a50385b26931`.

---

## Re-running each metric

Every script prints one CSV-ish line per run. Nothing needs a checkout of this
repo beyond this directory.

### Install time and payload

```sh
# realistic baseline: stock Ubuntu 22.04, python3 + curl already present
docker run --rm -v "$PWD/u22bench.sh":/b.sh u22-base bash /b.sh sysg
docker run --rm -v "$PWD/u22bench.sh":/b.sh u22-base bash /b.sh supervisor
docker run --rm -v "$PWD/u22bench.sh":/b.sh u22-base bash /b.sh compose

# clean-machine variant
docker run --rm -v "$PWD/bench.sh":/b.sh sysg-bench bash /b.sh sysg
docker run --rm -v "$PWD/bench2.sh":/b.sh debian:bookworm bash /b.sh supervisor-nopy
```

### Dependency-graph start

`dag.json` is the canonical 10-service graph; `systemg.yaml` and
`docker-compose.yml` are both generated from it, so the shape is identical.

```sh
docker run --rm -v "$PWD":/svc sysg-bench bash /svc/run-sysg.sh
docker compose -p dagbench -f docker-compose.yml up -d --wait

# controls that isolate WHY 0.64 was slow: same graph without health checks,
# and ten flat services with no dependencies at all
docker run --rm -v "$PWD":/svc sysg-bench bash /svc/ctl.sh
```

### Resource overhead

```sh
docker run --rm -v "$PWD":/h sysg-bench bash /h/bare2.sh 10     # baseline
docker run --rm -v "$PWD":/h sysg-bench bash /h/split2.sh 10 exec
docker run --rm -v "$PWD":/h sysg-bench bash /h/split2.sh 10 command
docker run --rm -v "$PWD":/h sysg-bench bash /h/sup2.sh 10
```

Memory is **PSS** from `/proc/*/smaps_rollup`, in KiB. RSS double-counts shared
pages, which would flatter a forked supervisor and penalise Python.

### Descendant containment

```sh
docker run --rm -v "$PWD":/h sysg-bench bash /h/orph.sh sysg
docker run --rm -v "$PWD":/h sysg-bench bash /h/orph.sh supervisor default
docker run --rm -v "$PWD":/h sysg-bench bash /h/orph.sh supervisor group
```

### Control-plane crash

```sh
docker run --rm -v "$PWD":/h sysg-bench bash /h/crash.sh sysg
docker run --rm -v "$PWD":/h sysg-bench bash /h/crash.sh supervisor
docker run --rm -v "$PWD":/h sysg-bench bash /h/hc.sh hc     # health-check variant
```

> The Docker row was produced by `kill -9` on dockerd inside the Docker Desktop
> VM. **That will stop every container on the machine** when `live-restore` is
> false, which is the default. Do not run it on a box you care about.

### Readiness

```sh
docker run --rm -v "$PWD":/h sysg-bench bash /h/ready2.sh sysg
docker run --rm -v "$PWD":/h sysg-bench bash /h/ss.sh 5 1    # startsecs default
docker run --rm -v "$PWD":/h sysg-bench bash /h/ss.sh 5 5    # tuned to startup
docker run --rm -v "$PWD":/h sysg-bench bash /h/ss.sh 8 5    # startup varies
```

---

## Ground rules used throughout

- **Same service body for every tool.** Where a comparison reuses a figure
  measured against a different body, it is re-measured rather than carried over.
- **Both configurations charted** wherever a setting changes the answer:
  `stopasgroup`/`killasgroup`, `startsecs`, `exec:` vs `command:`,
  `live-restore`. Comparing a tuned tool against a default one is the easiest
  way to get a wrong answer, and it happened twice during collection before
  being caught.
- **Clean-machine baseline for install metrics, warm steady-state for the rest.**
  Install cost is a provisioning question and containers and CI runners start
  empty; everything after installation is a behaviour question.
- **Nothing discarded.** Outliers stay in the results document. Runs thrown out for
  harness bugs are listed below with the bug.

## Harness bugs found during collection

Recorded because they all produced plausible, wrong numbers before being caught.

1. **supervisord silently fails to start if a command contains `%`** — its
   config parser interpolates. The service body used `date +%s.%N`, so the first
   memory run reported 0.73 MB at every N. Escape as `%%`.
2. **A readiness poll that times out is not a readiness time.** The first
   Supervisor readiness run reported ~16 s because `supervisorctl` cannot
   connect without a `[unix_http_server]` section, so the poll ran to its
   timeout while the process was already up. Real figure: 1.69 s.
3. **busybox `date` has no `%N`.** The first Compose graph run produced
   unparseable timestamps; `debian:bookworm-slim` was substituted so the
   instrumentation matches the other tools byte for byte.
4. **`pgrep -f <pattern>` also matches the measuring shell's own command line.**
   This produced a false "sysg double-started after a crash" reading. Isolating
   the variable showed identical pids before and after.

## Known gaps

- Compose ran against the host daemon while sysg ran in a container, so the
  graph-start comparison is indicative rather than controlled. Both in one VM
  would settle it.
- systemd is measured on install size only. Its runtime rows need a real VM,
  since it has to be PID 1.
- Docker's `live-restore: true` is untested. Docker-in-Docker is not a valid
  harness for it: `dockerd` is the DinD container's main process, so killing it
  kills the container and measures nothing.
- Trial counts are 2–5 per figure, cold. A reviewed protocol would want 10 cold
  plus 10 cached, with median, IQR and failure counts.
- Per-service times conflate scheduling, spawn, and the service's own init. An
  `entry_ns` stamp would separate them.

---

# Full results

Every trial, the reasoning behind each metric, and the harness bugs found while
collecting. The post is the short version of what follows.

## Scope and ground rules

```text
Started:    2026-08-20
sysg ver:   v0.64.4 (published release artifacts)

This is a LIVING, MULTI-METRIC document. No single metric decides anything.
Each metric gets its own numbered section built to the same template, so
sections can be added without disturbing the ones already collected.

PER-METRIC SECTION TEMPLATE
  x.0  Metric definition + what counts as a valid measurement
  x.1  Raw trials      (every trial, nothing discarded)
  x.2  Summary         (median, min-max)
  x.3  Reading         (what it does and does NOT support)
  x.4  Reproduction    (host, images/digests, exact commands)
  x.5  Gaps            (what this harness cannot prove)

Units:      MB = decimal megabytes (bytes / 1,000,000).
            apt "Installed-Size" is reported by dpkg in KiB; converted at
            x1024 before the MB conversion.
```

### 0.  HOW TO USE THIS DATA

```text
sysg is not going to win every metric, and this file is not built to make it
look like it does. It is built so that when a number IS quoted, it survives
someone re-running it.

Rules for anything quoted out of this file:

  1. Quote the comparison, not the superlative. "14x smaller than a Docker
     Compose install" is defensible and measured. "Lighter than anything else
     out there" is not -- systemd's package is SMALLER than sysg on Linux, and
     Supervisor on a machine that already has Python is smaller still.

  2. Name the platform. sysg is 12.0 MB on macOS and 19.5-23.6 MB on Linux.
     Any single unqualified number is wrong on some platform.

  3. FIXED BASELINE: a machine with NONE of these tools installed. Every
     tool is costed including whatever runtime it needs to function.
     Supervisor requires Python, so Python is part of Supervisor -- there is
     no Supervisor without it. Compose requires a container engine, so the
     engine is part of Compose. sysg carries its runtime inside the binary
     and is costed the same way: whole.
     A "Python already present" figure is NOT a comparison and must never
     appear in a head-to-head table. It is a marginal cost against a
     different baseline, and mixing baselines is how comparisons lie.

     WHY CLEAN-MACHINE IS THE RIGHT BASELINE FOR METRICS 1-3, AND ONLY THOSE.
     This was challenged in review ("on a real box Python and Docker often
     already exist, so marginal cost is the realistic number"). The objection
     is right for steady-state metrics and wrong for provisioning metrics:

       * Metrics 1-3 measure PROVISIONING COST -- what it takes to get to a
         working supervisor from nothing. For containers, CI runners, fresh
         VMs and images, the clean machine IS the machine. That is the
         dominant deployment shape, not a contrived one.
       * Assuming the runtime is already present does not make metrics 1-3
         fairer, it makes them measure something else. "Install Supervisor
         given Python" is `pip install`, which is a component of the question,
         not the question.
       * Metrics 4-8 DO use warm, steady-state comparison, because there the
         objection is correct: once everything is installed, what matters is
         behaviour, not provisioning.

     FETCH-TIME vs RUNTIME DEPENDENCIES -- the distinction that answers the
     "you excluded curl and CA certs from sysg" objection:
       * curl / CA certs are FETCH-TIME. `scp` the binary and you never need
         them. Nothing about RUNNING sysg requires them.
       * Python is RUNTIME, permanently. containerd/runc likewise for Compose.
         Remove them and the tool stops working.
       Charging a runtime dependency while not charging a fetch-time one is
       the correct distinction, not an asymmetry.

     CONCEDED: sysg is not literally zero-runtime-dependency. It spawns
     services through `/bin/sh` (see 5.4, the wrapper shell), so it depends
     on a POSIX shell. That is present on every Unix by definition and is not
     comparable to requiring CPython -- but the accurate phrasing is
     "no interpreter, no daemon of its own, no VM", not "no dependencies".

     ALSO CONCEDED FROM THE SAME REVIEW, and acted on: metric 6 originally
     compared a TUNED sysg against a DEFAULT Supervisor. That was a real
     unfairness and the tuned Supervisor row now exists (0 vs 2, not 0 vs 5).
     The equivalent checks for metric 8 (`startsecs` raised) and metric 7
     (`live-restore: true`) are still OUTSTANDING and those two metrics
     should be read as provisional until they are run.

  4. Ties are results. Metric 8: a TUNED Supervisor matches sysg exactly at
     +0.00 s readiness accuracy. That is a tie and it is reported as one.

  5. Where sysg genuinely wins, it wins on SHAPE, not on beating every number:
     one self-contained binary, no daemon, no interpreter, no VM, rootless
     install, uninstall by deleting a directory. That claim holds against all
     three comparables and does not depend on winning a size or speed race.

  6. Losses stay in the file. Anything removed because it was unflattering
     makes every remaining number untrustworthy.
```

### 0.1  METRIC INDEX

## 1. Installed size

```text
  #   METRIC                                   STATUS      HEADLINE
  --  ---------------------------------------  ----------  --------------------
  1   Installed size (on disk, after install)  DONE        Compose 14x; systemd
                                                           is SMALLER than sysg
  2   Install payload (bytes over the wire)    DONE        Ubuntu 22.04 cold:
                                                           sysg 6.8 vs 48 vs 172
  3   Install time (to first supervised svc)   PARTIAL     Ubuntu baseline:
                                                           sysg 3.7x (see 2.6)
  4   Dependency-graph start (10-svc DAG)      PARTIAL     sysg 6.91s vs 8.31s
                                                           BUT envs differ (4.4)
  5   Resource-usage overhead (composer tax)   PARTIAL     Compose 25x sysg;
                                                           v0.66 exec: see 5.6
  6   Descendant containment on stop           DONE        sysg 0, Compose 0,
                                                           Supervisor 2 (tuned)
  7   Control-plane crash durability           PARTIAL     Supervisor DUPLICATES;
                                                           Compose = DEFAULT cfg
  8   Readiness semantics (running != ready)   DONE        tuned Supervisor
                                                           TIES sysg; see 8.2
  9   Restart correctness under flapping       PLANNED
  10  Dependency failure containment/self-heal PLANNED
  11  Boot / cold start (single svc, no graph) PLANNED
  12  Idle CPU while supervising               PLANNED
  13  Uninstall completeness + time            PLANNED
  14  Config size for an identical 5-svc stack PLANNED
  15  Runtime dependency count                 PLANNED
  16  Time to first diagnosis of a failed svc  PLANNED

  PARTIAL = collected but short of the reviewed protocol, or missing rows that
  need a VM. Never quote a PARTIAL section without its caveat.

  NOTE ON NAMING: three different things get called "install size" or
  "install speed". Keep them apart:
    metric 1 = what lands ON DISK after installing.
    metric 2 = what crosses the NETWORK to get it there.
    metric 3 = INSTALL time, nothing -> a running supervised service.
    metric 4 = BOOT time, an already-installed tool starting services.
  They are different numbers and must never be quoted for each other.
```

### 1.1  sysg -- single self-contained binary, stripped

```text
Source: github.com/ra0x3/systemg release v0.64.4, tarballs unpacked.

  TARGET                              BYTES         MB     DOWNLOAD (.tar.gz)
  aarch64-apple-darwin           12,007,624      12.01           4,498,386
  x86_64-apple-darwin            12,682,820      12.68           4,757,080
  aarch64-unknown-linux-gnu      17,663,816      17.66           6,736,617
  x86_64-unknown-linux-gnu       19,500,064      19.50           7,002,935
  aarch64-linux-musl (Alpine)    21,138,952      21.14           7,986,317
  x86_64-linux-musl (Alpine)     23,605,680      23.61           8,018,251

  Runtime dependencies: a POSIX shell, plus a resident sysg supervisor
  process. No interpreter, no VM, no package manager, no separate
  engine/daemon pair. (curl/CA certs are fetch-time only -- the binary can be
  copied in and run without them.) NOTE: sysg IS itself a long-running
  daemon; "no daemon" would be false.
  Unstripped local build for reference: 14,421,592 B (macOS arm64).

  NOTE: Linux is 1.5-2.0x the macOS size. Prime suspect is vendored OpenSSL,
  linked on Linux only (Cargo.toml: openssl-sys features=["vendored"], with a
  second entry under the musl target). It is C, so Rust-level size tools do
  not see it. Unverified -- see Section 3 open items.
```

### 1.2  systemd

```text
Source: debian:bookworm, apt.

  COMPONENT                           BYTES         MB
  /usr/lib/systemd/systemd (PID 1)      133,464      0.13
  /usr/bin/systemctl                  1,387,544      1.39
  /usr/bin/journalctl                   133,624      0.13
  ---
  systemd package, all files         12,879,810     12.88   (measured, dpkg -L)
  systemd package (Installed-Size)   13,556,736     13.56
  libsystemd0    (Installed-Size)     1,010,688      1.01
  ===
  systemd + libsystemd0              14,567,424     14.57

  Marginal install cost on a normal Linux distro: ZERO. It is already PID 1.
  Not separately installable or removable in any practical sense.
```

### 1.3  Supervisor (Python)

```text
Source: debian:bookworm, supervisor 4.3.0 via pip into a clean venv.

Supervisor IS Python + Supervisor. It cannot run without a Python runtime, so
the runtime is part of its cost, exactly as the container engine is part of
Compose's. The split below is a BREAKDOWN of where that cost sits -- it is not
a set of alternative figures to choose between.

  COMPONENT                           BYTES         MB
  supervisor site-packages            3,731,209      3.73
  ---  every python3 package, dpkg Installed-Size:
  libpython3.11-stdlib               10,112,000     10.11
  python3.11-minimal                  6,710,272      6.71   <- interpreter bin
  libpython3.11-minimal               5,366,784      5.37
  python3.11                            665,600      0.67
  python3-minimal                       113,664      0.11
  python3                                82,944      0.08
  libpython3-stdlib                      27,648      0.03
  ===
  Python runtime subtotal            23,078,912     23.08
  Supervisor + Python runtime        26,810,121     26.81

  CORRECTION 2026-08-21: an earlier revision gave the Python subtotal as
  15.68 MB. That OMITTED `python3.11-minimal` -- the package containing the
  interpreter binary -- plus `python3.11` and `libpython3-stdlib`. Verified:
     dpkg-query -W -f='${Installed-Size}\t${Package}\n' | grep python3
  totals 22,538 KiB = 23.08 MB. The error UNDERSTATED Supervisor by 7.4 MB
  and produced a false "tie". Corrected in favour of the measurement, which
  here happens to favour sysg.

  Of the 26.81 MB total, 23.08 MB (86%) is the Python runtime and 3.73 MB
  (14%) is Supervisor itself. The runtime dominates at 6.2x the size of the
  supervisor it exists to run.
```

### 1.4  Docker Compose

```text
Compose is a CLI plugin. It cannot run anything without a container engine,
so both the plugin alone and the minimum working stack are given.

  A) Compose plugin binary alone (v2.32.4, GitHub release)
     COMPONENT                        BYTES         MB
     docker-compose-linux-x86_64  64,694,701      64.69
     docker-compose-linux-aarch64 62,902,454      62.90
     docker-compose (macOS arm64) 74,866,784      74.87

  B) Minimum stack to actually run `docker compose up` (Debian, apt)
     PACKAGE                          BYTES         MB
     docker-ce                   105,640,960     105.64
     docker-ce-cli                43,390,976      43.39
     containerd.io                83,117,056      83.12
     docker-compose-plugin        46,485,504      46.49
     ===
     TOTAL                       278,634,496     278.63

     (docker-buildx-plugin, 68,129,792 B / 68.13 MB, is installed by default
      but is not required by Compose. Excluded from the total.)

  C) macOS / Windows -- Docker Desktop, which ships a Linux VM
     Docker.app (this machine)  2,241,007,616   2,241.01   (2.24 GB)
     docker CLI                    41,292,240      41.29
```

### 1.5  HEAD-TO-HEAD

```text
Linux x86_64, minimum to run the tool at all, on a machine that has nothing
preinstalled:

  TOOL                          BYTES         MB      vs sysg
  sysg                     19,500,064      19.50        1.00x
  systemd (pkg+lib)        14,567,424      14.57        0.75x
  Supervisor + Python      26,810,121      26.81        1.37x
  Docker Compose stack    278,634,496     278.63       14.29x

macOS arm64:

  TOOL                          BYTES         MB      vs sysg
  sysg                     12,007,624      12.01        1.00x
  Docker Desktop        2,241,007,616   2,241.01      186.6x
  systemd                          --         --    not available
  Supervisor + Python              --         --    (Python preinstalled)

HONEST READING OF THE ABOVE:
  * Against Docker Compose the gap is decisive: 14x on Linux, 187x on macOS.
    This is the comparison that carries the claim.
  * Against Supervisor, sysg is 27% SMALLER: 19.50 vs 26.81 MB. Both carry a
    whole runtime; sysg's is compiled in at ~12 MB, Supervisor's is 23.08 MB
    of CPython -- 86% of its footprint.
  * Against systemd, sysg is LARGER (19.50 vs 14.57 MB), and systemd's
    marginal cost is zero because it is already PID 1.
  * The size claim should therefore be made against Docker Compose
    specifically, NOT as "smaller than everything on the list."

WHERE sysg ACTUALLY WINS ON FOOTPRINT (not size -- shape):
  * No interpreter, no VM, no engine/daemon pair. (sysg is itself a resident daemon; a POSIX shell is required.)
  * Rootless install; no package manager required.
  * Uninstall is deleting a directory.
  * One process, not a client/daemon split.
```

### 1.6  REPRODUCTION

```text
sysg:
  gh release download v0.64.4 -R ra0x3/systemg -p 'sysg-0.64.4-*.tar.gz'
  for f in sysg-*.tar.gz; do tar xzf "$f" -C "${f%.tar.gz}"; done
  find . -name sysg -type f -exec stat -f %z {} \;

systemd + Supervisor:
  docker run --rm debian:bookworm bash -c '
    apt-get update -qq && apt-get install -y -qq systemd python3 python3-venv
    dpkg-query -W -f="\${Installed-Size}\t\${Package}\n" systemd libsystemd0
    dpkg -L systemd | while read p; do [ -f "$p" ] && stat -c %s "$p"; done |
      awk "{s+=\$1} END {print s}"
    python3 -m venv /tmp/v && /tmp/v/bin/pip install -q supervisor
    du -sb /tmp/v/lib/python3*/site-packages/supervisor'

Docker:
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

## 2. Install payload

```text
Collected:  2026-08-20

2.0  DEFINITION
     Two different numbers, both reported, because they answer different
     questions:
       (a) PUBLISHED ARTIFACT BYTES -- what the vendor ships. Tarballs, .deb
           Size: fields, wheels. Reproducible from a release page.
       (b) MEASURED WIRE BYTES -- eth0 rx delta across a real install in a
           fresh container. Includes repo metadata, index refreshes, TLS, and
           transitive dependencies. This is what a machine actually pays.
     (b) is always larger than (a), sometimes by a lot. Quote (b) for "how
     much will this cost me"; quote (a) only when comparing artifacts.
```

### 2.1  (a) PUBLISHED ARTIFACT BYTES

```text
  sysg -- release tarballs, v0.64.4
    aarch64-apple-darwin                4,498,386     4.50 MB
    x86_64-apple-darwin                 4,757,080     4.76 MB
    aarch64-unknown-linux-gnu           6,736,617     6.74 MB
    x86_64-unknown-linux-gnu            7,002,935     7.00 MB
    aarch64-linux-musl (Alpine)         7,986,317     7.99 MB
    x86_64-linux-musl (Alpine)          8,018,251     8.02 MB

  Supervisor -- PyPI wheel
    supervisor-4.3.0-py2.py3-none-any    320,736     0.32 MB
    This is the wheel ALONE and is not a comparable figure: it does not run
    without a Python runtime. See 2.2 for Supervisor's real payload.

  Docker + Compose -- Debian .deb `Size:` fields
    docker-ce                          20,527,660    20.53 MB
    docker-ce-cli                      15,293,944    15.29 MB
    containerd.io                      19,099,456    19.10 MB
    docker-compose-plugin               9,408,596     9.41 MB
    ===
    TOTAL                              64,329,656    64.33 MB
    (docker-buildx-plugin 14,717,684 / 14.72 MB installed by default,
     not required by Compose, excluded)

  Compose plugin standalone (uncompressed binary, GitHub release v2.32.4)
    docker-compose-linux-x86_64        64,694,701    64.69 MB
    docker-compose-linux-aarch64       62,902,454    62.90 MB

  systemd -- n/a. Ships with the distro; there is no artifact to fetch.
```

### 2.2  (b) MEASURED WIRE BYTES  (eth0 rx delta, fresh container, cold)

```text
  TOOL                          MEDIAN BYTES        MB   TRIALS
  sysg                             6,812,042      6.81        5
  Supervisor (Python + pkg)      119,753,511    119.75        3
  Docker + Compose               117,943,013    117.94        3
  systemd                                 --        --      n/a

  BREAKDOWN, not a comparison row: of Supervisor's 119.75 MB, the supervisor
  package itself accounts for 0.34 MB measured (5 trials on an image that
  already had Python). The other 119.41 MB is the Python runtime and the apt
  metadata needed to fetch it.

  Docker's figure is the apt transaction only. Repository keyring and
  sources.list setup happened before the counter was baselined, and NO image
  has been pulled yet -- a first `compose up` adds the image on top.
```

### 2.6  REALISTIC-BASELINE RUN -- stock Ubuntu 22.04 dev box

```text
Added 2026-08-21 to answer the "your clean machine is contrived" objection by
MEASURING it rather than arguing about it.

BASELINE: ubuntu:22.04 with python3 3.10.12, curl 7.81.0 and ca-certificates
already present -- i.e. what a stock Ubuntu dev box actually has. Those are
installed OUTSIDE the timed window. Docker is NOT present, because a stock
Ubuntu box does not have it.

Each tool installed the way a developer would actually install it on Ubuntu:
    sysg        curl -fsSL https://sh.sysg.dev | sh
    Supervisor  apt install supervisor      (the distro package, not a venv)
    Compose     docker-ce + cli + containerd.io + docker-compose-plugin

RESULT (2 trials each; to_ready = install + one service supervised)
    TOOL          install        to_ready       rx bytes
    sysg          1.22 / 1.50 s  1.49 / 1.78 s   6,841,975
    Supervisor    4.45 / 4.58 s  5.53 / 5.66 s  47,952,570
    Compose      15.85 /16.69 s  (pkg only)    172,504,339

DECOMPOSITION -- because charging Supervisor for Ubuntu's package index would
be dishonest:
    apt index metadata alone     47,552,321 bytes   (47.55 MB)
    supervisor .debs alone          414,613 bytes   ( 0.41 MB)
    installed size added          2,226 KB = 2.28 MB (supervisor +
                                  python3-pkg-resources, on top of the
                                  Python that was already there)

READING -- and this cuts both ways, deliberately:
  * COLD apt cache (fresh box, CI runner, new container -- the dominant
    modern case): sysg wins decisively. 1.49 s / 6.84 MB against 5.53 s /
    47.95 MB and 15.85 s / 172.50 MB.
  * WARM apt cache (a dev box that ran apt today): Supervisor's marginal
    payload is 0.41 MB, which is 17x SMALLER than sysg's 6.84 MB, and its
    install time drops to roughly sysg's. On that specific machine, in that
    specific state, Supervisor is the lighter install. That is a real result
    and it belongs here.
  * Compose loses in every cache state by an order of magnitude.
  * The durable sysg advantage on this metric is PREDICTABILITY, not just
    magnitude: 6.84 MB and ~1.5 s regardless of cache state, mirror
    geography, dependency resolution, or what else is installed. The apt
    path varies between 0.41 MB and 48 MB depending on state you do not
    control.

WHY THIS BASELINE AND NOT A DEP-BY-DEP STANDARDISATION: standardising every
transitive dependency ends up measuring how fast the box installs apt
packages, which is a property of the mirror and the machine, not of the
supervisors. Naming a realistic baseline machine and installing each tool the
way its users actually install it is the comparison that means something.
```

### 2.3  READING

```text
  * sysg pays 6.81 MB measured / 7.00 MB artifact on Linux x86_64.
  * Supervisor pays 119.75 MB -- 17.6x sysg, and marginally MORE than Docker.
    The Python runtime, not Supervisor, is the cost: 0.34 MB of that total is
    the supervisor package.
  * Docker + Compose pays 117.94 MB before pulling a single image -- 17.3x
    sysg.
  * This is the metric where sysg's shape pays off hardest: one static
    artifact, no runtime to fetch, no dependency resolution.
  * The (a)/(b) gap is the honest surprise: Docker's published .debs total
    64.33 MB but a real install moves 117.94 MB. Anyone quoting only the
    published figure understates it by 1.8x.
  * sysg's gap is small (7.00 -> 6.81 MB; measured is LOWER because the
    artifact figure is the largest target and the container fetched the
    aarch64 build). Single-artifact installs are predictable; package-manager
    installs are not.
```

### 2.4  REPRODUCTION

```text
  Published artifact bytes:
    gh release view v0.64.4 -R ra0x3/systemg --json assets \
      -q '.assets[] | "\(.size)  \(.name)"'
    apt-cache --no-all-versions show docker-ce docker-ce-cli containerd.io \
      docker-compose-plugin | grep -E "^(Package|Size):"
    pip download supervisor -d /tmp/w && ls -l /tmp/w

  Measured wire bytes: rx_bytes delta captured inside each install trial --
  same harness and same containers as Section 3.4. Read via
    cat /sys/class/net/eth0/statistics/rx_bytes
  before and after the timed window.
```

### 2.5  GAPS

```text
  * Cold only; a warm apt/pip cache would cut (b) sharply for Docker and
    Supervisor and barely move sysg. Warm rows are outstanding.
  * No image-pull row for Docker. A pinned BusyBox pull should be added so
    "what Compose costs to actually run something" is covered.
  * rx_bytes counts the container's interface, so Docker Desktop's own NAT
    layer is excluded. Fine for relative comparison, not an absolute.
  * sysg's measured row is the aarch64 tarball (host is arm64); the artifact
    table lists both architectures.
```

## 3. Install time

```text
Collected:  2026-08-20
Host:       macOS Darwin 25.2.0, arm64, 10 CPU, 32 GiB RAM
Harness:    Docker Desktop, debian:bookworm @ sha256:813017f3d62be4b5891a7acca6a01bdcd4b8513daa81b1ab99d3a50385b26931
            Fresh container per trial. Cold: real network every trial.

METRICS (methodology reviewed with Codex before collection)
  T_pkg    seconds until the install command returns
  T_cli    seconds until a shell resolves the tool and `--version` exits 0
  T_ready  seconds from start of install until the tool reports a trivial
           service (`sleep 600`) RUNNING via its OWN status command.
           T_ready is the primary metric. Config authoring is excluded.
  rx       bytes received on eth0 across the measured window

WHAT A CONTAINER CAN AND CANNOT PROVE
  Valid here:   sysg install->ready, Supervisor install->ready, and Docker's
                dpkg transaction (T_pkg only).
  NOT valid:    Docker T_ready and systemd T_ready. The Docker daemon cannot
                start in a plain container and systemd is not PID 1 here.
                Those rows require a real VM and are marked PENDING below.
  Disclosure:   container writes land on overlayfs, not raw disk.
```

### 3.1  RAW TRIALS  (seconds; rx in bytes)

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

  BREAKDOWN, not comparison rows -- the pip step alone, on an image that
  already had Python. Recorded so the 119.75 MB / 21.72 s above can be
  attributed, NOT for use in any head-to-head:
                       0.49    0.04     1.62     yes       342,898
                       0.57    0.04     1.69     yes       342,458
                       0.56    0.04     1.73     yes       342,260
                       0.50    0.04     1.62     yes       343,363
                       0.52    0.03     1.73     yes       343,229

  docker+compose       9.34    0.02      n/a     n/a   117,903,979
  (dpkg only)          9.61    0.02      n/a     n/a   117,943,013
                      14.69    0.02      n/a     n/a   117,984,563

  systemd                --      --       --      --            --   PENDING (VM)
```

### 3.2  SUMMARY  (median, [min-max])

```text
  TOOL                      T_pkg           T_ready          rx (MB)
  sysg                 1.36 [1.22-3.15]  1.65 [1.51-3.43]      6.81
  Supervisor          20.60 [18.6-31.6] 21.72 [19.7-32.8]    119.75
  Docker + Compose     9.61 [9.34-14.7]      PENDING          117.94
```

### 3.3  READING

```text
  * sysg reaches a supervised service in 1.65 s median, from nothing, with no
    package manager and no root.
  * Against Supervisor, sysg is 13.2x faster: 1.65 s vs 21.72 s, moving 17.6x
    fewer bytes. Supervisor has to fetch and install a Python runtime before
    it can supervise anything.
  * The pip step alone is 0.52 s, faster than sysg's 1.36 s -- but that step
    presupposes a runtime already on the machine and is not a comparison.
  * Docker's package transaction alone is ~6x sysg's entire time-to-ready,
    before the daemon has started or a single image has been pulled. Its true
    T_ready will be strictly larger and needs the VM run.
  * The 3.15 s sysg outlier and the 31.63 s Supervisor outlier are both
    network variance on a single trial. Reported, not discarded.

  CAVEAT: the svc_up column for the Supervisor rows reads "--" because
  pgrep is not present in a bare debian:bookworm image. Readiness for those
  rows was still confirmed by supervisorctl reporting RUNNING -- T_ready sits
  ~1.2 s after T_pkg rather than at the 10 s poll timeout.
```

### 3.4  REPRODUCTION -- install time

```text
Everything below was run on 2026-08-20 from this Mac against Docker Desktop.
No number in Section 2 came from anywhere else.

HOST
  macOS Darwin 25.2.0, arm64, 10 CPU, 32 GiB RAM (sysctl hw.ncpu hw.memsize)

BASE IMAGES
  debian:bookworm @ sha256:813017f3d62be4b5891a7acca6a01bdcd4b8513daa81b1ab99d3a50385b26931
  sysg-bench: debian:bookworm + curl ca-certificates python3 python3-venv
              python3-pip procps, apt lists removed. Built so that installing
              those prerequisites is NEVER inside a timed window.

  Dockerfile.bench:
    FROM debian:bookworm
    ENV DEBIAN_FRONTEND=noninteractive
    RUN apt-get update -qq && apt-get install -y -qq \
          curl ca-certificates python3 python3-venv python3-pip procps \
        && rm -rf /var/lib/apt/lists/*

  docker build -t sysg-bench -f Dockerfile.bench .

TIMING PRIMITIVES (identical for every tool)
    now() { date +%s.%N; }
    el()  { awk -v a="$1" -v b="$2" 'BEGIN{printf "%.2f", b-a}'; }
    RXB() { cat /sys/class/net/eth0/statistics/rx_bytes; }

sysg  -- one fresh container per trial:
    docker run --rm -v ./bench.sh:/b.sh sysg-bench bash /b.sh sysg

    T0; curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev | sh; T1
    export PATH="$HOME/.local/bin:$HOME/.sysg/bin:$PATH"
    sysg --version; T2
    printf 'version: "2"\nservices:\n  p:\n    command: "sleep 600"\n' \
      > /tmp/svc/systemg.yaml
    sysg start -c /tmp/svc/systemg.yaml --daemonize
    poll: sysg status -c /svc/systemg.yaml --format json  (100 x 0.1 s)
    T3; pgrep -f "sleep 600"

Supervisor breakdown run (pip step alone, NOT a comparison row) -- sysg-bench
image, which already contains Python:
    docker run --rm -v ./bench.sh:/b.sh sysg-bench bash /b.sh supervisor

    T0; pip install -q --break-system-packages supervisor; T1
    supervisord -v; T2
    write /tmp/sd.conf  (SEE HARNESS BUG BELOW -- must include
                         [unix_http_server] and [supervisorctl])
    supervisord -c /tmp/sd.conf
    poll: supervisorctl -c /tmp/sd.conf status probe | grep RUNNING
    T3; pgrep -f "sleep 600"

Supervisor -- bare debian:bookworm. Python is installed INSIDE the timed
window because Supervisor does not exist without it:
    docker run --rm -v ./bench2.sh:/b.sh debian:bookworm bash /b.sh supervisor-nopy
    T0; apt-get update -qq
        apt-get install -y -qq python3 python3-pip
        pip install -q --break-system-packages supervisor; T1
        (readiness poll identical to the Python-present variant above)

Docker + Compose -- bare debian:bookworm. Repo setup happens BEFORE T0 and
rx is re-baselined after it, so the timed window is the apt transaction only:
    docker run --rm -v ./bench2.sh:/b.sh debian:bookworm bash /b.sh docker-pkg

    add download.docker.com keyring + sources.list.d entry   (untimed)
    R0=rx_bytes
    T0; apt-get update; apt-get install -y docker-ce docker-ce-cli \
        containerd.io docker-compose-plugin; T1
    docker compose version; T2

TRIAL COUNTS ACTUALLY RUN
    sysg 5, Supervisor 3, Docker 3 (+5 breakdown runs of the pip step).
    Codex's reviewed protocol asks for 10 cold + 10 cached. This is short of
    that. Treat the medians as indicative, not settled. No trial was
    discarded; both outliers (sysg 3.15 s, Supervisor 31.63 s) are in 2.1.

HARNESS BUG FOUND AND FIXED MID-COLLECTION -- recorded because it changes how
much you should trust an unreviewed benchmark:
    The first Supervisor run measured T_ready = 15.4-17.0 s. That was WRONG
    and flattered sysg by ~10x. The config had no [unix_http_server] section,
    so supervisorctl could not connect, so the readiness poll ran to its full
    10 s timeout while `sleep 600` was in fact already running (pgrep said
    yes). Adding [unix_http_server] + [rpcinterface] + [supervisorctl] gave
    the real figure, 1.62-1.73 s.
    Lesson applied to every row: a readiness poll that times out must never be
    reported as a readiness time. Cross-check with an independent signal
    (pgrep) before believing any number.

KNOWN GAPS IN THIS SECTION
    * No cached/warm-cache rows yet; all trials are cold.
    * Container writes are overlayfs, not raw disk.
    * Mirror geography and time-of-day not controlled.
    * Docker and systemd T_ready are absent by design, not by oversight:
      the daemon cannot start and systemd is not PID 1 in a container.
      See Section 3 outstanding rows.
```

## 3b. Install time — outstanding rows

```text
  * systemd + Docker T_ready in a clean Ubuntu VM (multipass). A container
    cannot prove either: the Docker daemon will not start and systemd is not
    PID 1. Multipass is installed; its daemon needs starting.
  * Docker Desktop macOS: DMG bytes, copy time, launch -> `docker info` ready.
    Interactive install with no repeatable reset -- label it
    "not benchmarked (interactive)", never zero.
  * Warm-cache rows for every tool. All current trials are cold.
  * Trial counts up to 10 cold + 10 cached per the reviewed protocol.
```

## 4. Dependency-graph start

```text
Collected:  2026-08-20

4.0  DEFINITION
     A fixed 10-service DAG, expressed identically in every tool. Measured:
       TOTAL      command start -> all 10 services up and reporting healthy
       PER-SVC    when each individual service became ready
     Services are INSTRUMENTED, not the supervisors: every service runs the
     same body -- sleep 0.3 s (simulated init), write `date +%s.%N` to a
     shared dir, then hold. Per-service times therefore come from the service
     itself and are byte-identical across tools, instead of parsing four
     different log formats or trusting four different status commands.

     THE GRAPH (a data pipeline; deliberately not web/REST)
       db        <- (none)          queue     <- (none)
       cache     <- db              ingest1/2 <- queue
       worker1/2/3 <- cache
       aggregator  <- worker1,worker2,worker3,ingest1,ingest2
       reporter    <- aggregator

     Critical path is 5 levels deep, with 2 independent roots and a 5-wide
     fan-in. This shape is chosen to expose ONE thing: does the tool start
     independent branches in PARALLEL, or one at a time?
       ideal parallel  = 5 levels x 0.3 s = 1.5 s
       fully serial    = 10 svcs  x 0.3 s = 3.0 s
```

### 4.1  RESULTS  (3 trials each; per-service seconds from command start)

```text
  sysg v0.65.0 -- TOTAL 6.901 / 6.912 / 6.910   (median 6.91)   ** CURRENT **
     db 0.312, queue 0.309                       <- level 1, together
     cache 1.687, ingest1 1.687, ingest2 1.687   <- level 2, together
     worker1 3.067, worker2 3.066, worker3 3.066 <- level 3, together
     aggregator 4.449                            <- level 4
     reporter 5.819                              <- level 5

  sysg v0.64.4 -- TOTAL 13.839 / 13.827 / 13.782 (median 13.83)  [superseded]
     db 0.310 | cache 1.699 | queue 3.070 | ingest1 4.451 | ingest2 5.835
     worker1 7.228 | worker2 8.613 | worker3 10.005 | aggregator 11.382
     reporter 12.765
     Serial: 10 units ~1.38 s apart. Kept for the before/after record.

  Docker Compose -- TOTAL 8.407 / 8.312 / 8.295   (median 8.31)
     db 0.788, queue 0.790                      <- level 1, together
     cache 2.411, ingest1 2.411, ingest2 2.411  <- level 2, together
     worker1 3.990, worker2 3.997, worker3 3.995<- level 3, together
     aggregator 5.593                           <- level 4
     reporter 7.158                             <- level 5

  Supervisor -- CANNOT EXPRESS THIS GRAPH. supervisord has integer `priority`
     for start ORDER only; it has no dependency edges and no health gating, so
     "start B after A is healthy" is not expressible. Recorded as a RESULT,
     not a caveat. Any priority-ordered approximation would be measuring a
     different thing and is not reported.

  systemd -- PENDING. Requires PID 1; a container cannot prove it. Needs the
     multipass VM.
```

### 4.2  READING

```text
  ** RESULT CHANGED IN v0.65.0. sysg now wins this metric. **
    v0.64.4  13.83 s  (serial)
    v0.65.0   6.91 s  (concurrent)   -- 2.00x faster, and 1.20x faster than
    Compose   8.31 s                    Docker Compose

  * sysg v0.65.0 completes the graph in 5 clean level-waves, identical in
    shape to Compose's: both independent roots together, all three workers
    together. Per-level cost ~1.38 s (0.3 s init + ~1.08 s health poll).

  * The 6.91 s outcome matches the prediction made from the v0.64.4 data
    (5 levels x 1.38 s ~= 6.9 s). sysg's per-unit overhead was already
    competitive; only the scheduling was wrong.

  * WHY sysg NOW BEATS COMPOSE: both run 5 waves, but Compose pays ~1.58 s
    per level because each unit is a container create+start, while sysg pays
    ~1.38 s because each unit is a process. Same shape, cheaper units.

  * HISTORICAL -- what v0.64.4 did wrong, kept because it explains the fix:
    services started STRICTLY ONE AT A TIME. Not a dependency-resolution
    problem: `queue` had NO dependencies and still waited until 3.07 s. A
    control run of 10 FLAT independent services also started 0.27 s apart,
    proving the serialization was global. Two code sites walked their order
    sequentially (daemon.rs:5296, supervisor.rs:626).

  * CONTROLS (run to isolate the cause, same harness):
      same DAG, health checks removed  -> 2.752 s, services 0.27 s apart
      NO dependencies at all, flat x10 -> 2.761 s, services 0.27 s apart
    A flat set of 10 fully independent services with nothing to wait for
    still starts serially. The serialization is global, not graph-derived.

  * Confirmed in source, in TWO places -- both would have to change:
      daemon.rs:5296     `'service_loop: for service_name in order`
      supervisor.rs:626  `start_project_services` walks startup_service_order
    Each service is started and health-gated to completion before the next
    begins. There is no parallel start path. (Codex located the supervisor.rs
    site independently of the daemon.rs one.)

  * COST BREAKDOWN of sysg's ~1.38 s per service: ~0.3 s is the service's own
    init; the remaining ~1.1 s is health-poll latency. sysg's duration parser
    (`daemon.rs:7659`) accepts only whole seconds/minutes/hours, so 1 s is the
    FINEST health-check interval expressible. `interval: "100ms"` is rejected.

  * THE CONSTRUCTIVE READING: sysg's per-unit overhead is competitive.
    Compose pays ~1.58 s per LEVEL (container create included); sysg pays
    ~1.38 s per SERVICE. If sysg parallelized independent branches it would
    finish this graph in roughly 5 x 1.38 = 6.9 s and BEAT Compose's 8.31 s.
    It loses this metric on scheduling, not on overhead.

  * SCALING: the gap widens with graph WIDTH, not depth. Compose's total
    tracks the critical path (levels); sysg's tracks the service count. For a
    40-service project of similar shape, extrapolating: Compose stays ~8 s,
    sysg approaches ~55 s.

  * README currently claims "Dependencies: Topological, health-aware". Both
    words are true and the ordering is correct -- but it omits that startup is
    fully serialized, which is the dominant cost at this scale.
```

### 4.3  REPRODUCTION

```text
  Host: macOS Darwin 25.2.0, arm64, 10 CPU, 32 GiB. Docker Desktop.
  sysg:    debian:bookworm + curl/python (image `sysg-bench`), sysg installed
           via sh.sysg.dev inside the container, `sysg start -c /svc/systemg.yaml --daemonize`,
           poll until 10 ready-files exist.
  Compose: `docker compose up -d --wait`, images PRE-PULLED
           (debian:bookworm-slim), depends_on: condition: service_healthy,
           healthcheck interval 1s to match sysg's floor.
  Service body, identical everywhere:
           sh -c "sleep 0.3; date +%s.%N > /ready/<name>; exec sleep 3600"
  Generator scripts and both manifests: ~/Desktop/sysg-comp-harness/
  (dag.json, systemg.yaml, docker-compose.yml, run-sysg.sh, ctl.sh).

  NOTE: the first Compose attempt used busybox, whose `date` has no %N. That
  produced unparseable stamps and was discarded, not reported. debian-slim was
  substituted so instrumentation is identical to the sysg run.
```

### 4.4  GAPS  (methodology reviewed by Codex after collection; its findings below)

```text
  ** ENVIRONMENT ASYMMETRY -- the biggest threat to these numbers. **
    sysg was measured INSIDE a Docker container (overlayfs, container CPU
    scheduling). Compose was measured against the HOST Docker daemon. Those
    are not the same environment, and the comparison inherits the difference.
    Codex's fix: run every tool in ONE Linux VM, where systemd is native PID 1
    and every manager is equally warm. The 1.66x ratio should be treated as
    INDICATIVE until re-run that way. The serialization finding itself does
    NOT depend on this -- it is confirmed in source and by the flat-10 control.

  * PER-SERVICE TIMES CONFLATE THREE THINGS. A service's stamp marks
    process-ready, which is (supervisor decided to launch) + (spawn) +
    (0.3 s init). To separate them the service must also stamp `entry_ns` on
    first instruction; then `entry - max(parent ready)` isolates gating and
    spawn latency from init. NOT YET IMPLEMENTED -- current per-service
    numbers are ready-times only.

  * Command-return time should be recorded SEPARATELY from all-services-ready.
    `docker compose up -d` gates on healthy dependencies but not necessarily
    final health; `--wait` was used, which is correct, but the two totals are
    different numbers and only one is currently reported.

  * Timestamps use wall clock via `date +%s.%N`. CLOCK_MONOTONIC with atomic
    writes onto a bind-mounted tmpfs would remove clock-skew, mtime-resolution
    and overlayfs concerns entirely.

  * The 5-wide fan-in yields only 3 simultaneous initializers at the widest
    level, so this DAG under-tests parallelism. Codex suggests also running at
    5 s and 10 s init to check the ratios hold when real work dominates.

  * 3 trials, cold, single host. Variance was low (sysg 13.78-13.84,
    Compose 8.30-8.41) but the reviewed protocol asks for 30+ randomized runs
    reporting median / p95 / IQR and failure counts, not 3.
  * Compose's per-service time includes container create/start, which sysg
    does not pay. That is inherent to Compose and is NOT netted out.
  * Compose services are separate containers with their own PID namespaces;
    sysg services are processes. Not identical isolation -- disclosed, not
    corrected.
  * systemd row absent (needs VM). Supervisor row absent by capability.
  * 0.3 s init is short enough that per-level overhead dominates. A second run
    at 3 s init would show whether the ratios hold when real work dominates.
```

## 5. Resource-usage overhead

```text
Collected:  2026-08-20

5.0  DEFINITION
     RU(T, N) = resources(T supervising N services) - resources(same N
     services run bare). What the tool costs you ON TOP of your workload.

     Services are deliberately near-zero cost -- `sh -c "sleep 0.2; stamp;
     exec sleep 3600"` -- so the tool's tax dominates the measurement.

     UNITS: /proc/*/smaps_rollup reports PSS in KiB (1024 bytes). MB figures
     below are KiB x 1024 / 1e6. An earlier revision treated those values as
     kB and understated every memory figure in this section by 2.4%.

     MEMORY IS PSS (/proc/*/smaps_rollup), not RSS. RSS double-counts shared
     pages: it would flatter forked/COW supervisors and unfairly penalise
     Python. PSS splits shared pages by sharer count and is the correct
     primary userspace measure.

     BASELINE is a one-shot launcher that forks N services with `setsid` and
     EXITS. No resident parent process, so the zero point contains no
     supervisor of any kind.

     Linux figures: debian:bookworm container, total PSS of every process
     except the measuring shell -- which makes attribution disputes inside the
     container moot. Docker figures: measured inside the Docker Desktop VM via
     `docker run --privileged --pid=host`, counting the causal process
     closure (dockerd + containerd + containerd-shim per container).
```

### 5.1  RAW  (PSS in kB; procs = total processes)

```text
  N     bare PSS   procs      sysg PSS   procs   supervisor PSS   procs
   1       1,063       5        13,834       7           18,755       6
  10       2,291      14        16,126      25           20,091      15
  40       5,086      44        22,444      85           23,391      45
```

### 5.2  RU = tool - bare  (MB)

```text
  N        sysg    Supervisor    Docker Compose
   1      13.08         18.12         ~289  (engine + 1 shim)
  10      14.17         18.23         ~354  (engine + 10 shims)
  40      17.77         18.74         ~572  (engine + 40 shims, extrapolated)

  FITTED  RU(N) = intercept + slope x N
    sysg          12.96 MB  +  0.120 MB/service
    Supervisor    18.10 MB  +  0.016 MB/service
    Compose      281.9  MB  +  7.256 MB/service
  Crossover sysg/Supervisor unchanged at N ~= 49 (the unit error cancels).

  Docker component detail (measured in-VM, PSS):
    dockerd                       174.6 MB   (177.5 with 10 containers)
    containerd                     91.8 MB
    containerd-shim, baseline      15.5 MB   (1 shim present when idle)
    ---
    engine subtotal, idle         281.9 MB   paid before you run anything
    per additional container        7.3 MB   (measured: 10 shims = 72.6 MB)
    process count                 188 -> 209 for 10 services (+21)
```

### 5.6  v0.66.0 -- the `exec:` argv form   [RE-MEASURED 2026-08-21]

```text
v0.66.0 adds an OPT-IN argv form. A service declares either `command:` (shell
form, unchanged) or `exec:` (argv list, run directly with no shell):

    services:
      w:
        exec: ["/usr/bin/myapp", "--port", "8080"]

This was filed from this dataset as ~/Desktop/sysg-issue-wrapper-shell.md.

MEASURED, same harness (split2.sh + bare2.sh), matched bare baseline, PSS KiB
converted at x1024/1e6:

  ALL FOUR ROWS RE-MEASURED TOGETHER with the identical service body
  (`leaf.sh`), so this table is internally consistent. Supervisor was
  re-measured too rather than carried over from the older harness -- reusing
  its earlier fit would have compared different service bodies.

  N    bare       sysg exec    sysg command    Supervisor      (RU, MB)
   1   0.52 MB      12.22          12.33          17.88
  10   1.42 MB      12.44          13.55          18.07
  40   4.33 MB      13.57          17.99          18.67

  FITTED
    sysg exec form     RU(N) = 12.18 MB + 0.0346 MB/service
    sysg command form  RU(N) = 12.18 MB + 0.1451 MB/service
    Supervisor         RU(N) = 17.86 MB + 0.0202 MB/service

  PROCESS COUNT at N=40
    sysg exec form   45 processes   (1 per service + 5 overhead)
    sysg command     85 processes   (2 per service + 5 overhead)
    Supervisor       45 processes

  CROSSOVER vs Supervisor
    sysg command form   N ~= 46
    sysg exec form      N ~= 395     <- out of practical range

  HEAD-TO-HEAD, sysg exec form vs Supervisor
    N=10   12.44 MB vs 18.07 MB   sysg 1.45x lighter
    N=40   13.57 MB vs 18.67 MB   sysg 1.38x lighter

READING
  * The wrapper is gone under `exec:`. Process count per service drops from 2
    to 1, matching Supervisor exactly, and the memory slope falls 4.2x.
  * The residual 0.0346 MB/service is sysg's OWN per-service bookkeeping --
    pid, status, health config, restart state, log buffers. Independently
    corroborated: the supervisor process itself grows ~34 KiB/service under
    BOTH forms (11,922->13,275 KiB command, 11,926->13,252 KiB exec), so the
    remaining slope is real work, not waste.
  * Supervisor still has the lower slope (0.0202 vs 0.0346 MB/service), so it
    still wins eventually -- but at N~=395 instead of N~=46, which takes it
    out of the range a single host would realistically hit. Below that, sysg
    is lighter at every N: 1.45x at N=10, 1.38x at N=40.
  * Supervisor's own re-measured fit (17.86 MB + 0.0202) is close to its
    earlier one (18.10 + 0.0160), so the older figure was not badly wrong --
    but it was measured against a different service body and should not have
    been carried into this table without re-running it.
  * THIS IS OPT-IN AND THAT MATTERS. A manifest using the default `command:`
    form pays the wrapper exactly as before: slope 0.145 MB/service,
    crossover N~=46. The fix is available, not automatic. Quote it as
    "with the exec form", never as "sysg fixed it".
  * Prediction check: this dataset predicted the slope would fall to
    ~0.026 MB/service and the crossover to ~488. Actual: 0.0346 and ~395.
    Directionally right, optimistic by ~30% on the slope.

  CAVEAT: the earlier 5.1/5.2 tables used a different service body and are NOT
  directly comparable to these numbers. 5.6 is internally consistent -- bare,
  command and exec were all re-measured together with the same body.
```

### 5.3  MACOS HOST -- SEPARATE TABLE, DO NOT ADD TO THE ABOVE

```text
  Docker Desktop, host-side RSS, idle:            476 MB
    com.docker.backend (x3)                       230.3 MB
    Docker (UI, x3)                               166.1 MB
    com.docker.virtualization                      38.8 MB
    com.docker.build                               36.0 MB

  This is the Mac's own view of the VM and its helpers. The in-VM PSS figures
  in 5.2 are INSIDE that envelope -- adding them together would double-count.
  A Mac developer pays the 476 MB host cost AND gets the in-VM behaviour.
  sysg and Supervisor have no macOS equivalent: they are the process itself.
```

### 5.4  READING

```text
  * DOCKER COMPOSE: overhead is ~25x sysg at N=10 (354 vs 14.2 MB). It loses
    on BOTH terms -- a 275 MB engine you pay before running anything, and
    7.1 MB per container, which is 60x sysg's per-service slope. On a Mac add
    476 MB of host-side Docker Desktop on top. The hypothesis that Compose
    carries large overhead is CONFIRMED, decisively.

  * BUT COMPOSE IS NOT ONLY SUPERVISING. It containerises: separate PID and
    network namespaces, separate filesystems. That is more ISOLATION, not
    merely more overhead. Subtracting a bare-process baseline therefore
    measures "containerised composition cost", not pure supervision cost.
    Stated here so the number is not read as a straight indictment.

  * SYSG IS NOT ZERO-OVERHEAD. The hypothesis going in was "effectively
    zero"; the measurement says 12.96 MB resident intercept. That is the
    supervisor binary itself living in memory. It is SMALL and it is FLAT
    against a 282 MB engine -- but it is not zero and should not be claimed
    as zero.

  * VERIFICATION OF THE SUPERVISOR NUMBER. 18.10 MB looked low for "an
    entire Python runtime", so it was checked per-process:
        supervisord   pss = 17,644 kB   rss = 18,564 kB   (one process)
        sysg          pss = 12,562 kB                     (one process)
    The number is correct. The intuition conflates DISK with RESIDENT: the
    Python install is 23.08 MB on disk, but CPython mmaps libpython and
    imports only the modules supervisord actually uses -- most of the stdlib
    is .py files never read into memory. 17.6 MB resident IS the whole Python
    runtime cost. Note sysg is structurally the same thing: a ~12 MB static
    binary that also carries its entire runtime, just compiled in.

  * SYSG LOSES THE SLOPE -- AND THE CAUSE IS A REDUNDANT SHELL. sysg costs
    0.120 MB per supervised service; Supervisor costs 0.016 MB. The process
    trees show why:

      sysg:        sysg
                   +- sh -c sh -c '...; exec sleep 3600'   <- WRAPPER STAYS
                      +- sleep 3600
      supervisor:  supervisord
                   +- sleep 3600                           <- clean exec

    Both tools were handed the IDENTICAL `sh -c "..."` command. Supervisor's
    shell exec'd away and left one process. sysg wraps the command in its own
    additional `sh -c`, and that outer wrapper stays resident as the parent
    forever. So sysg pays an extra shell PER SERVICE: ~127 kB PSS and one
    process, which accounts for essentially ALL of its 0.120 MB/service slope.

    CROSSOVER TODAY: N ~= 49 services -- beyond that, Supervisor uses less
    memory than sysg. But this is a FIXABLE artifact, not an architectural
    loss. Remove the redundant wrapper and sysg's slope should approach
    Supervisor's 0.016 MB/service, pushing the crossover out to many hundreds
    of services or removing it entirely. This is a profiling/spawn-path fix,
    not a reason to soften the claim.

  * PROCESS COUNT follows directly: sysg runs 2 processes per service (85
    procs for 40 services vs bare's 44); Supervisor runs 1 (45 procs for 40).

  * DECOMPOSITION OF SYSG'S SLOPE (harness: split.sh; PSS in KiB):
        component                 N=1      N=10    marginal/service
        sysg supervisor process  12,468   12,699      ~26 KiB
        wrapper shells              471    1,703     ~137 KiB
        leaf processes              304    1,337     ~115 KiB (bare pays too)

    So ~84% of sysg's 120 KiB/service slope is the redundant wrapper, and only
    ~26 KiB is legitimate per-service bookkeeping (pid, status, health config,
    restart state, log buffers). Supervisor's ENTIRE per-service cost is
    16 KiB, so sysg's bookkeeping alone is in the same ballpark.

    Remove the wrapper and the slope should fall to ~26 KiB/service, moving
    the Supervisor crossover from N~=49 to N~=488 -- out of practical range.
    Filed as ~/Desktop/sysg-issue-wrapper-shell.md.

    CAVEAT: PSS per identical process FALLS as N rises (more `sleep` processes
    share more pages), so these components do not sum exactly to the fitted
    120 KiB slope. The decomposition is directionally right, not arithmetically
    exact. N=40 was attempted and timed out at 10 min.

  * HONEST HEADLINE: against Docker Compose, sysg's overhead is negligible
    (13.8 vs 346 MB at N=10). Against Supervisor it is a close-run thing that
    sysg wins at small N and loses past ~49 services.
```

### 5.5  GAPS

```text
  * COMPOSE'S "COMPOSER-ONLY TAX" NOT YET MEASURED. Codex's framing: Compose
    should be reported twice -- (a) end-to-end stack tax vs bare, which is the
    ~346 MB above, and (b) composer-only tax, Compose vs the identical
    containers started with plain `docker run`. (b) is expected to be near
    zero at rest, because the compose CLI exits after `up -d`; the cost is
    Docker's, not Compose's. Publishing only (a) overstates Compose the tool
    while correctly stating Compose the stack.
  * Engine measured WARM and SHARED (Docker Desktop was already running).
    A cold, dedicated-engine number would differ; both should be published.
  * No cgroup-v2 `memory.current` / `memory.stat` collection, so kernel-side
    memory -- network namespaces, overlayfs, cgroup structures -- is not
    counted for ANY tool. This systematically undercounts Docker most.
  * systemd not measured. Its marginal cost is NOT automatically zero:
    per-unit cgroups and journald growth are real and should be charged, even
    though its base footprint would exist anyway.
  * Idle CPU over >=5 min and file-descriptor deltas not yet collected;
    codex ranks those above thread counts, which are noise.
  * Single host, one trial per N, 3 s settle. No confidence intervals.
  * HARNESS BUG FOUND AND FIXED: the first Supervisor run reported ~0.73 MB
    at every N with only 4 processes -- supervisord had not started at all.
    Its config parser interpolates `%`, and the service body contains
    `date +%s.%N`. Escaping to `%%` fixed it. A tool that silently fails to
    start on a `%` in a command is itself worth noting.
```

## 6. Descendant containment

```text
Collected:  2026-08-20

6.0  WHAT THIS MEASURES, AND WHY IT MATTERS
     Plainly: does STOPPING a service actually stop it, or does the tool
     leave processes running while reporting the service stopped?

     A supervisor's core contract is start/stop. If stop leaves descendants
     behind, the tool is reporting a state that is not true, and four
     concrete things follow:
       * PORT CONFLICTS ON RESTART -- the orphan still holds the port, so the
         replacement cannot bind. This is the single most common symptom.
       * DUPLICATE WORK -- an orphaned worker keeps consuming the same queue
         as its replacement.
       * UNBOUNDED LEAK -- the loss is per stop/restart CYCLE, so a service
         that restarts hourly accumulates orphans until the box exhausts
         processes or memory.
       * MANUAL CLEANUP -- the operator has to `pgrep`/`kill` by hand, which
         is exactly the job they delegated to the supervisor.

     A score of 0 is not "nice tidiness"; anything above 0 means the tool
     cannot be trusted to stop what it started.

6.0.1  DEFINITION
     A service forks children -- the normal case for shell wrappers, worker
     pools, `npm start`, anything that spawns helpers. Stop the service by the
     tool's OWN documented stop command. Count surviving processes.

     Child shapes in the test service, all three at once:
       1. a plain background child            (sleep &)
       2. a grandchild under a shell          (sh -c 'sleep' &)
       3. a double-forked child in its own session (setsid sh -c 'sleep' &)
     Shape 3 is the hard case: it deliberately escapes the process group.

6.1  RESULT
     TOOL                          procs before    surviving after stop
     sysg                                6                 0
     Docker Compose                      6                 0
     Supervisor, tuned                   6                 2
       (stopasgroup=true, killasgroup=true)
     Supervisor, DEFAULT config          6                 5

     THE TUNED ROW WAS ADDED AFTER REVIEW. The first version of this metric
     compared a default-configured Supervisor against sysg and reported 5
     leaked. Codex correctly flagged that as unfair: Supervisor HAS relevant
     settings and they were not enabled. With process-group stop turned on it
     leaks 2, not 5. The honest margin is 0 vs 2.

     The 2 survivors are the `setsid` double-fork and its child -- a process
     that left the group cannot be reached by killasgroup, by construction.

     Supervisor's survivors, all reparented to PID 1:
        80  1  sleep 3600
        81  1  sh -c sleep 3600
        82  1  sh -c sleep 3600
        83 81  sleep 3600
        84 82  sleep 3600

6.2  READING
     * sysg and Docker Compose score 0. Supervisor scores 2 at its best
       setting and 5 at defaults.
     * Supervisor's remaining leak is not a tuning oversight, it is a
       ceiling: `killasgroup` signals the process GROUP, and the escaped
       session is no longer in it. Closing that requires session or cgroup
       teardown, which Supervisor does not have.
     * Quote this as "0 vs 2 against a tuned Supervisor", never "0 vs 5".
     * These two 0s are NOT the same achievement. Compose gets it free from
       the kernel: stopping a container tears down the whole PID namespace,
       so nothing inside can survive regardless of how it forked. sysg has no
       namespace to lean on and has to do it deliberately, via session and
       provenance-based teardown. Same result, and Compose's is structurally
       harder to get wrong.
     * sysg cleaned up ALL descendants including the setsid double-fork --
       the shape Codex predicted it would miss. It did not miss it.
     * Supervisor signalled only the pid it spawned. Every child, grandchild
       and the escaped session survived and were reparented to init.
     * This is the metric with the clearest operational consequence: under
       Supervisor, a service that spawns workers leaks them on every stop and
       restart until the box runs out of processes. Under sysg it does not.
     * This is a capability gap, not a tuning difference. Supervisor has no
       process-tree or session teardown to configure.

6.3  GAPS
     * Default stop configuration for both tools; no TERM-grace/KILL-deadline
       sweep. Codex asks for survivors counted after TERM and again after
       KILL, and for the strongest natively-configurable setting as a second
       row. Not yet done.
     * One trial. The result is categorical (0 vs 5) rather than marginal, so
       repetition is lower priority here than elsewhere.
```

## 7. Control-plane crash durability

```text
Collected:  2026-08-20

7.0  DEFINITION
     `kill -9` the supervisor itself, then restart it. Check three things:
       (a) do the services survive while the control plane is dead?
       (b) on restart, is the running service RE-ADOPTED, DUPLICATED, or lost?
       (c) is the reported PID/status truthful afterwards?
     Codex's rule: duplicate or false ownership is a HARD failure -- it means
     two copies of a service competing for the same resource.

7.1  RESULT -- Docker Compose   (control plane = dockerd, NOT the compose CLI)
     CONFIGURATION, WHICH DOMINATES THIS RESULT:
        live-restore = false   <- Docker's DEFAULT
        container restart policy = no
     Method: SIGKILL to dockerd (pid 300) inside the Docker Desktop VM.

     services survive control-plane death:  NOT OBSERVABLE, then NO
        The workload could not even be INSPECTED while dockerd was down --
        every observation path (docker exec, docker run --pid=host, docker ps)
        goes through the daemon that was killed. Losing the control plane
        means losing visibility of the workload, not just control of it.
     dockerd self-recovery:                 NONE after 200 s. Docker Desktop
        did not restart it; the app had to be relaunched manually.
     after recovery:                        ALL CONTAINERS `exited`
        Not re-adopted, not duplicated -- terminated. With live-restore
        false, dockerd does not re-attach to containers from a previous
        session. With restart policy `no`, nothing brought them back.
     operator action required:              YES -- manual restart of the app
        AND manual `docker start` of every container.

     THIS IS CONFIGURATION-DEPENDENT AND MUST BE QUOTED AS SUCH.
     `live-restore: true` exists precisely for this scenario and would very
     likely change the outcome. It is NOT the default, and was not enabled
     here. A fair headline is "Docker Compose in its default configuration",
     never "Docker Compose".

     Collateral note, recorded because it is the honest cost of this test:
     five unrelated containers on the test machine (Grafana, 2x Prometheus,
     2x postgres-exporter) were also terminated and required manual restart.
     That is the blast radius of a control-plane crash under these defaults.

7.2  RESULT -- Supervisor
     services survive supervisor death:  YES (reparented to init)
     after supervisord restart:          2 PROCESSES -- DUPLICATE STARTED
     Supervisor has no record of the surviving child, so it starts a second
     copy. Hard failure by the above rule.

7.3  RESULT -- sysg   [RESOLVED -- earlier ambiguity was a harness fault]
     services survive control-plane death:  YES (reparented to init)
     after restart, health_check ABSENT:    pids 77, 79 -- UNCHANGED
     after restart, health_check PRESENT:   pids 77, 79 -- UNCHANGED
     No duplicate in either configuration; the same processes are re-adopted.

     An earlier run appeared to show a pid change (75 -> 138) with three
     matching processes. That was a harness fault -- the `pgrep -f` pattern
     also matched the measuring shell's own command line, and a concurrent
     `sysg logs` invocation added another. Isolating health_check as the only
     variable shows identical behaviour both ways. Recorded because the wrong
     reading would have been a false negative AGAINST sysg.

7.4  SCORECARD

     dimension                        sysg      Supervisor   Compose(default)
     workload survives CP death       YES       YES          NO
     workload observable during       n/a       n/a          NO
     duplicate started on recovery    NO        YES          NO
     workload lost on recovery        NO        NO           YES
     automatic recovery               YES       YES          NO (manual x2)

     Reading: Supervisor and Compose fail this metric in OPPOSITE ways.
     Supervisor keeps the workload alive but loses track of it and starts a
     duplicate -- two copies fighting over one resource. Compose never
     duplicates, because it kills everything instead. sysg is the only one of
     the three that both kept the workload alive and did not duplicate it,
     though its health-check case is still unresolved.

7.5  REMAINING GAPS
     * Log-pipe reattachment and resumption of recurring health probes after
       cold adoption are NOT verified. Codex flags both as likely weak. A
       service can be re-adopted by pid yet have lost its log stream.
     * LIVE-RESTORE TRUE IS UNTESTED, and this is the largest single hole in
       the dataset. Attempted via Docker-in-Docker and ABANDONED as invalid:
       in DinD, `dockerd` IS the container's main process, so killing it
       terminates the whole DinD container and measures nothing. Codex had
       already warned that DinD is not equivalent to a host daemon.
       Testing it properly requires editing daemon.json on a real host and
       restarting Docker twice, which was not done here.
       EXPECTED (unverified): live-restore exists precisely for this case and
       should let containers survive a daemon restart, which would move
       Compose from "loses everything" to something closer to sysg's row.
       Until measured, the Compose result in 7.1 must be quoted as
       "in Docker's DEFAULT configuration" and never as "Docker Compose".
     * One trial per tool per configuration.

     NOTE ON PROCESS IDENTITY: sysg's tracked pid is its wrapper shell
     (`sh -c /h/svc2.sh`), not the service (`/bin/sh /h/svc2.sh`). Any
     adoption logic keyed on the wrapper inherits the wrapper problem
     recorded in Section 5.4.
```

## 8. Readiness semantics

```text
Collected:  2026-08-21

8.0  WHAT THIS MEASURES
     A service is started but is not USABLE for 5 seconds -- it has to load,
     migrate, warm a cache, whatever. The question is what the tool tells you
     during those 5 seconds.

     This is the metric behind most "it says it's up but the site is down"
     incidents, and behind broken dependency ordering: if A reports ready
     before it can serve, everything gated on A starts against a dead service.

     LIE WINDOW = (time service is actually usable) - (time tool reports up).
     Positive means the tool claimed readiness it did not have.

8.1  RESULT
     TOOL                          reports up  usable   LIE WINDOW
     sysg (health probe)              5.64 s   5.64 s     0.00 s
     Docker Compose (healthcheck)     5.73 s   5.21 s    -0.52 s (conservative)
     Supervisor startsecs=1 (default) 1.22 s   5.10 s    +3.88 s
     Supervisor startsecs=5 (TUNED)   5.22 s   5.22 s    +0.00 s  <- ties sysg
     Supervisor startsecs=5, service
       actually takes 8 s               5.19 s   8.12 s    +2.93 s

     THE TUNED ROWS WERE ADDED AFTER REVIEW, for the same reason as metric 6:
     the first version compared a configured sysg against a default
     Supervisor. Tuned properly, Supervisor TIES sysg -- as long as the
     startup time is a known constant.

8.2  READING
     * THE REAL DIFFERENCE IS OPEN-LOOP vs CLOSED-LOOP, not accuracy.
       `startsecs` is a fixed timer. Set it to the service's actual startup
       time and Supervisor is exactly as accurate as sysg (+0.00 s). But the
       moment startup time VARIES -- cold cache, contended DB, larger
       dataset, slower disk -- the timer is wrong by exactly the variance:
       an 8 s start under startsecs=5 lies by +2.93 s. A probe closes the
       loop and is correct in both cases without being told anything.
     * So the honest claim is NOT "Supervisor lies". It is: Supervisor can
       be configured to be accurate for a PREDICTABLE service and cannot be
       accurate for a VARIABLE one. Calling its default output a "lie" was
       mislabelling -- RUNNING is an accurate liveness report; it is just not
       a readiness report.
     * sysg reports up exactly when the service becomes usable. It gates on
       an actual health probe, so there is no window in which status is
       wrong.
     * Compose is CONSERVATIVE rather than accurate: it declared ready 0.52 s
       AFTER the service was usable, because its healthcheck polls on a 1 s
       interval and the probe landed late. Erring late is the safe direction;
       it is not a lie, it is latency. sysg has the same 1 s polling floor and
       happened to land tighter on this run.
     * DIRECTION MATTERS MORE THAN MAGNITUDE. A negative window costs you
       time; a positive window costs you an outage.

8.3  A REAL FINDING ABOUT SYSG'S DEFAULTS
     The first attempt at this test FAILED the service outright:
        error[SG0104]: service `w` failed to become healthy
        the health check ... reported the service is not healthy after 3
        attempts
     sysg's DEFAULT health-check budget is 3 attempts. With a 1 s interval,
     any service needing more than ~3 s to become healthy is torn down at
     boot unless `retries`/`total_timeout` are raised. The 5.64 s figure above
     required `retries: 30, total_timeout: "60s"`.

     This is defensible fail-closed behaviour and the diagnostic is good --
     it explicitly says "the process is running but never answered the health
     check". But the default is tight for real services (JVMs, migrations,
     anything touching a network) and is worth revisiting. Notably it is
     ALSO the reason sysg cannot lie here: it would rather kill a service
     than report it healthy on faith.

8.4  GAPS
     * One trial per tool. The gaps are large relative to noise, but the
       Compose -0.52 s figure is within its own 1 s poll interval and should
       not be read as precise.
     * All three tools were given a 1 s probe interval, which is sysg's
       floor. Compose can poll faster; that would tighten its window.
     * Only one readiness shape (a file appearing). A TCP-listen or HTTP-200
       probe may behave differently, particularly for Supervisor, which has
       no probe mechanism at all and would score the same on any shape.
```

## 9+. Metrics not yet collected

```text
Each becomes a full section using the template above when collected.

  5   BOOT / COLD START (single-service, no graph)
      Definition: already-installed tool, config already on disk, time from
      process exec to ALL services reporting healthy via the tool's own status.
      Rows: 1, 5, and 40 services. Distinguish first-boot from restart.
      Docker note: must state whether images are pre-pulled. Pulling is not
      boot, but a first-run user pays it -- report both.

  6   IDLE RESIDENT MEMORY
      Definition: RSS of the supervisor and every helper it keeps alive, at
      rest, supervising N = 1, 10, 40 trivial services. Docker must include
      dockerd + containerd + shims; on macOS it must include the VM.

  7   PROCESS COUNT AT REST
      Definition: processes attributable to the tool while idle. sysg should
      win outright here; measure it rather than asserting it.

  8   IDLE CPU
      Definition: mean CPU% over 60 s at rest supervising 10 services.
      Watch for polling loops -- this is where a poll-based supervisor loses.

  9   UNINSTALL
      Definition: time to remove, plus a diff of files/dirs/units/sockets left
      behind afterwards. This is a sysg strength; it needs the residue diff to
      be credible, including the shell-rc PATH line sysg appends.

  10  CONFIG SIZE
      Definition: bytes and lines to express one identical 5-service stack
      (deps, restart policy, env, logs) in each tool. Publish all four configs
      verbatim so the comparison can be argued with.

  11  RUNTIME DEPENDENCY COUNT
      Definition: what must already exist on the box. sysg: nothing.
      Supervisor: a Python runtime. Compose: an engine + containerd + runc.
      systemd: is the box.

  12  TIME TO FIRST DIAGNOSIS
      Definition: seconds from a deliberately broken service to an operator
      having the actual cause on screen. Scenarios: bad binary path, port
      already bound, permission denied, OOM. This is the metric sysg is
      designed to win; measure it honestly or it is worthless as a claim.
```

## Open items and caveats

```text
  * Linux sysg is 1.5-2.0x macOS. Vendored OpenSSL is the hypothesis, NOT yet
    confirmed. Test: build the musl target with reqwest swapped for a rustls
    client and re-measure.
  * "Installed-Size" from apt is the packager's own figure. For systemd it was
    cross-checked against a direct sum of dpkg -L files (13.56 vs 12.88 MB);
    they agree within ~5%. Other packages were not cross-checked.
  * Docker Engine numbers are the Debian packages. Other distros differ.
  * Docker Desktop's 2.24 GB includes a Linux VM image and the Electron UI.
    It is the honest number for a macOS developer, but it is not a
    like-for-like comparison against a single Linux binary.
  * Supervisor is compared with its own venv. A distro package
    (apt install supervisor) would share more with the system Python.
  * README.md currently claims "Rootless: ~12 MB executable". That is the
    macOS figure. Linux is 17.7-23.6 MB. The row needs correcting.
```

