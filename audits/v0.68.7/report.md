# Audit Report — v0.68.7 source snapshot

- **Scope**: SystemG's root-facing control plane, persistent supervisor state, lifecycle and recovery paths, privilege transitions, confinement, filesystem inputs, logging, cron, and live upgrade. Includes the applicable threat-model phases 0–3 and the implemented observation baseline.
- **Package version / git description**: 0.68.7 / v0.68.6-4-g7e6eb57. No v0.68.7 tag was established by this review.
- **Commit**: 7e6eb578cb5cf93785890b203882728d91207d4b.
- **Date**: 2026-10-21.
- **Auditor**: Codex, with parallel source reviews of lifecycle/recovery, privilege/filesystem boundaries, and logging/cron/upgrade. The environment and log-buffer findings received an additional adversarial source review.
- **Maintainer reviewer**: Pending owner review.
- **External reviewer**: Not performed. Parallel AI reviews do not satisfy the independent external-review requirement.
- **Status**: Requested snapshot analysis, pending owner disposition; not an approved release-candidate audit or release certification. The versioned directory identifies the reviewed snapshot and does not change the policy that patch releases inherit an RC audit.
- **Authorization**: Source analysis was read-only. The subsequent instruction authorized creation of this document. No application code, historical report, test, configuration, or audit-policy file was changed; no tests, lint, builds, fuzzing, or exploit demonstrations were run.

## Executive summary

The review identifies **eight primary findings, F01–F08**. They are confirmed by source analysis under the stated prerequisites; none was demonstrated against a running supervisor. Five additional security hypotheses remain candidates, C01–C05. Four narrower hardening/recovery observations are recorded separately as H01–H04.

The most consequential confirmed paths let service-writable configuration data cross into root authority: service env files mutate the supervisor's persistent environment before validation completes, and the include loader accepts foreign-owned fragments when the supervisor is root. Both need a trusted operator manifest to reference an attacker-writable resource; neither establishes an unrestricted default-installation exploit.

A compromised lower-privilege service can also grow the supervisor's log buffers without bound. Other confirmed defects concern namespace and Landlock promises, upgrade interrupting unfinished dynamic-child cleanup, and cron recovery restoring a running flag without restoring timeout/completion ownership.

No new unauthenticated control-socket authorization bypass was established. Same-UID/root authentication remains a significant boundary. Historical findings are not counted again: in particular, the accepted numeric PID/PGID signal race is carried forward rather than presented as new. The new persistent-identity candidates concern different producers and later consumers.

**Recommendation: BLOCK root-facing release approval** until the confirmed privilege-boundary and enforcement defects have owner-approved dispositions, appropriate validation has been performed, and the required external review is complete. This is an auditor recommendation, not an owner signature or an action on a release.

## Method, evidence, and classification

The review began with AGENTS.md, followed by audits/README.md, audits/TEMPLATE.md, audits/threat-model.md, audits/unsafe-inventory.md, reports/2026-06-11.md, reports/2026-07-13.md, and reports/CVE.md. Those reports remain immutable history.

Every source location below refers to the commit above. Novel means not already described by those reports; introduction dates and earlier affected releases were not bisected. The initial worktree was clean. Evidence consists of source reads, read-only searches, Git history/diffs, a comparable lexical unsafe census, dependency-manifest inspection, and primary API/kernel documentation. No generated execution evidence is attached because none was produced.

- **Confirmed**: the relevant defect and complete conditional path follow from the inspected source. This does not mean runtime reproduction or measured exploit reliability.
- **Candidate**: a concrete suspicious path exists, but practical reachability, a deployment assumption, or a consequential security outcome remains unvalidated.
- **Rejected**: a proposed new finding was contradicted, already disclosed, outside the stated trust model, or lacked a distinct security consequence. Rejection is scoped to the hypothesis, not a guarantee that the subsystem is bug-free.

Severity is qualitative and conditional. No CVSS score, CVE assignment, universal exploit claim, or host-wide compromise claim is made. All primary findings remain open: no fix, accepted-risk decision, issue creation, or deferral was authorized. The template's final disposition and issue-link fields are therefore pending rather than fabricated.

## Tool versions

| Tool / evidence | Version / environment | Result |
|---|---|---|
| Git | 2.50.1 (Apple Git-155) | Read-only status, history, and source/dependency comparisons |
| ripgrep | 15.2.0, rev e89fff89ac | Source and policy inventory |
| Review host | Darwin 25.2.0, arm64 | Source inspection only; not a tested target |
| cargo audit | Not invoked; version not collected | No fresh advisory-database result |
| cargo deny | Not invoked; version not collected | Policy inspected, not executed |
| cargo vet | Not invoked; version not collected | Local audits/exemptions inspected, not verified |
| cargo geiger | Not invoked | Lexical comparison only |
| fuzz (ipc_frame) | Not invoked | Session duration 0; executions 0; crash/triage result not assessed |
| Tests / lint / builds / privileged UAT | Not invoked | Explicitly excluded by authorization |

## Coverage matrix

Coverage here means each scoped surface was traced or given an explicit limitation/disposition. It does not assert exhaustive path exploration or runtime verification of every function.

| Surface / root-capable path | Input → retained state / crossing → later consumer | Evidence and disposition |
|---|---|---|
| Socket admission | Local connection → accepted stream → credential check before command read | ipc.rs:522–529; supervisor.rs:2788–2855. Unauthorized peer command execution rejected as a new hypothesis. A worker is allocated before authentication; authenticated connection/queue budgets remain limited only indirectly. |
| IPC decoding and queues | Authenticated frame → capped buffer / typed command → request worker and mutation queue | ipc.rs:941–969; supervisor.rs:2956–2985,3538. Historical unbounded individual frame fixed. No connection read deadline or bounded worker/mutation admission established; same-UID trust limits severity. |
| Start / AddProject / Restart | Authorized config path → parsed project/config/env → registration, reconcile, child launch | supervisor.rs:1657–1697,4491–4620,5182–5497. F01/F02; validation does not roll back global env side effects. |
| Stop / StopProject / Shutdown | Authorized selector/cancellation → current and retired generations → cleanup and runtime unlink | supervisor.rs:2861–2884,4545–4582,6371–6479; ipc.rs:1161–1170. Historical signal TOCTOU retained; C03 covers a separate stale-supervisor authorization path. |
| Spawn | Authenticated parent/name/argv/TTL → tree and child records → privilege application, waiter, timer, cascade | supervisor.rs:4761–5171; spawn.rs:268–272,538–596. Empty argv rejected and privilege plan applied. F07 and H01; PID-only dynamic ancestry is a remaining identity limitation. |
| Status / Inspect / DeclaringProjects / Version / CurrentOp | Authenticated selectors → snapshots/operation state → read responses | supervisor.rs:3005–3030,4621–4681,4715–4733. No independent cross-privilege execution path established. Cached status is not proof of current kernel identity. |
| Logs / ClearLogs / BootStream / OpStream | Authenticated selector/watch ID → log targets, buffers, journals → streams/truncation/progress | supervisor.rs:2897–2935,3113–3163,4254–4484,4683–4692. Existing traversal finding not recounted; F03 concerns child-output ingress. Journal leases seal normal completion/failure at supervisor.rs:331–357. |
| Manifest and includes | Trusted manifest plus filesystem fragments → same-FD YAML and include graph → executable service config | runtime.rs:333–392; config/include.rs:130–195. F02. Cycle/depth/cumulative-byte checks exist; blocking FIFO open remains a related availability edge. |
| Env loading / hooks / health / pre-start | Service env file → process-global environment → rejected reload or later privileged helper | config/mod.rs:2247–2261,2573–2611,2719–2746; daemon.rs:3022–3054,7932–7939,8613–8619. F01; H04 covers helper output-drain deadlines. |
| UID/GID and groups | Trusted identity plan → pre-exec switch → service exec | privilege.rs:355–365,480–505. Supplementary groups reset; no new arbitrary group-retention finding established. Environment isolation is not equivalent to supervisor-global isolation. |
| Capabilities | Capability declaration → sets modified before UID transition → exec capability calculation | privilege.rs:510–575,726–738. H02; default root execution is not itself a privilege escalation. |
| Namespaces | Isolation declaration → unshare result / namespace-for-children state → payload exec | privilege.rs:585–640; daemon.rs:5889–5903. F04/F05. Non-Linux requested namespace isolation is refused at privilege.rs:624–637. |
| Seccomp / Landlock | Security declaration and filesystem paths → prepared filter/FD rules → post-drop enforcement | sandbox.rs:51–101,105–204,286–391. F06, C05, H01. Unsupported profiles and non-enforcement results are refused in the configured launch path. |
| Cgroups / resource ceilings | Manifest limits → parent-created group/control FD → child joins before exec | privilege.rs:355–358,375–389,643–685,794–837. Old post-spawn attachment-window hypothesis rejected for this path. Service ceilings do not bound supervisor log allocations. |
| Inherited FDs | Activation/control/log/report descriptors → CLOEXEC or temporary handoff state → exec | runtime.rs:468–530; daemon.rs:151–228; supervisor.rs:3370–3378,3411–3416,3468–3472. No new default cross-UID FD leak established; upgrade exception and residual read-worker caveat below. |
| Signals and process identity | PID/start/SID/PGID records → exit/reuse/retirement → stop/reap/adoption | daemon.rs:994–1135,4535–4541,4700–4803,5356–5405. C02/C03; accepted historical K2 remains. Existing pidfds provide readiness, not complete signal identity binding. |
| Container PID 1 | Adopted orphan exit → PID-keyed mailbox → managed/helper waiter | reaper.rs:38–79,112–165; daemon.rs:3501–3508. C02. Live upgrade refusal present at supervisor.rs:3985–3990. |
| Live upgrade / rollback | Authorized replacement path → probe, quiesced state, inheritable FDs → exec/resume/rollback | upgrade.rs:231–269,403–487; supervisor.rs:3262–3472,3985–4019. C01/F07. Version/protocol checks, active cron/child/boot guards, and ordinary failed-exec recovery exist. |
| Cron | Due job → in-flight/completion claim and persisted run → timeout, exit, cold recovery, next boundary | cron.rs:515–550,743–754,892–953; supervisor.rs:2182–2186,3683–3850. F08. Claim withdrawal and failure completion cover ordinary spawn errors, not recovered ownership. |
| Persistent state / interruption | Runtime XML/JSON → atomic write or retained live handle → load, repair, adoption | runtime.rs:215–252; daemon.rs:808–839,893–906. Ordinary partial-write replacement preserves old bytes; fresh corrupt PID state fails closed. H03 is the migration exception. |
| Root user/system mode / registry replay | Inherited HOME / mode selection → runtime and loose registry → cold project restore | runtime.rs:51–70,195–203; loose_registry.rs:84–122; supervisor.rs:2635–2716. C04. |
| Boot installation / purge / migration | Operator account/project IDs and state → directory handles/staging → ownership, removal, publication | boot.rs:774–805,829–976; main.rs:4781–4824,5265–5275. Descriptor-relative boot preparation and purge component validation reject tested-in-source traversal hypotheses; H03 covers torn migration publication. |
| Observation / eBPF phase | Kernel process observations → metrics/status → reconciliation | daemon.rs:3457–3470,10329–10341; Cargo.toml. pidfd observation exists. No aya dependency or implemented eBPF loader was found; future Phase 4 is not certified. |

## Findings

| ID | Classification | Severity | Finding | Disposition |
|---|---|---|---|---|
| F01 | Confirmed, source | High, conditional | Service env files poison persistent supervisor environment before validation | Open; owner disposition pending |
| F02 | Confirmed, source | High, conditional | Root include validation accepts attacker-owned executable fragments | Open; owner disposition pending |
| F03 | Confirmed, source | Medium | Child output grows supervisor capture buffers and writer queues without bound | Open; owner disposition pending |
| F04 | Confirmed, source | Medium | Namespace creation failures are accepted despite fail-closed configuration | Open; owner disposition pending |
| F05 | Confirmed, source | Medium | PID namespace setup does not place the directly executed service inside it | Open; owner disposition pending |
| F06 | Confirmed, source | Medium | Landlock read-only policy omits truncation restrictions | Open; owner disposition pending |
| F07 | Confirmed ordering defect; race untested | Medium | Upgrade can interrupt dynamic-child cleanup after ownership is erased | Open; owner disposition pending |
| F08 | Confirmed, source | Medium | Cold cron recovery restores running state without timeout/completion ownership | Open; owner disposition pending |

### F01 — Persistent environment poisoning across service and reload boundaries

**CWE**: CWE-15, External Control of System or Configuration Setting; CWE-454, External Initialization of Trusted Variables. **Affected**: root system-mode or root user-mode supervisors on Unix; the concrete LD_PRELOAD escalation applies to Linux dynamically linked children outside secure-execution mode. Ordinary user mode permits same-authority cross-service contamination. macOS loader-specific escalation was not verified.

**Prerequisites**: An operator-controlled manifest references an env file/path writable by a compromised lower-privilege service or another local attacker. A load/reload occurs, followed by a privileged subprocess that consumes a poisoned variable. The attacker does not need to edit the top-level manifest or authenticate to the root socket.

**Trace**: attacker-controlled service env file → loader reads assignments → assignments are installed in the root supervisor's global environment → later expansion/validation returns an error, or loading succeeds → the continuing supervisor launches an unrelated root helper/service → the child consumes PATH or loader variables with root authority.

**Evidence**:

- src/config/mod.rs:2247–2261 reads env files with ordinary read_to_string and calls process-global env::set_var.
- src/config/mod.rs:2719–2746 applies both project and every service's env settings globally.
- src/config/mod.rs:2573–2576 performs these side effects before expansion/parse at 2580–2586 and validation at 2609–2611.
- src/supervisor.rs:1657–1697 returns a manifest-rejected error; 4091–4103 sends the error and continues processing. No environment rollback is present.
- src/daemon.rs:3022–3027,3054 launches hooks with an inheriting shell; 5807–5818 clears a normal service environment only for an identity switch without inherit_env. Pre-start has another inheriting shell at 7932–7939.

**Impact and reliability**: Once the prerequisites hold, contamination is deterministic. Linux library-loading or PATH execution is conditional on the later command, its environment overrides, and its loader. LD_PRELOAD does not retroactively execute code inside the already-running supervisor. A rejected-load demonstration must fail after the side effects, for example through an env-supplied invalid duration; malformed first-pass YAML never reaches them. No runtime exploit was run.

**Challenges and mitigation limits**: A service's env_clear does not undo global parent mutation or sanitize unrelated root helpers. Root-owned, non-replaceable env files remove this unprivileged entry point, but do not fix cross-service contamination from accepted configuration. A variable denylist is insufficient. The same parser also panics on a lone double-quote value at src/config/mod.rs:2255–2256; invalid env keys/values can panic in set_var. Those are related interruption paths, not additional primary findings. The supervisor panic hook logs rather than contains panics at src/bin/main.rs:7824–7827.

**Fix invariant**: Loading, rejecting, or inspecting a manifest must not mutate process-global environment. Resolve variables in local validated maps and construct each child's complete environment explicitly, with separate helper authority. Rust also documents concurrent Unix environment mutation as unsafe; no memory-corruption exploit is claimed here. See [Rust set_var safety and panic contract](https://doc.rust-lang.org/std/env/fn.set_var.html).

**Novelty**: This is lower-trust service data modifying retained privileged parent state, including after rejected reload, rather than the historical disclosure of inherited root environment to a dropped child.

### F02 — Root accepts foreign-owned include fragments

**CWE**: CWE-863, Incorrect Authorization; CWE-829, Inclusion of Functionality from Untrusted Control Sphere. **Affected**: root supervisors on Linux/macOS, system or user runtime, when loading a referenced fragment through an attacker-writable path.

**Prerequisites**: A trusted manifest already includes a fragment at a path a non-root attacker can replace or edit. The attacker can supply a regular file owned by them with mode 0600, and an operator startup/reload subsequently reads it. A trusted, non-replaceable include tree is not exposed to this route.

**Trace**: attacker-owned fragment → trusted include resolver → metadata check accepts foreign ownership because caller UID is zero → parsed command/identity/security settings become retained ServiceConfig → service launch executes the resulting definition with the supervisor's configured authority.

**Evidence**:

- src/config/include.rs:145 calls open_trusted_config; 169–195 reads/parses the accepted fragment.
- src/runtime.rs:339–354 checks regular-file type and group/other write bits.
- src/runtime.rs:357–359 rejects foreign ownership only when the supervisor UID is also nonzero. Therefore a root caller accepts every file owner.
- src/config/mod.rs:2532–2535 feeds resolved includes into project loading; src/daemon.rs:5735–5756,5784–5792,5903 constructs and spawns the resulting service.

**Impact and reliability**: Conditional root execution if the included position can define a root-running service or its command/security settings. No check/use race is needed once a foreign-owned 0600 fragment is at the referenced path. The consequence is narrower if the included position cannot affect executable or privilege-bearing settings. No runtime demonstration was performed.

**Challenges and mitigation limits**: Same-FD validation and O_NOFOLLOW address inode substitution, not the incorrect ownership predicate. Group/other write restrictions do not prevent a file's owner from modifying it. This finding does not claim that a malicious operator manifest is a vulnerability.

A related availability path exists before metadata validation: src/runtime.rs:387–391 opens read-only without O_NONBLOCK before checking regular-file type. Replacing a writable include path with a FIFO can block the owner thread before rejection. Include size/depth limits do not bound this wait.

**Fix invariant**: Every executable manifest fragment must be owned by the supervisor identity or root, including when the supervisor itself is root. Establish trust and bounded regular-file access on the descriptor actually parsed; preserve the existing same-FD property.

**Novelty**: The newly introduced include surface exposes an ownership-policy bypass without granting the attacker socket access. This is not a restatement of the old unauthenticated arbitrary config-path finding.

### F03 — Unbounded service-output retention in the supervisor

**CWE**: CWE-400 / CWE-770, Uncontrolled Resource Consumption / Allocation Without Limits. **Affected**: Linux/macOS user, system, and init supervisors with service output capture; the forwarding-only stderr path is also affected.

**Prerequisites**: An attacker controls a managed child's stdout/stderr, including a child already dropped to a lower UID. No manifest edit, socket access, or service-writable configuration is required.

**Trace**: child streams bytes without a newline → parent pipe reader appends to a partial-line vector → partial bytes and a handoff copy remain retained across reads → repeated scans/clones amplify work → eventual newline, EOF, or upgrade consumes the accumulated bytes only after memory has grown → supervisor memory exhaustion and CPU amplification.

**Evidence**:

- src/daemon.rs:5889–5899 applies the child privilege/sandbox transition; 5913 and 5940–5946 hand output to parent capture workers.
- src/logs.rs:2504 appends without a length bound; 2505–2506 rescans the accumulated vector; 2523–2526 clones it into shared handoff state after every read.
- src/logs.rs:2446 creates an unbounded mpsc writer channel; 2516–2518 queues full line vectors.
- src/logs.rs:1615–1642 truncates only persisted events. It is called after dequeue at 2258–2264, too late to bound ingress or queued bytes.
- src/daemon.rs:5924–5926 uses unbounded line reads for the separate pipe_stderr path.

**Impact and reliability**: Newline-free output deterministically accumulates memory independently of disk speed. The queue variant additionally requires producers to outrun the writer. Actual exhaustion time, allocator behavior, and kernel OOM victim selection are unmeasured; host-wide OOM and code execution are not claimed.

**Challenges and mitigation limits**: The 8 KiB read chunk, 16 KiB persisted-line cap, live-cache limits, rotation, and bounded subscriber queues do not bound the producer's partial line or canonical writer queue. Child cgroup/rlimit memory ceilings do not account for these supervisor allocations. Disabling capture avoids this route only if stderr forwarding is also disabled.

**Fix invariant**: Per-stream partial bytes, all queued bytes, handoff copies, and aggregate supervisor capture memory must remain bounded before delimiter detection. Apply bounded buffering and explicit backpressure/drop policy at ingress; bound every capture mode.

**Novelty**: This crosses the child-to-supervisor boundary through output, rather than the historical authenticated IPC frame-length defect.

### F04 — Namespace denial is silently accepted

**CWE**: CWE-636, Not Failing Securely. **Affected**: Linux, all launch modes using PrivilegeContext; particularly schema v3's fail-closed promise in restricted containers or kernels that deny requested namespace creation.

**Prerequisites**: The operator requests network, mount, PID, or user isolation and unshare returns EPERM or EINVAL. A compromised service then uses authority that the requested namespace should have removed.

**Trace**: trusted isolation declaration → namespace flags retained in privilege plan → kernel rejects unshare → error is treated as success → limits/identity transition and exec continue → payload runs in the original namespaces.

**Evidence**: src/privilege.rs:595–606 builds the flags; 615–616 accepts both EPERM and EINVAL; 355–365 then continues the privilege transaction. The fail_closed field exists at 115–121 and is set at 243, but is not consulted by this failure branch. src/daemon.rs:5889–5903 executes after that transaction.

**Impact and reliability**: Deterministic continuation when either ignored error occurs. Network or process/filesystem exposure depends on the requested namespace and the payload's remaining UID/capabilities. This is not automatic root privilege acquisition. Non-Linux namespace requests are explicitly refused at src/privilege.rs:624–637 and are not included in this finding.

**Challenges and mitigation limits**: Compatibility intent in the comment at 610–614 explains the choice but does not implement a v3 exception or informed operator acceptance. A successful UID drop cannot replace requested network or mount isolation.

**Fix invariant**: A fail-closed service must not execute unless every requested namespace transition succeeds. Any legacy best-effort policy must be explicit, separately diagnosed, and excluded from the fail-closed contract.

### F05 — PID isolation applies to future children, not the service exec

**CWE**: CWE-693, Protection Mechanism Failure. **Affected**: Linux services declaring PID isolation, especially the direct argv/exec form; applies when namespace creation itself succeeds, independently of F04.

**Prerequisites**: An operator relies on isolation.pid to isolate a compromised service from processes in the supervisor's PID namespace. Remaining UID/capability checks still constrain signals and inspection.

**Trace**: pid isolation request → pre_exec unshare(CLONE_NEWPID) creates namespace-for-children state → no intervening fork places a process in it → direct exec replaces the same process → service runs in the original PID namespace despite the isolation success report.

**Evidence**: src/privilege.rs:602–603 adds CLONE_NEWPID; 615 calls unshare. src/daemon.rs:5889–5903 proceeds through sandbox application to exec without another fork. src/privilege.rs:704–710 reports the spawned service as isolated.

**Impact and reliability**: The directly executed service retains visibility and permitted interactions with the original namespace. This follows deterministically from Linux namespace semantics, but was not exercised here. In shell form, later forked descendants may enter the new namespace while the service shell remains outside; that is not equivalent to containing the service. [Linux PID namespace semantics](https://man7.org/linux/man-pages/man7/pid_namespaces.7.html) specify that unshare affects subsequent children and does not move the caller.

**Challenges and mitigation limits**: Removing the success log or checking only the unshare return value does not repair the boundary. UID separation mitigates some interactions but does not establish PID isolation.

**Fix invariant**: The payload must execute inside the requested PID namespace, with correct namespace-init, wait ownership, PID translation, and teardown. The reported identity must describe the actual payload, not merely a prepared namespace for descendants.

### F06 — Landlock read-only confinement leaves truncation unhandled

**CWE**: CWE-693 / CWE-863. **Affected**: Linux with a configured Landlock plan, including modern kernels and services also using baseline-v1 seccomp.

**Prerequisites**: A compromised confined service has ordinary DAC write permission to a file that the manifest intends to expose read-only. The operator relies on Landlock to remove that write authority.

**Trace**: read-only path declaration → ABI V1 handled-access mask retained in the prepared ruleset → successful enforcement of that limited mask → payload uses an allowed file-opening operation with truncation → file contents are erased despite the read-only policy.

**Evidence**: src/config/mod.rs:1290–1300 describes read-only/writable confinement. src/sandbox.rs:162 selects ABI::V1; 163–172 derives all handled and granted rights from it. The success check at 194–197 only establishes enforcement of the requested rights. baseline-v1 permits openat at 313.

**Impact and reliability**: A DAC-writable read-only-listed file can be truncated through openat with O_RDONLY | O_TRUNC. Without a seccomp restriction on truncate, unlisted DAC-writable files also have a direct truncation route. This is an integrity/availability failure, not arbitrary-content writing or automatic root escalation. Source and kernel-contract analysis support the path; no runtime test was run.

**Challenges and mitigation limits**: A newer kernel does not help when SystemG explicitly requests only V1 rights. FullyEnforced is relative to that mask. A read-only DAC permission independently prevents the affected mutation. Kernel documentation explains that truncation is independent of WRITE_FILE and requires the TRUNCATE right introduced in ABI3: [Landlock truncation rules](https://docs.kernel.org/userspace-api/landlock.html#truncating-files).

**Fix invariant**: The policy must handle every content-mutation right needed to enforce its promise, granting truncation only where writing is allowed. Refuse kernels unable to provide the required minimum enforcement rather than advertising complete read-only protection.

### F07 — Cleanup ownership disappears before live upgrade can safely proceed

**CWE**: CWE-664, Improper Control of a Resource Through its Lifetime; CWE-362, Race Condition. **Affected**: Linux/macOS user/system live upgrade with dynamic children; init-mode live upgrade is prohibited and excluded.

**Prerequisites**: Legitimately spawned dynamic children exist. During cascade or TTL cleanup, at least one controlled child remains alive, for example by ignoring SIGTERM. An authorized upgrade overlaps unfinished teardown. The child need not be able to issue the upgrade request itself.

**Trace**: dynamic parent exits → cascade cleanup removes both in-memory subtree records and durable PID rows → living descendants remain in the cleanup thread's local vector → upgrade checks the emptied registry and accepts → exec destroys that cleanup thread before all descendants die → replacement has no transferred dynamic ownership and survivors escape the intended cleanup. The TTL variant removes in-memory tracking before termination but retains durable rows until src/supervisor.rs:5085–5088; those rows do not recreate the interrupted timer/cleanup worker after exec.

**Evidence**:

- src/spawn.rs:538–596 removes child records, specifically 563, before returning the local list.
- src/supervisor.rs:5123 removes the cascade subtree; 5125–5132 removes durable PID rows; only then does 5138–5148 terminate descendants. The TTL variant removes records at 5065 before termination at 5077.
- src/spawn.rs:268–272 counts only remaining registered children. src/supervisor.rs:3299–3304 uses that count to reject unsafe upgrades.
- src/daemon.rs:4772–4779 sends TERM and waits before KILL at 4786, providing a nonzero overlap interval.
- src/supervisor.rs:3237–3241 quiesces project monitors, not detached dynamic waiters/timers. exec occurs at 3448.
- src/daemon.rs:5359–5403,5527–5533 captures configured service ownership, not the removed dynamic subtree; src/supervisor.rs:2342–2498 does not recreate those dynamic cleanup workers.

**Impact and reliability**: Source-confirmed ordering defect; successful exploitation is scheduling-dependent and unmeasured. Multiple surviving descendants enlarge the window. A quiet survivor need not die when its output reader closes. The result is orphaned workloads and lost cascade/TTL enforcement, not a demonstrated privilege escalation.

**Challenges and mitigation limits**: Normal active-child upgrade rejection works until cleanup erases those records. Joining project monitors does not join dynamic cleanup threads. This is distinct from the documented ability of an ordinary process to escape a session with setsid.

**Fix invariant**: Every live or terminating dynamic generation must retain authoritative ownership until verified dead or explicitly transferred. Upgrade must account for outstanding teardown operations, not just entries remaining in an active-child map. Persistence removal must follow that lifecycle decision.

### F08 — Cron recovery restores a running flag without an owner

**CWE**: CWE-664 / CWE-459, lifecycle and incomplete-cleanup failures. **Affected**: Linux/macOS ordinary user/system supervisors recovering after abnormal exit while a cron process survives. Same-container PID1 death, which ends that namespace, is not the demonstrated scenario.

**Prerequisites**: A cron run remains alive across a supervisor crash, kill, OOM, or comparable interruption and is still live at cold restart. A compromised cron can prolong its life; the interruption need not be attacker-controlled. Persisted run state must be available.

**Trace**: live cron run and incomplete record → supervisor interruption destroys completion/timeout worker → startup recognizes the PID/start identity and restores currently_running → no recovered worker or deadline is installed → scheduler keeps skipping boundaries; even after the orphan exits, the retained bool continues suppressing runs until resync/restart repairs it.

**Evidence**:

- src/supervisor.rs:2182–2186 restores CronManager state during construction.
- src/cron.rs:743–754 loads persisted jobs; 545–550 sets currently_running from the live incomplete record.
- src/supervisor.rs:869–870 skips cron in ordinary project boot.
- src/supervisor.rs:3683–3755 creates completion ownership only for newly due launches. Timeout handling belongs to that worker at 3775–3783, and completion clears state at 3847–3850.
- src/cron.rs:915–942 skips each later boundary based on currently_running without refreshing kernel liveness.
- src/daemon.rs:10090–10106,10710–10721 does not supply the missing recovered CronManager completion/timeout path.

**Impact and reliability**: The recovered run can exceed its configured timeout, then leave future executions suppressed after it exits. The static condition is deterministic once a live incomplete run is recovered; timing and platform behavior remain untested. This is availability and lifecycle enforcement loss, not arbitrary root execution.

**Challenges and mitigation limits**: PID/start validation prevents some false adoption, but does not reconstruct ownership. The live-upgrade running-cron guard at src/supervisor.rs:3287–3297 protects planned reexec, not cold recovery. Resync after the process has died can clear the flag; therefore suppression is not claimed irreversible.

**Fix invariant**: Every recovered live run must acquire exactly one completion owner and a deadline derived from its persisted start time, or be explicitly terminated and reconciled before scheduling resumes. A running flag alone cannot represent an owned execution.

## Candidate findings

These are not counted among F01–F08. Potential severities describe the consequence if the remaining assumptions are validated, not a verified exploit rating.

### C01 — Upgrade validates an executable path, then executes a replaceable object

**Potential severity**: High, conditional. **CWE**: CWE-367. **Scope**: Unix root upgrade/probe; ordinary private installation trees are not exposed by the stated route.

Trace: operator selects a legitimate root-owned candidate beneath an attacker-replaceable directory → src/upgrade.rs:403–439 canonicalizes and checks only the final file's metadata → only PathBuf is retained → attacker replaces a path component or directory entry after validation → probe at 444–448 or final exec at src/supervisor.rs:3423–3448 executes the replacement as root. Rollback also executes a recorded pathname at src/upgrade.rs:244–269.

The final-file ownership/mode check does not prove ancestor integrity. A root-owned file directly in a sticky directory is not automatically attacker-removable; the prerequisite is actual authority to replace the relevant entry/ancestor. The source-level check/use gap is clear, but no scheduled race or deployment was exercised, so practical exploitation remains candidate. No malicious manifest or root socket peer is required beyond the operator's legitimate upgrade action.

Invariant: the verified immutable executable object must be the object used for probe, commit, and rollback. Descriptor-bound execution or securely staged immutable files with protected ancestry are possible approaches. Rechecking a pathname only narrows the race.

### C02 — Adopted-orphan exit status can be consumed by a later PID generation

**Potential severity**: Medium. **CWE**: CWE-346 / CWE-664. **Scope**: Linux container-init orphan broker.

Trace: unprivileged orphan exits → src/reaper.rs:62–79 stores its status under numeric PID with claimant None → PID is reused while the entry remains → a new managed child/helper gets that PID → take/take_for returns the old status → monitor/helper acts on a false exit. Entries contain no birth identity at 38–44; nominal retention is 30 seconds at 33, with expiry only during sweep at 186–200. src/daemon.rs:3501–3508 consults the mailbox before the actual child; 10053 compares numeric ownership and 10074 reaps the process group. Helper waits consume the mailbox at 3181–3187.

Practical PID reuse within retention, process quotas, and scheduler timing remain unvalidated; large PID spaces mitigate reliability. No general arbitrary-process kill is claimed. Named claimant checks do not disambiguate generations or reject unaddressed stale entries.

Invariant: every exit delivery must be bound to an exact registered child generation; unknown orphan records must never authorize a subsequently created child's exit. Blind post-spawn PID clearing risks deleting a legitimate new exit and is not a complete fix.

### C03 — Stale supervisor PID is reauthorized from command-line appearance

**Potential severity**: Low to Medium availability, deployment-dependent. **CWE**: CWE-346 / CWE-664. **Scope**: Unix shutdown/recycle fallback, consequential when root controls multiple runtimes.

Trace: abnormal supervisor death leaves sysg.pid → numeric PID is later assigned to an unrelated session-leading sysg → fallback accepts argv plus session leadership → root signals that unrelated process/group. src/bin/main.rs:5413–5418 and 5515–5523 implement that acceptance; 5429–5456 infers supervisor identity from argv; 5538/5548 signals the group.

This is an already-stale persisted identity, not historical K2's small check-to-signal window. It also contradicts the threat model's no-command-string-inference invariant. Exploitation requires stale state, a consequential PID reuse/match, and an operator fallback; making an attacker's own argv look like sysg merely gets that attacker's process killed and is not sufficient security impact. Reliability remains unmeasured.

Invariant: socket provenance or a persisted/pinned generation and runtime identity must authorize fallback; command strings cannot substitute. pidfds must be acquired from an established identity, not used to bless an already recycled PID.

### C04 — Root with inherited non-root HOME can replay user-owned runtime state

**Potential severity**: High, conditional. **CWE**: CWE-73 / CWE-863. **Scope**: root invocation without --sys; depends on an unsafe preserved HOME/deployment context, not the normal protected system runtime.

Trace: root starts with HOME pointing to a non-root user's existing tree → src/runtime.rs:51–70 selects that runtime → create_private_dir at 195–203 changes mode but not ownership → user-owned loose_registry.json is loaded at src/loose_registry.rs:84–122 → cold replay at src/supervisor.rs:2635–2716 loads its named manifest → F02's root ownership exemption can accept attacker-owned executable configuration.

The guard at src/bin/main.rs:1091–1118 only refuses when system_mode_state_detected is true; 2516–2518 checks hardcoded Linux pid/socket paths, not all persisted system state or macOS's system location. Directory mode 0700 does not revoke the existing owner's control.

The launch-environment prerequisite and end-to-end deployment remain untested, so this is a candidate escalation chain, not a claim against default sudo behavior. Invariant: root state selection must be explicitly root-owned and securely resolved before any log/config/registry access; mode warnings and chmod alone are insufficient.

### C05 — A replaceable Landlock policy path can broaden a prepared rule

**Potential severity**: Medium confinement bypass, conditional. **CWE**: CWE-61 / CWE-863. **Scope**: Linux Landlock with an allowed path under a service-writable parent.

Trace: attacker replaces a declared allowed directory with a symlink before restart → src/sandbox.rs:107–119 opens the policy path as supervisor → retained PathFd refers to a broader target → rw rule at 147–172 authorizes that target. The inspected locked landlock 0.4.7 implementation opens PathFd with O_PATH | O_CLOEXEC and follows symlinks (crate src/fs.rs:524–533).

Post-open inode pinning does not establish that the initially resolved object was authorized. A replacement pointing at a broader tree could remove the intended Landlock restriction, but ordinary DAC still applies; no automatic root elevation is claimed. Intended symlink policy and a realistic writable-parent deployment need validation. Protected policy-path ancestry removes this entry point.

Invariant: policy resolution must not allow service-writable path components to redefine the set of objects being authorized. Define symlink semantics explicitly and bind resolution to trusted anchors, rather than merely pinning the final result.

## Additional hardening and recovery observations

These observations are deliberately separated from new demonstrated trust-boundary violations.

| ID | Classification / severity | Trace, evidence, limits, and required invariant |
|---|---|---|
| H01 | Confirmed omission; Low under the stated same-UID trust model | A sandboxed dynamic service's authorized Spawn → supervisor reconstructs PrivilegeContext at src/supervisor.rs:4845–4875 → pre_exec at 4920–4933 omits SandboxPlan → service-specific restrictions are not reapplied, so the child may lack its parent's Landlock/seccomp/no_new_privs restrictions. Restrictions already imposed on the supervisor can still be inherited. Compare configured launch at src/daemon.rs:5847–5856,5896. Linux; requires authenticated supervisor-UID/root peer, a registered tree, and socket access. This is a new sandbox consistency gap, not an unauthenticated privilege escalation; arbitrary dropped UIDs still fail socket authentication. CWE-693. Restore the complete service security plan on every derived launch. |
| H02 | Confirmed capability-order/API defects; security consequence candidate, Low | Empty caps with no UID/GID switch configured clear Effective/Permitted before Bounding at src/privilege.rs:525–531, then ignore clear errors at 726–738. Root exec may regain bounding capabilities when no_new_privs is absent; significance depends on an explicit cap-removal promise because default root workloads are intentionally root. Nonempty lists call caps::set(Bounding) at 538–545, while locked caps 0.5.6 rejects that operation (crate src/lib.rs:298–310): fail-closed launch failure, not added authority. The error helper also logs/allocates after fork, creating an untested contention/deadlock candidate. Linux; CWE-269/CWE-667 as applicable. Restore correct bounding-set operations/order, distinguish omitted from restrictive policy, and keep post-fork paths allocation/lock-free. |
| H03 | Confirmed durability/retry defect; Low, security impact candidate | Migration stages bytes, then src/bin/main.rs:4816 copies directly to the final state file → SIGKILL/ENOSPC can leave a partial target → retry at 4786–4796 treats differing bytes as newer state → 4805–4808 skips that project. Registry/publication completion can still proceed at 4606–4609. Unix root/user state migration; needs interruption or failed copy. Availability and ownership-record loss are plausible, but no privilege exploit was established. Archives preserve manual recovery. CWE-664. Publish atomically and distinguish interrupted own publication from genuinely newer state; do not mark skipped corrupt output as completed migration. |
| H04 | Confirmed unbounded join; Low security candidate | A helper/descendant retains output FDs → helper leader exits or times out → src/daemon.rs:3289–3314 joins pipe readers without a deadline → owner worker can remain blocked indefinitely. A descendant that escapes teardown can also retain pipes after timeout. Unix helper paths; an untrusted influence over executed helper resources is needed, since a hostile operator command is out of scope. No runtime hang was demonstrated. CWE-400/CWE-404. Bound the complete operation, including drain/join, and retain cleanup ownership for all helper resources. |

## Challenged and rejected hypotheses

| Hypothesis | Disposition and evidence |
|---|---|
| Non-root local peer directly executes root IPC commands | Rejected for the protected normal runtime: credential validation precedes frame decoding at src/supervisor.rs:2832–2841; ipc.rs:522–529 enforces same UID/root. This does not certify user-owned root runtime paths in C04. |
| Historical unlimited single control frame remains | Rejected: ipc.rs:963–969 caps input before decode. Aggregate thread/connection/queue consumption remains an authenticated hardening concern, distinct from a per-frame regression. |
| Ordinary active dynamic children or active cron can be live-upgraded | Rejected: supervisor.rs:3287–3304 refuses both. F07 specifically begins after dynamic cleanup has erased the records used by that guard. |
| Generic monitor hook races inherit upgrade FDs into a dropped service | Not established: monitor shutdown joins at supervisor.rs:3237–3241; startup claims and cron gates cover normal hook ownership. Read-side Logs can still spawn a root tail worker while FDs are temporarily inheritable; a lock-liveness concern remains, but no lower-privilege consumer was demonstrated. |
| Every normal partial state write truncates existing pid/state data | Rejected: runtime.rs:215–252 uses create-new sibling files, sync, rename, and parent sync. Fresh corrupt PID loads fail closed at daemon.rs:893–906; live-handle repair uses retained state at 808–839. Migration publication H03 is a separate path. |
| Service setsid/double-fork escape is a novel finding | Rejected as new: sysg.dev/content/docs/how-it-works/process-trees.mdx:24–26,120–130 explicitly describes escaping sessions and says cgroups are resource rather than kill boundaries. Carry as a documented limitation. |
| A command-string match proves a service generation | Rejected as a valid security argument. C03 records a remaining fallback that nevertheless uses it; numeric identity or a plausible argv is not provenance. |
| UID switch still preserves unspecified root supplementary groups | No new recurrence established: explicit group reset occurs in privilege.rs:480–505. Capability ordering is a separate observation, H02. |
| Newly configured cgroups attach only after the child can fork | Rejected for the current configured/dynamic privilege path: privilege.rs:355–358 joins before exec using the pre-opened control FD. Cgroup identity sharing for equal service hashes is already acknowledged at config/mod.rs:2317–2323. |
| Boot account-directory setup follows attacker symlinks while chowning | Rejected on the inspected path: boot.rs:774–805,829–976 validates ownership and uses descriptor-relative no-follow opens/fchown. |
| Purge project identifiers escape the root by ../ or absolute paths | Rejected on the identifier path: main.rs:5265–5275 requires one normal component. A compromised runtime parent directory is a different prerequisite, not covered by this rejection. |
| Panic automatically leaves all operation claims permanently busy | Not established: startup/replacement/watch/completion guards release through Drop; startup worker reporting handles unwind at start/sched.rs:225–232,342–347. Abrupt OOM/SIGKILL does not execute destructors, and cross-process recovery still needs separate invariants. |
| A crash between exec and durable registration is already safely recoverable | Not established. Ordinary registration failures attempt termination at daemon.rs:6023–6048, but abrupt process death in the exec-to-registration gap can still lose provenance. No separate exploitable chain was confirmed. |

## Unsafe inventory delta

The checked-in inventory lists 188 sites and was last updated at a2e269f9dacee868f38acf495172cfb831236085. A read-only, line-based syntax filter for unsafe blocks/functions/impls/extern declarations, excluding lines beginning with comments, reproduces 188 at that baseline and finds **238 matching source lines at the audited commit: +50**. Test modules are included in both counts. This is a comparable lexical check, not a compiler-level or cargo-geiger census; multiple sites on one line and unusual comment layouts are limitations. The inventory file was not regenerated or edited.

| File | Baseline | Current | Delta |
|---|---:|---:|---:|
| src/supervisor.rs | 41 | 43 | +2 |
| src/ipc.rs | 26 | 31 | +5 |
| src/logs.rs | 25 | 26 | +1 |
| src/daemon.rs | 23 | 36 | +13 |
| src/bin/main.rs | 22 | 25 | +3 |
| src/privilege.rs | 14 | 15 | +1 |
| src/cron.rs | 12 | 12 | 0 |
| src/runtime.rs | 8 | 8 | 0 |
| src/config/mod.rs | 6 | 8 | +2 |
| src/status/mod.rs | 6 | 6 | 0 |
| src/bin/sysg/ui.rs | 3 | 3 | 0 |
| src/upgrade.rs | 1 | 1 | 0 |
| src/spawn.rs | 1 | 1 | 0 |
| src/boot.rs | 0 | 12 | +12 |
| src/reaper.rs | 0 | 1 | +1 |
| src/sandbox.rs | 0 | 10 | +10 |
| **Total** | **188** | **238** | **+50** |

Review attention went to post-fork operations, namespace/capability order, mailbox identity, descriptor handoff, and filesystem descriptor ownership. A fresh exact inventory and privileged runtime checks remain prerequisites for a formal RC audit.

## Dependency review delta

Compared with the commit holding the latest historical report revision, c4966f4bfe9a953ee71399d806ac7070c7e31369, the current lockfile introduces the package names enumflags2, enumflags2_derive, landlock, seccompiler, and semver. Direct manifest changes add semver and the Linux-only landlock/seccompiler dependencies; the latter two enter the root sandbox path. This is a package-name comparison, not a complete version-by-version supply-chain audit.

| New package | Review in this session | Recorded assurance / remaining requirement |
|---|---|---|
| landlock 0.4.7 | Codex reviewed SystemG integration and relevant PathFd/access-right behavior; F06/C05 | Exemption at supply-chain/config.toml:390–392; no full crate audit or named external review |
| seccompiler 0.5.0 | Codex reviewed SystemG filter construction/application and launch coverage; H01 | Exemption at supply-chain/config.toml:666–668; full dependency review pending |
| enumflags2 | Identified as added dependency; no crate audit | Exempted without an in-entry rationale; review or justified exemption pending |
| enumflags2_derive | Identified as added build/proc-macro dependency; no crate audit | Exempted without an in-entry rationale; review or justified exemption pending |
| semver | Version/protocol comparison call sites inspected; no crate audit | Dependency-level assurance not established by this session |

The initial cargo-vet configuration at 855e4bc contains 277 exemption entries; the current configuration contains **284**, a net increase of seven. Additions include enumflags2, enumflags2_derive, landlock, seccompiler, and additional syn/thiserror/thiserror-impl versions. The local supply-chain/audits.toml has an empty audits table. The inspected added entries state versions and criteria but no explicit per-entry justification. These facts do not establish malicious dependencies; they leave the policy's named-review/justified-exemption requirement unfulfilled by the evidence reviewed here. Imported or external review coverage was not exhaustively verified.

The prior CVE log was read and preserved. deny.toml:15 currently records an unmaintained unsafe-libyaml advisory exception. No fresh vulnerability scan or upstream advisory reconciliation was performed, and the old report's fixes are not presented as a current clean bill of dependency health.

## Mode / kernel test matrix

| Mode / platform | Source coverage | Execution in this audit |
|---|---|---|
| Linux glibc x86_64/aarch64, user supervisor | IPC, launch, lifecycle, sandbox, logging, upgrade, cron | Not run |
| Linux glibc x86_64/aarch64, root --sys | Full applicable threat-model review | Not run |
| Linux musl/Alpine | Shared paths and platform branches inspected | Not built or run |
| Linux restricted container / namespace denial | Fail-open branch and cgroup fallback inspected | Not run |
| Linux container PID1 | Reaper routing, shutdown, upgrade refusal | Not run |
| Linux Landlock ABI1 versus ABI3+ / seccomp | Source and primary kernel/API contracts | No kernel matrix exercised |
| macOS user/system supervisors | Unix IPC, filesystem, state, logging, cron, upgrade; unsupported-kernel-control branches | Host available; SystemG not executed |
| Root without --sys / inherited non-root HOME | Mode selection, directory ownership, registry replay | Not run; C04 deployment validation pending |
| Failed exec / crash / OOM / partial write / signal / child exit | Error paths and retained-state consumers traced | No fault injection |

Historical CI/UAT/fuzz existence was not treated as a passing result for this commit. Historical fuzz duration, executions, crash totals, and privileged UAT results were not collected.

## Known limitations carried forward

- The manifest author, hostile root, same-UID/root socket peer authority, and kernel defects retain the threat model's stated treatment. Findings distinguish service-writable ancillary resources and compromised children from a malicious top-level operator manifest.
- Historical K2 numeric PID/PGID signal TOCTOU remains accepted history. This review does not reopen it as a new finding or assume that readiness pidfds already fix signal delivery.
- Documented session escape and the absence of a cgroup kill boundary remain. Resource containment is not complete descendant ownership.
- No earlier affected-version boundary, race probability, Linux/macOS runtime reproduction, or exploit timing was established. All source-confirmed findings still require proportionate validation before a public advisory makes runtime claims.
- Thread creation, channel admission, hook completion, output draining, and worker failure are not all globally budgeted. A same-UID peer is trusted under the model, so authenticated resource-amplification observations are not represented as unauthenticated privilege escalation.
- Ordinary atomic state writes help partial I/O but do not guarantee a multi-file transaction or recovery after every machine/filesystem failure. Parent-directory sync is best effort; migration H03 differs from normal persistence.
- Abrupt termination between child exec and durable registration, post-fork allocation/logging, and shared mount propagation remain validation targets. No independent exploitable chain was asserted for those residual hypotheses.
- Filesystem ACLs, mount topology, service-manager environment policy, executable staging practices, and kernel feature availability can change prerequisite reachability. Protected default roots mitigate several candidates, but mode bits alone do not prove ownership or ancestor trust.
- Future eBPF observation work is out of the implemented source scope. No conclusion here authorizes that future root TCB expansion.
- Source review and parallel AI review do not replace the external reviewer, runtime evidence, dependency vetting, owner disposition, or signed release decision required by audits/README.md.

## Concise remediation plan

1. **Restore configuration trust boundaries first**: make env resolution local and side-effect-free; enforce fragment ownership even for root; establish secure runtime ownership and bounded regular-file access. Address F01/F02 and validate C04.
2. **Bound supervisor-owned resources**: cap partial log bytes and aggregate queued bytes before formatting; apply backpressure/drop policy consistently, including stderr and handoff. Address F03 and the complete helper-operation deadline in H04.
3. **Make confinement promises match execution**: fail closed on denied namespaces, implement PID namespace payload/init ownership correctly, handle Landlock truncation rights, and share a complete sandbox plan across configured/dynamic launch. Address F04–F06/H01 and review C05/H02.
4. **Retain ownership through interruption**: keep terminating dynamic generations registered until verified dead, block upgrade on outstanding cleanup, and recreate cron completion/deadline owners during recovery. Address F07/F08; bind mailbox and supervisor fallback to exact generations for C02/C03.
5. **Harden transitions and finish release evidence**: bind upgrade/rollback execution to validated objects for C01; atomically publish migration outputs for H03; then, after authorization, perform targeted fault/race/kernel validation, refresh the unsafe inventory, complete dependency reviews/justifications, and obtain independent review and owner disposition.

No part of this plan was implemented. Suggested validation is follow-up work requiring the owner's approval under the session constraints.

## Release decision

**BLOCK — auditor recommendation; owner decision/signature pending.**

Owner: not signed. Date of owner decision: pending. Findings remain open, and candidate severities remain provisional. This document records the completed source analysis and stops at the requested approval boundary; it does not modify historical reports, publish advisories, create issues, or authorize remediation.
