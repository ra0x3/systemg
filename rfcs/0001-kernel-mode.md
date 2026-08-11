# RFC 0001: Kernel Mode

- **Status**: Draft — design agreed 2026-08-10; owner approval pending
- **Owners**: ra0x3 (owner), Claude + Codex (design)
- **Target platforms**: linux-gnu (x86_64/aarch64), linux-musl (x86_64/aarch64), macOS (x86_64/arm64)

## 1. Summary

sysg today is a first-class *user-mode* supervisor with a second-class root mode
(`--sys`). This RFC makes system-level operation a headline capability under the
name **kernel mode**, defined precisely so the claim stays honest. It covers
four tracks: system-mode parity, container-init (PID 1), kernel-enforced
sandboxing, and kernel-assisted observation (eBPF). Security is the governing
constraint: every phase gates on auditable evidence, because an exploitable
kernel mode ruins the product.

> [!CAUTION]
> **Red-team verdict (Codex, 2026-08-11): the RFC as written overreaches.**
> An independent Codex red-team session recommended rejecting the full scope
> in favor of a Linux-only "system-mode hardening" v1: parity fixes, SG070x
> diagnostics, musl + privileged CI lanes, pidfd-based exit detection, and a
> deprecation window for security keys — deferring PID 1 and launchd, and
> cutting eBPF. Specific objections appear as callouts in the sections they
> attack. Owner decision on scope reduction is pending.

## 2. Terminology

The supervisor is and remains a userspace program. "Kernel mode" is the
product-level name for the family of capabilities below — it never means sysg
code executes in ring 0, and marketing must never claim that.

| Term | Meaning | What executes where |
|---|---|---|
| User mode | Default. Per-user supervisor, `~/.local/share/systemg` | Userspace, caller's UID |
| System mode | `--sys`. Root supervisor, `/var/lib/systemg`, `/etc/systemg`, `/var/log/systemg`. Historically "kernel mode" in the test harness | Userspace, root, drops per-service |
| Container-init mode | sysg as PID 1 inside a container | Userspace, PID 1 duties |
| Kernel-assisted mode | Optional Linux eBPF process-event observation | eBPF programs verified and executed by the kernel; loader in userspace |
| Kernel-enforced sandboxing | seccomp, `no_new_privs`, Landlock applied per service | Kernel enforces; sysg configures |

**Mode parity contract**: identical commands, identical output shapes, and
identical SG diagnostics wherever a capability is supported; a typed refusal —
never a warning that proceeds — where it is not.

> [!WARNING]
> **Red-team objection (naming, Codex 2026-08-11):** "kernel mode" only stays
> honest with a permanent disclaimer that no sysg code runs in ring 0 — and a
> name that requires a disclaimer is a bad name. Alternative: brand the family
> "system mode" and reserve kernel language for the eBPF/sandboxing tracks.

## 3. Motivation

- The `--sys` path exists, is exercised by `tests/docker/kernel/`, and is
  shipped by `scripts/install-systemg.sh` — but it is documented only as
  Privileged Mode (not headline-complete), its systemd unit has an `ExecStop`
  bug (missing `--sys`), and root-without-`--sys` fails only when system state
  already exists; otherwise it warns and proceeds into user paths.
- Alpine/GNU containers routinely need tini/s6/supervisord; sysg already owns
  lifecycle, logs, and diagnostics but has no supported PID 1 duties.
- `isolation.seccomp` / `apparmor_profile` config keys exist but warn and run
  the service unenforced (`src/privilege.rs:454`) — a silent security no-op.
- Direct-child exit detection polls `try_wait`; `kill(pid, 0)` polling serves
  stop and reconciliation. The kernel can push these events.

> [!CAUTION]
> **Red-team objection (foundation, Codex 2026-08-11):** severe lifecycle bugs
> were fixed weeks before this RFC — stop killing siblings (`628ca59`), status
> lying (`c562fd1`), restart scope (`fc2fed1`), the Aug 5 teardown/orphan
> rewrite. Privileged UAT is disabled in CI (`ci.yaml:344`) and a lifecycle
> test is ignored. Granting this codebase root powers multiplies the blast
> radius of exactly these bug classes. Mitigation demanded: green privileged
> CI and a soak period as hard gates before any root-facing phase ships.
>
> **Red-team objection (demand):** no user request for sysg-as-PID1 or any
> kernel-mode capability exists in the repo; README and philosophy explicitly
> disclaim PID 1 and machine management. The motivation above is
> capability-driven, not demand-driven.

## 4. Goals / non-goals

**Goals**
- System mode as a seamless peer of user mode on all three target platforms.
- sysg as container PID 1 on linux-gnu and linux-musl (Alpine first-class).
- Fail-closed sandboxing: seccomp + `no_new_privs` (then Landlock) on Linux.
- Kernel-assisted exit/fork/exec observation via Aya eBPF, with a polling
  fallback in `auto` mode; eBPF supplements `waitpid`/reaping, never replaces
  it.
- Per-release security audit records in `audits/`, linked from docs.

**Non-goals**
- sysg as full-OS PID 1 / bare-metal init (no VM/host init replacement).
- Any in-kernel sysg component beyond verifier-checked eBPF programs.
- macOS Endpoint Security framework, container-init, eBPF, seccomp, Landlock.
- Platforms beyond the three release targets.
- Auto-selecting `--sys` from EUID or on-disk state (state-dependent magic;
  typed diagnostic instead).

## 5. Capability matrix

| Capability | linux-gnu | linux-musl | macOS |
|---|---|---|---|
| System mode | ✓ systemd unit | ✓ (incl. Alpine OpenRC docs) | ✓ launchd, `/Library/Application Support/systemg`, `/Library/Logs/systemg` — see red-team callout below |
| Container-init (PID 1) | ✓ | ✓ primary audience | [SG0711](/how-it-works/dialog/codes#sg0711) refusal |
| `no_new_privs` + seccomp | ✓ (well below 5.10) | ✓ (raw prctl/seccomp syscalls; audit-arch per ISA) | [SG0721](/how-it-works/dialog/codes#sg0721) refusal |
| Landlock | ✓ 5.13+, recommend 5.15 LTS; fail closed below | same | [SG0721](/how-it-works/dialog/codes#sg0721) refusal |
| Kernel-assisted (Aya eBPF) | ✓ 5.10 LTS + kernel BTF; CAP_BPF+CAP_PERFMON (CAP_SYS_ADMIN fallback) | ✓ loader is libc-independent; caveats are BTF availability/memlock, not musl | [SG0731](/how-it-works/dialog/codes#sg0731) refusal |

Feature-specific kernel floors, detected independently: system mode never
implies eBPF availability.

> [!WARNING]
> **Red-team objection (macOS, Codex 2026-08-11):** launchd *is* the system
> supervisor on macOS — sysg under launchd is double supervision. Foreground
> bootstrap is coherent, but KeepAlive interaction, stop semantics, and state
> migration are unspecified here, and parity marketing overpromises what this
> platform can honestly deliver. Red team recommends deferring macOS system
> mode until the Linux story has soaked.

## 6. Design by track

### 6.1 System-mode parity (Phase 1a)

- Fix `scripts/install-systemg.sh` systemd unit: `ExecStop` must pass `--sys`;
  revisit `Type=` vs `--daemonize`.
- Root-without-`--sys` becomes typed diagnostic [SG0701](/how-it-works/dialog/codes#sg0701) with the exact retry
  command, replacing the log warning at `src/bin/main.rs:977`.
- `--sys` stays explicit. Detection of system-mode state while in user mode (and
  vice versa) produces [SG0702](/how-it-works/dialog/codes#sg0702) guidance, never silent retargeting.
- Parameterize mode-neutral UAT scenarios to run in both modes; keep documented
  mode-specific lanes.

> [!WARNING]
> **Red-team objection (ops UX, Codex 2026-08-11):** system-mode socket auth
> is supervisor-UID-or-root, so even `sysg status` needs sudo. The RFC is
> silent on authorization granularity (e.g. a read-only group for
> status/logs), which pushes operators toward running everything as root.
> Phase 1a must either add a read-only access story or document sudo-only
> operation as an explicit, accepted cost.

### 6.2 Container-init (Phase 1b)

- PID 1 duties: reap *all* adopted orphans (today nobody does), install
  TERM/INT/CHLD dispositions, forward signals, shutdown in reverse topological
  order, then exit with a meaningful status.
- Same-PID re-exec works without a parent (live upgrade already execs in
  place); raw `fork()` after thread creation stays forbidden (the recycle-wedge
  lesson).
- Prerequisite probes (`/proc` mounted, controlling env) fail with [SG0712](/how-it-works/dialog/codes#sg0712), not
  undefined behavior.

> [!CAUTION]
> **Red-team objection (PID1 reexec, Codex 2026-08-11):** a failed `execv`
> during live upgrade is recoverable for a normal supervisor
> (`src/supervisor.rs:3717`), but a post-exec restore failure as container
> PID 1 exits the init process and kills the container and every service in
> it. Live upgrade must be *forbidden* in container-init mode, not designed
> around. Also: PID 1 demand is unproven (see §3) — red team ranks this track
> third of four by value/risk and recommends deferring it.

### 6.3 Kernel-enforced sandboxing (Phase 3)

- `isolation.seccomp` and `no_new_privs` become enforced; configuration that
  cannot be enforced refuses to start the service (fail closed, [SG0722](/how-it-works/dialog/codes#sg0722)/[SG0723](/how-it-works/dialog/codes#sg0723)).
- Landlock filesystem rules follow; unavailable or insufficient Landlock ABI
  under an explicit config fails closed with [SG0724](/how-it-works/dialog/codes#sg0724).
- `no_new_privs` is mandatory whenever seccomp or Landlock is configured.
- Fail-closed applies uniformly: explicit AppArmor/SELinux/private-tmp/
  private-devices configuration that cannot be enforced refuses to start the
  service. No security key ever warns and proceeds.

> [!WARNING]
> **Red-team objection (compatibility, Codex 2026-08-11):** flipping
> warn-and-run keys to fail-closed breaks manifests that run today, with no
> schema bump, key inventory, grace window, or rollback story in this RFC.
> Required before Phase 3: a deprecation release that warns loudly with the
> future refusal date, then a manifest schema bump when refusal lands.

### 6.4 Kernel-assisted observation (Phase 4)

- Aya (pure Rust; no libbpf C toolchain; musl-safe) over libbpf-rs and the
  lossy, config-dependent netlink proc connector.
- Tracepoints: `sched_process_exit`, `sched_process_fork`, `sched_process_exec`
  → ringbuf → supervisor event loop. Replaces polling *latency*, not
  `waitpid` semantics, and never becomes the source of truth for reaping.
- Config: `observation: auto | required | off`. `auto` degrades to polling
  with [SG0732](/how-it-works/dialog/codes#sg0732) logged once; `required` fails start with [SG0731](/how-it-works/dialog/codes#sg0731); event loss
  (ringbuf overrun) forces a `/proc` reconciliation pass and [SG0733](/how-it-works/dialog/codes#sg0733).

> [!CAUTION]
> **Red-team objection (eBPF ROI, Codex 2026-08-11):** the red team ranks this
> track last by value/risk and recommends *cutting it from v1*. The design
> hole: `pidfd_open(2)` + poll (Linux 5.3+) gives event-driven exit detection
> with no BTF requirement, no CAP_BPF/CAP_PERFMON, and no aya dependency tree
> inside a root supervisor's TCB — solving the 2s-poll latency this track
> exists to remove. eBPF's residual value (fork/exec lineage of
> non-descendants) does not justify enlarging the root attack surface. If
> Phase 4 survives, it must first justify itself against a pidfd baseline.

## 7. Diagnostics: SG07xx family

| Code | Meaning |
|---|---|
| [SG0701](/how-it-works/dialog/codes#sg0701) | Running as root without `--sys`; state would land in user paths (exact retry command included) |
| [SG0702](/how-it-works/dialog/codes#sg0702) | Mode/state mismatch: on-disk state belongs to the other runtime mode |
| [SG0703](/how-it-works/dialog/codes#sg0703) | Install/doctor: system-mode integration broken (unit/plist missing, wrong, or stale) |
| [SG0704](/how-it-works/dialog/codes#sg0704) | `--sys` requested without root privileges |
| [SG0711](/how-it-works/dialog/codes#sg0711) | Container-init unsupported on this platform |
| [SG0712](/how-it-works/dialog/codes#sg0712) | PID 1 prerequisites missing (e.g. `/proc` not mounted) |
| [SG0713](/how-it-works/dialog/codes#sg0713) | PID 1 shutdown incomplete: services survived reverse-order teardown |
| [SG0721](/how-it-works/dialog/codes#sg0721) | Sandbox capability unsupported on this platform |
| [SG0722](/how-it-works/dialog/codes#sg0722) | seccomp filter could not be built/applied; service refused (fail closed) |
| [SG0723](/how-it-works/dialog/codes#sg0723) | `no_new_privs` could not be set; service refused |
| [SG0724](/how-it-works/dialog/codes#sg0724) | Landlock requested but ABI unavailable/insufficient; service refused |
| [SG0731](/how-it-works/dialog/codes#sg0731) | eBPF required but unavailable (kernel/BTF/capabilities as evidence); start refused |
| [SG0732](/how-it-works/dialog/codes#sg0732) | eBPF unavailable; degraded to polling (auto mode; missing capabilities as evidence) |
| [SG0733](/how-it-works/dialog/codes#sg0733) | eBPF event loss; forced /proc reconciliation |

Codes are stable, get permanent docs anchors, and join `SgCode::ALL`.

## 8. Use-case tests (names bound the scope)

Hyphenated, matching `tests/docker/` directory conventions. Landlock and
event-loss lanes use fault injection.

Parity lanes (each runs user mode + system mode):

- `parity-start-status-stop`
- `parity-restart-reconcile`
- `parity-logs-follow-and-purge`
- `parity-cron-lifecycle`
- `parity-inspect-json`

System mode:

- `sys-multiuser-service-drop` (postgres/redis users from one manifest)
- `sys-execstop-systemd-unit`
- `sys-boot-start-via-unit`
- `sys-openrc-alpine-integration`
- `sys-root-without-sys-sg0701`
- `sys-mode-state-mismatch-sg0702`
- `sys-nonroot-sys-refusal-sg0704`
- `sys-macos-launchd-paths` (native runner)
- `sys-macos-refusals-typed` (native runner)

Container-init (gnu + musl images):

- `pid1-reaps-adopted-orphans`
- `pid1-wait-status-routing-echild`
- `pid1-sigterm-reverse-order-shutdown`
- `pid1-signal-forwarding`
- `pid1-zombie-none-after-churn`
- `pid1-shutdown-survivors-sg0713`
- `pid1-same-pid-reexec-upgrade`
- `pid1-alpine-musl-entrypoint`
- `pid1-missing-proc-sg0712`
- `pid1-init-refused-when-not-pid1`

Sandbox:

- `sandbox-seccomp-denies-syscall`
- `sandbox-seccomp-apply-failure-fail-closed-sg0722`
- `sandbox-no-new-privs-blocks-setuid-exec-elevation`
- `sandbox-landlock-denies-fs-escape`
- `sandbox-landlock-abi-unavailable-sg0724` (fault-injected)
- `sandbox-audit-arch-x86-64-vs-aarch64`

Kernel-assisted:

- `ebpf-exit-wakes-monitor`
- `ebpf-fork-exec-descendant-tracking`
- `ebpf-pid-namespace-cgroup-scoping`
- `ebpf-auto-fallback-polling-sg0732`
- `ebpf-required-refusal-sg0731`
- `ebpf-event-loss-reconcile-sg0733` (fault-injected)
- `ebpf-survives-supervisor-reexec`
- `ebpf-musl-static-binary-load`

## 9. Security program

- Threat model: root supervisor's IPC socket, manifest parsing, privilege-drop
  ordering, FD handoff, and the eBPF loader are the primary surfaces. eBPF
  maps stay FD-owned and unpinned — no `/sys/fs/bpf` state to defend. Past prod bug classes (identity inference from command
  text, stale-state trust, fork-after-threads) are re-audited on every kill
  and spawn path.
- `audits/README.md` is the living policy and index. Each release gets an
  immutable `audits/v0.x.y/report.md` + `evidence/`: scope, tag/commit, date,
  auditor, tool versions, findings/dispositions, unsafe inventory (grep count
  today: 186 sites including tests; 61 in privilege/ipc/daemon), dependency
  review, fuzz
  duration/executions/crashes, mode/kernel test matrix, release decision. A
  clean release still ships a report.
- Tooling: cargo audit (exists) + cargo deny + cargo vet (aya and friends),
  IPC frame-decoder fuzzing, unsafe-block review.
- Docs `kernel-mode/security.mdx` links the latest report; `docs/security.mdx`
  stays the canonical trust model.

> [!CAUTION]
> **Red-team objection (audit economics, Codex 2026-08-11):** per-release
> self-audit by the maintainer risks assurance theater. Our own `reports/`
> found unauthenticated root control (2026-06-11) and root path traversal
> (2026-07-13) — and ~29 releases have shipped since July 13. At that
> cadence, per-release reports either become rubber stamps or destroy
> velocity. Red-team alternative: external audit at RC milestones for
> root-facing phases + a soak period, with self-audit evidence reserved for
> the RC reports rather than every patch release.

## 10. Docs plan

New top-level Mintlify nav group `docs/kernel-mode/`:

`index.mdx` (terminology + parity contract) · `system-mode.mdx` ·
`container-init.mdx` · `kernel-assisted.mdx` · `sandboxing.mdx` ·
`security.mdx`. Redirect `how-it-works/privileged-mode` →
`kernel-mode/system-mode`.

## 11. Phases

| Phase | Content | Gate |
|---|---|---|
| 0 | Threat model, audits/ scaffolding, cargo deny/vet, IPC fuzzing, unsafe review | First audit report lands |
| 1a | Mode parity: ExecStop fix, [SG0701](/how-it-works/dialog/codes#sg0701)/0702, parity UAT matrix, enable musl release builds + Alpine CI lanes | Parity matrix green both modes |
| 1b | Container-init: reaper, signals, shutdown, same-PID reexec | PID1 UAT green on gnu+musl |
| 2 | System integration: systemd unit hardening + socket activation, launchd + /Library paths + state migration | Boot-start UAT green; macOS native lane green |
| 3 | Sandboxing: no_new_privs + seccomp fail-closed, then Landlock | Sandbox UAT green; audit updated |
| 4 | Kernel-assisted: Aya, ringbuf events, fallback + reconciliation | eBPF UAT green incl. loss/fallback; audit updated |

CI: enable both musl release builds (currently commented out), re-enable Docker
UAT, add Alpine user/system/PID1 lanes, macOS parity on native runners.

> [!CAUTION]
> **Red-team reduced scope (Codex 2026-08-11):** defensible v1 = Phases 0–1a
> only, Linux-only, plus pidfd exit detection and the sandbox deprecation
> release. Defer 1b (PID1) and the macOS half of 2; cut 4 (eBPF) pending a
> pidfd baseline comparison. Hard gates regardless of scope decision: green
> privileged CI, the ignored lifecycle test re-enabled, and external audit at
> RC for any root-facing release.

## 12. Decisions (formerly open questions)

- systemd unit runs sysg foreground under `Type=notify` with `sd_notify`; no
  daemonization under an init system.
- `observation: auto | required | off` is supervisor-level config, default
  `auto`.
- [SG0732](/how-it-works/dialog/codes#sg0732) (degraded observation) surfaces in logs *and* status metadata.
- Container-init ships as `sysg init`: requires actually being PID 1, implies
  system mode, and routes all wait statuses through a centralized wait broker
  (managed children to their monitors, adopted orphans reaped).
- `no_new_privs` is set whenever seccomp or Landlock is configured.
- Manifest schema shapes for `isolation.landlock:` and `observation:` freeze
  before owner approval of this RFC.

## 13. Amendments

- 2026-08-10: Initial draft frozen with Codex agreement (platform matrix,
  Aya choice, fail-closed semantics, priority D→A→C→B).
- 2026-08-10: Codex review round applied — SG07xx decades accepted ([SG0704](/how-it-works/dialog/codes#sg0704)
  added, [SG0734](/how-it-works/dialog/codes#sg0734) folded into [SG0731](/how-it-works/dialog/codes#sg0731)/0732 evidence), uniform fail-closed for all
  security keys, `sysg init` + wait broker, `Type=notify` foreground, test
  renames + fault-injection lanes.
- 2026-08-11: Independent Codex red-team review embedded as callouts
  (foundation stability, demand evidence, pidfd-vs-eBPF, audit economics,
  fail-closed migration, PID1 reexec, macOS double supervision, naming, ops
  UX, reduced v1 scope). Owner decision on scope reduction pending.
