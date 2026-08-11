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
| System mode | ✓ systemd unit | ✓ (incl. Alpine OpenRC docs) | ✓ launchd, `/Library/Application Support/systemg`, `/Library/Logs/systemg` |
| Container-init (PID 1) | ✓ | ✓ primary audience | SG0711 refusal |
| `no_new_privs` + seccomp | ✓ (well below 5.10) | ✓ (raw prctl/seccomp syscalls; audit-arch per ISA) | SG0721 refusal |
| Landlock | ✓ 5.13+, recommend 5.15 LTS; fail closed below | same | SG0721 refusal |
| Kernel-assisted (Aya eBPF) | ✓ 5.10 LTS + kernel BTF; CAP_BPF+CAP_PERFMON (CAP_SYS_ADMIN fallback) | ✓ loader is libc-independent; caveats are BTF availability/memlock, not musl | SG0731 refusal |

Feature-specific kernel floors, detected independently: system mode never
implies eBPF availability.

## 6. Design by track

### 6.1 System-mode parity (Phase 1a)

- Fix `scripts/install-systemg.sh` systemd unit: `ExecStop` must pass `--sys`;
  revisit `Type=` vs `--daemonize`.
- Root-without-`--sys` becomes typed diagnostic SG0701 with the exact retry
  command, replacing the log warning at `src/bin/main.rs:977`.
- `--sys` stays explicit. Detection of system-mode state while in user mode (and
  vice versa) produces SG0702 guidance, never silent retargeting.
- Parameterize mode-neutral UAT scenarios to run in both modes; keep documented
  mode-specific lanes.

### 6.2 Container-init (Phase 1b)

- PID 1 duties: reap *all* adopted orphans (today nobody does), install
  TERM/INT/CHLD dispositions, forward signals, shutdown in reverse topological
  order, then exit with a meaningful status.
- Same-PID re-exec works without a parent (live upgrade already execs in
  place); raw `fork()` after thread creation stays forbidden (the recycle-wedge
  lesson).
- Prerequisite probes (`/proc` mounted, controlling env) fail with SG0712, not
  undefined behavior.

### 6.3 Kernel-enforced sandboxing (Phase 3)

- `isolation.seccomp` and `no_new_privs` become enforced; configuration that
  cannot be enforced refuses to start the service (fail closed, SG0722/SG0723).
- Landlock filesystem rules follow; unavailable or insufficient Landlock ABI
  under an explicit config fails closed with SG0724.
- `no_new_privs` is mandatory whenever seccomp or Landlock is configured.
- Fail-closed applies uniformly: explicit AppArmor/SELinux/private-tmp/
  private-devices configuration that cannot be enforced refuses to start the
  service. No security key ever warns and proceeds.

### 6.4 Kernel-assisted observation (Phase 4)

- Aya (pure Rust; no libbpf C toolchain; musl-safe) over libbpf-rs and the
  lossy, config-dependent netlink proc connector.
- Tracepoints: `sched_process_exit`, `sched_process_fork`, `sched_process_exec`
  → ringbuf → supervisor event loop. Replaces polling *latency*, not
  `waitpid` semantics, and never becomes the source of truth for reaping.
- Config: `observation: auto | required | off`. `auto` degrades to polling
  with SG0732 logged once; `required` fails start with SG0731; event loss
  (ringbuf overrun) forces a `/proc` reconciliation pass and SG0733.

## 7. Diagnostics: SG07xx family

| Code | Meaning |
|---|---|
| SG0701 | Running as root without `--sys`; state would land in user paths (exact retry command included) |
| SG0702 | Mode/state mismatch: on-disk state belongs to the other runtime mode |
| SG0703 | Install/doctor: system-mode integration broken (unit/plist missing, wrong, or stale) |
| SG0704 | `--sys` requested without root privileges |
| SG0711 | Container-init unsupported on this platform |
| SG0712 | PID 1 prerequisites missing (e.g. `/proc` not mounted) |
| SG0713 | PID 1 shutdown incomplete: services survived reverse-order teardown |
| SG0721 | Sandbox capability unsupported on this platform |
| SG0722 | seccomp filter could not be built/applied; service refused (fail closed) |
| SG0723 | `no_new_privs` could not be set; service refused |
| SG0724 | Landlock requested but ABI unavailable/insufficient; service refused |
| SG0731 | eBPF required but unavailable (kernel/BTF/capabilities as evidence); start refused |
| SG0732 | eBPF unavailable; degraded to polling (auto mode; missing capabilities as evidence) |
| SG0733 | eBPF event loss; forced /proc reconciliation |

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
| 1a | Mode parity: ExecStop fix, SG0701/0702, parity UAT matrix, enable musl release builds + Alpine CI lanes | Parity matrix green both modes |
| 1b | Container-init: reaper, signals, shutdown, same-PID reexec | PID1 UAT green on gnu+musl |
| 2 | System integration: systemd unit hardening + socket activation, launchd + /Library paths + state migration | Boot-start UAT green; macOS native lane green |
| 3 | Sandboxing: no_new_privs + seccomp fail-closed, then Landlock | Sandbox UAT green; audit updated |
| 4 | Kernel-assisted: Aya, ringbuf events, fallback + reconciliation | eBPF UAT green incl. loss/fallback; audit updated |

CI: enable both musl release builds (currently commented out), re-enable Docker
UAT, add Alpine user/system/PID1 lanes, macOS parity on native runners.

## 12. Decisions (formerly open questions)

- systemd unit runs sysg foreground under `Type=notify` with `sd_notify`; no
  daemonization under an init system.
- `observation: auto | required | off` is supervisor-level config, default
  `auto`.
- SG0732 (degraded observation) surfaces in logs *and* status metadata.
- Container-init ships as `sysg init`: requires actually being PID 1, implies
  system mode, and routes all wait statuses through a centralized wait broker
  (managed children to their monitors, adopted orphans reaped).
- `no_new_privs` is set whenever seccomp or Landlock is configured.
- Manifest schema shapes for `isolation.landlock:` and `observation:` freeze
  before owner approval of this RFC.

## 13. Amendments

- 2026-08-10: Initial draft frozen with Codex agreement (platform matrix,
  Aya choice, fail-closed semantics, priority D→A→C→B).
- 2026-08-10: Codex review round applied — SG07xx decades accepted (SG0704
  added, SG0734 folded into SG0731/0732 evidence), uniform fail-closed for all
  security keys, `sysg init` + wait broker, `Type=notify` foreground, test
  renames + fault-injection lanes.
