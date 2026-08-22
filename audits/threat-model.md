# Threat Model

Living document for kernel mode. Updated as capabilities land; per-RC audit
reports snapshot the version they audited.

## Trust boundary

Trusted: the manifest (operator-controlled — its commands are intended
execution, not vulns) and a same-UID (or root) socket peer. Everything else
is untrusted: socket frame contents, service names, config paths supplied
over IPC, filesystem state in service-writable directories, child process
output, and — in system mode — every non-root local user.

## Assets

1. Root execution context of the system-mode supervisor
2. Per-service privilege boundaries (dropped UID/GID, caps, namespaces)
3. Runtime state integrity (`/var/lib/systemg`, pid/state/cron XML)
4. Service log confidentiality and integrity
5. Liveness: the supervisor's ability to observe and control its services

## Attack surfaces (system mode)

| Surface | Attacker | Vector | Standing mitigations |
|---|---|---|---|
| Control socket | local non-root user | connect, forge frames | 0700 dir + 0600 socket; SO_PEERCRED/getpeereid on every accept; reject before read |
| IPC frame decode | authenticated-but-malicious same-UID process; compromised admin tool | oversized/malformed/deeply-nested JSON | 1 MiB frame cap; typed Serde enum; fuzz target (`fuzz/fuzz_targets/ipc_frame.rs`) |
| Manifest + !include parse | operator typo → confused deputy; service-writable include path | YAML expansion, include cycles, TOCTOU swap | O_NOFOLLOW + fstat-same-fd parse; [SG0207](/reference/dialog/codes#sg0207)–0209 include validation; YAML parser migrated to maintained `serde_yaml_ng` |
| Privilege drop ordering | misordered syscalls re-grant authority | groups/caps retained across setuid | ordered transaction (unshare→rlimit→caps-pre→setgroups→setgid→setuid→caps-post); always-reset supplementary groups; env_clear on switch |
| FD inheritance | dropped child inherits supervisor FDs | leaked socket/log/lock FDs | FD_CLOEXEC default; handoff FDs cleared briefly and restored |
| Signals/PID identity | PID reuse → signal wrong process | stale pid.xml, recycled PID | PID+start-time identity; SID-scoped teardown; provenance ledger; no command-string inference (locked invariant) |
| Log paths | socket-supplied service name | path traversal to root-owned file read/create | validated service names; confined log resolution (fixed 2026-07-13 P0) |
| Live upgrade handoff | crafted snapshot/state | schema confusion across versions | version+protocol probe; refusal on mismatch ([SG0502](/reference/dialog/codes#sg0502)); forbidden entirely as container PID 1 (red-team decision) |

## Kernel-mode deltas (added as phases land)

- **Phase 1a (parity)**: root-without-`--sys` misdirection becomes a typed
  warning ([SG0701](/reference/dialog/codes#sg0701)) and a typed refusal when system-mode
  state exists ([SG0702](/reference/dialog/codes#sg0702)); system/user state cross-targeting is a correctness
  *and* privilege issue since user-writable state must never steer a root
  supervisor.
- **Phase 1b (container-init)**: PID 1 inherits every orphan on the host
  namespace's behalf; wait-status routing must not let an adopted process
  impersonate a managed service exit. Live upgrade as PID 1 is forbidden per
  the red-team review (RFC §6.2 callout); owner decision pending.
- **Phase 3 (sandboxing)**: seccomp/Landlock configs are security promises;
  fail-closed is the enforcement of that promise. Filter construction from
  manifest input must be total (no partial filters).
- **Phase 4 (observation, if approved over pidfd baseline)**: eBPF loader and
  aya dependency tree enter the root TCB; maps stay FD-owned/unpinned; event
  data is advisory (reconciliation cross-checks /proc) so a spoofed/lost
  event can degrade freshness, never truth.

## Non-goals

Defending against a hostile root, a hostile manifest author, or kernel bugs.
Seccomp/AppArmor/SELinux enforcement gaps are typed refusals, not silent
acceptance.
