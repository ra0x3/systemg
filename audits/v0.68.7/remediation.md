# Audit Remediation — v0.68.7

- **Scope**: approved remediation of F01–F08 in [report.md](report.md); candidate findings C01–C05 and observations H01–H04 are not independently closed by this work.
- **Baseline**: `1f84289` (`docs(audit): record v0.68.7 security review`), containing the review of code commit `7e6eb578cb5cf93785890b203882728d91207d4b`.
- **Date**: 2026-10-21.
- **Authorization**: owner approved the eight-finding implementation plan, individual finding commits, and an audit-type rebase/squash. Tests, lint, builds, and check scripts remain deferred.
- **Maintainer review**: owner functionality confirmation and release sign-off pending.
- **Source reviewers**: Codex main agent with independent environment/privilege, logging, and lifecycle source-review passes. This is not an external audit.
- **External reviewer**: not supplied; still required for a root-facing release candidate.
- **Commit convention**: title only, `audit: remediate v0.68.7 security findings`.

## Executive summary

This is a remediation supplement, not a replacement for the immutable audit report. The original advisory evidence, prerequisites, severity qualifications, and untested exploit-reliability assessments remain in that report.

The implemented changes remove process-global environment mutation during loading, correct root config ownership checks, bound captured output before queuing, refuse unenforced namespace requests, require Landlock truncation protection, and retain lifecycle ownership across dynamic teardown and same-key cron recovery.

PID isolation is **refused**, not implemented: a service requesting `isolation.pid: true` cannot start under either schema. Namespace failure handling and the Landlock minimum enforcement requirement also deliberately tighten compatibility. These changes were explicitly approved in the implementation plan.

No runtime or build result is claimed. Source-review agreement does not establish that the changes compile, pass the repository checks, or behave correctly on a deployed kernel.

## Findings and coverage

| Finding | Approved invariant and remediation | Disposition |
|---|---|---|
| F01 | Manifest resolution uses an ambient snapshot and separate project/service maps. Structural YAML interpolation and command/redaction provenance share the same scope. Env assignments are validated before child/helper application; dynamic children's explicit assignments come only from their authorizing unit. | Implemented and source-reviewed; runtime validation pending. |
| F02 | Trust-validated supervisor config/include opens require a regular file owned by the effective supervisor UID or root, without a root-caller exemption. `O_NOFOLLOW | O_NONBLOCK` preserves descriptor validation while avoiding a FIFO-open wait. | Implemented and source-reviewed; privileged filesystem validation pending. |
| F03 | Partial lines are capped at ingress, canonical queues have count and shared byte limits, and handoff preserves bounded state. Pause acknowledgements are generation-bound; failed flush/pause attempts cancel within the handoff deadline. | Implemented and source-reviewed; saturation and handoff validation pending. |
| F04 | Every requested namespace transition must succeed before exec, including under schema 2 and in restricted containers. | Implemented and source-reviewed; Linux denial-path validation pending. |
| F05 | Unsupported PID isolation is rejected before fork and defensively before exec; the misleading success report is removed. | Mitigated by explicit refusal; actual PID namespace support remains unavailable. |
| F06 | Landlock requires ABI 3 or later, handles `TRUNCATE`, grants it only on writable paths, and refuses incomplete enforcement. Existing ABI 1 rights are retained without newly granting `REFER`. | Implemented and source-reviewed; kernel/DAC matrix validation pending. |
| F07 | Active and terminating dynamic generations retain authoritative ownership until verified cleanup. Detached children retain waiter/TTL obligations without automatic termination. Upgrade cannot overlook pending teardown; persistence retirement is generation-bound. | Implemented and source-reviewed; interruption and identity validation pending. |
| F08 | Recovered unfinished runs are claimed before new due admission. Persisted deadlines are preserved, worker loss releases only the claim, and unverified identities cannot authorize signals. | Implemented and source-reviewed for same-key recovery; cold-restart/fault-injection validation pending. |

## Implementation notes and mitigation limits

### F01 — Configuration and child environments

The loader no longer installs env-file or inline assignments in the supervisor process. Rejected manifests cannot leave those assignments behind. Project-level fields cannot consume a sibling service's variables; service order cannot determine another service's environment. Substitutions modify parsed YAML values rather than introducing new YAML syntax, and duplicate expanded keys are refused.

The shared env resolver rejects empty names, embedded NULs, malformed assignment lines, and incomplete double quotes without printing secret values. Launch/helper rereads use that resolver and fail before applying a partial set of assignments. Empty quoted values remain valid. A service env file overrides the project env file, and inline values override file values. Existing service inheritance policy remains; a privilege-dropped dynamic child still starts clean and receives a safe default PATH unless the unit supplies one explicitly.

This does not make an operator-authorized root helper safe when its own declared environment is attacker-writable. It prevents unrelated service declarations or failed loads from modifying ambient supervisor authority. Configured helper privileges, ambient operator environment trust, and env-file filesystem ancestry retain the audit's stated limitations.

### F02 — Config descriptor trust

Root supervisors now require root-owned executable manifest fragments. Non-root supervisors may load files owned by their effective UID or root. Group/other-write rejection and same-descriptor parsing remain in force. Nonblocking open permits rejection of a FIFO before waiting for a writer.

This is not an ACL, ancestor-directory, mount-topology, or concurrent-write audit. It does not close candidate C04's runtime-directory ownership question by itself.

### F03 — Output retention and handoff

Each captured line retains a 16 KiB prefix and a saturating count of discarded bytes. The same policy covers managed/legacy capture, forwarded stderr, pre-start output, and dynamic-child console echo. Dynamic raw log files continue receiving streamed bytes without line buffering.

Each canonical writer admits at most 64 queued messages. An 8 MiB shared reservation budget covers queued/in-flight line allocations, including an overhead charge. This is **not** an 8 MiB cap on total supervisor memory: separately bounded partial buffers, read chunks, pending producer messages, channel storage, thread stacks, and existing caches scale with configured stream/writer count.

Backpressure may slow a child's writes when a log destination stalls. A blocked capture path can cause live upgrade to be declined; cancellation resumes capture rather than losing its partial state. Helper-wide output-drain deadlines in H04 are not independently fixed.

### F04–F06 — Confinement compatibility

Schema 2 no longer silently ignores failed namespace creation. Operators must grant the necessary kernel authority or remove the requested isolation knowingly. `isolation.pid: true` is unsupported under both schemas; removing the success log alone would not have repaired that boundary, so startup is refused.

Landlock policies now require ABI 3 or later, normally Linux 6.2+ or a compatible backport. A newer kernel is not sufficient unless the relevant access rights are requested; the fix explicitly handles truncation and requires enforcement. Landlock does not revoke access through descriptors opened before confinement, and candidate C05's path-resolution question remains open. This work does not implement the complete dynamic-child sandbox plan described in H01.

### F07 — Dynamic teardown

Subtree and service-generation removal transfer children into retained pending cleanup under the same ownership lock used by the upgrade count. A worker claims work without consuming it; failed worker creation or unwind releases the claim for retry. Active entries are counted until completion is recorded, and pending entries always block unsupported handoff.

Admission rechecks the authenticated parent's kernel identity and registration after child creation. Cleanup retains a child handle when a waiter cannot be installed, preserves the ordinary waiter's completion marker, and avoids stealing its exit status. Signaling checks captured PID/start pairs; cleanup requires known targets and the original group/session to be empty before retiring ownership. If ancestry cannot be verified after the anchor exits, ownership remains pending rather than signaling unrelated processes or reporting success.

Durable dynamic rows carry a registration-generation string with full timestamp precision and a local sequence number. Completion/removal matches that string, avoiding the existing whole-second `SystemTime` serialization ambiguity. Missing, legacy, or superseding rows are not deleted by an old generation. Filesystem failure retains the cleanup obligation for retry.

Orphan/reparent policy is captured during authorization, so ordinary exit after detachment does not become an implicit cascade. Detached live children remain owned and block live upgrade until their obligations settle. Explicit TTL can still promote their retained subtree to termination; promotion follows registration tokens, not numeric PID ancestry. This is more conservative than transferring those children across reexec; no such transfer protocol is implemented.

This does not recover every dynamic child after a full supervisor crash, make numeric signaling atomic, or confine processes that escape the known session. A failed TTL timer before cleanup begins is not made durable by the pending-cleanup registry. Unverifiable survivors can require operator reconciliation.

### F08 — Recovered cron ownership

Recovery claims unfinished runs before admitting new due work. The claim has an exact in-memory run timestamp and an unwind-safe owner; failed dispatch or worker loss releases only that claim, preserving unfinished state for the next recovery pass. Reload retains the record belonging to a claimed worker, including a provisional launch without a PID. Completion changes the record before releasing its claim, closing the duplicate-owner interval.

New runs carry the PID and kernel birth captured at registration through startup hooks. Their PID, birth, and timeout deadline are published to cron state together. The deadline begins when normal timeout supervision begins, preserving the previous treatment of pre-start/onstart duration; this work does not impose a new hook-wide timeout. Recovered records retain that absolute deadline rather than receiving another full timeout. Legacy records derive a deadline from their saved start and current timeout configuration. An explicit unlimited-run marker distinguishes new unlimited runs from legacy records.

Recovered survivors may not be waitable children. `ECHILD` alone must not mean completion: identity-aware polling continues until the original process is gone or verified teardown succeeds. An unavailable exit status is interruption, never invented success. A live PID without a saved birth is retained conservatively without signaling; it can block the schedule until it disappears or an operator reconciles state. A provisional record can retain an attributable service PID without adopting its current birth as signaling authority.

Once a known-birth run's deadline has expired, recovery checks generation/group/session emptiness before accepting a mailbox outcome, a changed PID birth, or a vanished leader as terminal. This preserves the cleanup obligation if a timeout worker dies after killing only the leader. Unanchored surviving groups remain owned but are not signaled without a verified identity; this can require operator reconciliation. Ordinary numeric signaling and session-escape limits still apply.

Fresh and recovered workers share completion ordering. Transient wait/cleanup failure leaves the run unfinished for retry. PID retirement compares both PID and birth after durable reload. Lifecycle updates check the in-memory PID/birth and the existing durable PID supersession guard; this is not a new cross-file atomic generation transaction. Cron completion is recorded before failure hooks execute. Persistence retains the existing warning-on-failure policy, so disk failure or a crash between exec and state publication still prevents a universal exactly-once guarantee. In particular, interruption during onstart before cron identity publication can leave only an unattributed live PID: recovery blocks overlap but cannot safely enforce its timeout. The reaper mailbox's historical numeric-identity limitation is not independently closed.

Claims are keyed by the persistent `v{schema}:{project}:{service}` identity, not the changing command/configuration hash. Ordinary same-key resync is covered. Migrating an active job between schema/project/service keys is not a transactional ownership transfer; operators must quiesce active cron runs before that migration. This work does not claim cross-key deduplication or state migration.

## Verification record

### Source evidence

These references identify the remediation implementation, not a reproduced exploit. The original report retains baseline vulnerability evidence.

| Finding | Implementation anchors |
|---|---|
| F01 | `src/config/mod.rs:1725` (`EnvConfig::resolve`), `:2208` (scoped interpolation), `:2281` (validated env-file parsing), `:2710` (manifest scope expansion). |
| F02 | `src/runtime.rs:358` (effective-owner check), `:387` (nonblocking no-follow open and same-descriptor validation). |
| F03 | `src/logs.rs:2155` (bounded partial lines), `:2243` (shared decoder), `:2276` (bounded writer transport), `:2344` (deadline-bound flush), `:2857` (handoff preparation/cancellation). |
| F04 | `src/privilege.rs:627` (namespace failure propagation). |
| F05 | `src/privilege.rs:224` (pre-fork refusal), `:605` (defensive child refusal). |
| F06 | `src/sandbox.rs:138` (ABI floor), `:163` (truncate rights and strict compatibility). |
| F07 | `src/spawn.rs:331` (upgrade ownership count), `:351` (retained transfer/token promotion), `:443` (retryable claims), `:501` (durable generation identity), `:656` (admission revalidation); `src/daemon.rs:1344` (generation-matched retirement), `:4827` (identity-checked teardown); `src/supervisor.rs:6507` (cleanup completion). |
| F08 | `src/cron.rs:492` (RAII owner), `:928` (recovery claims), `:1294` (deadline preservation), `:1321` (atomic launch publication); `src/daemon.rs:1145` (durable PID/birth clear), `:9156` (pre-hook launch identity); `src/supervisor.rs:3689` (recovery-before-admission), `:6907` (shared completion), `:6972` (identity/deadline polling and cleanup). |

### Activities

| Activity | Result |
|---|---|
| Source review and cross-path tracing | Completed for F01–F08, including independent review and cross-finding integration review; execution remains unverified. |
| Individual finding commits | F01–F08 created separately; the eight-commit series is retained locally on `audit/finding-fixes`. |
| Rebase/squash | Only the remediation series above `1f84289` is rebased/squashed into the title-only audit commit named above. The original report commit remains separate. |
| Build / unit / integration tests | Not run, per owner constraint. |
| Formatter / lint / `scripts/commit-check.sh` | Not run, per owner constraint; passing status is not claimed. |
| `cargo audit`, `cargo deny`, `cargo vet` | Not run. |
| Fuzzing / fault injection | Not run; no new duration, execution, or crash totals. |
| Privileged kernel or filesystem reproduction | Not run. |

## Unsafe inventory delta

The historical inventory and audit report remain unchanged. Production config-loader environment mutation was removed. Cron recovery adds numeric-process liveness inspection; those checks do not grant signaling authority without a saved birth identity. The unsafe inventory has not been regenerated, and no updated repository-wide unsafe count is asserted.

## Dependency review delta

No dependencies or lockfile entries were added or changed for these fixes. Landlock uses the already pinned crate; the change is to requested rights and compatibility policy. Existing cargo-vet exemptions and the need for independent root-TCB review remain.

## Mode / kernel test matrix

| Surface | Source review | Execution |
|---|---|---|
| Linux user / root `--sys` | Shared config, IPC-launched children, log capture, lifecycle and cron paths | Not run |
| Linux restricted container | Namespace failure and fail-closed paths | Not run |
| Linux Landlock ABI <3 / >=3, glibc / musl | Minimum ABI, handled rights, error propagation | Not run |
| Linux PID1 | Shared spawn cleanup and reaper boundaries; live upgrade remains prohibited | Not run |
| macOS user / system | Shared Unix config, logging, upgrade, cleanup and cron branches | Not built or run |
| Crash, panic, failed worker creation, failed exec, partial persistence | Source-level retained-state and cleanup-order review | No fault injection |

## Known limitations carried forward

- C01–C05 and H01–H04 retain their original classifications unless a particular overlapping change is explicitly described above. This supplement does not silently promote or close them.
- Numeric PID checks followed by signaling are not atomic pidfd-based operations. Historical K2 and documented session escape remain; no claim of complete process-tree confinement is made.
- Filesystem or machine failure can prevent durable registration or completion. These changes do not create a transaction spanning child exec and every state file.
- Missing identity must fail conservatively rather than authorize signals against a recycled PID. Such cases can require operator reconciliation and can block scheduling or upgrade.
- No tests, lint, build, fault injection, or supported-kernel matrix was executed. The repository's required checks and functionality confirmation remain outstanding.
- Parallel AI source review is not an external reviewer or owner release approval.

## Concise follow-up plan

1. Obtain owner functionality confirmation and authorization for validation.
2. Exercise malformed/rejected env loads, root foreign-owned/FIFO includes, hostile output and blocked writers, namespace denial, PID refusal, and Landlock truncation.
3. Exercise overlapping teardown/upgrade, failed waiter creation, TTL/cascade/detachment, durable-write failure, cron survivor recovery, stale/missing identities, worker failure, and deadline preservation.
4. Run the repository checks and supported-platform/kernel matrix, refresh release evidence, and obtain external review plus owner disposition of remaining candidates/observations.

## Release decision

**BLOCK — validation and owner sign-off pending.** Remediation source work is not a signed release decision. Owner: not signed. External review: pending.
