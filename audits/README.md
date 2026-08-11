# Security Audits

This directory is the auditable security record for systemg's kernel-mode
program (RFC 0001). `reports/` holds the historical free-form security
reviews; `audits/` holds the formal per-milestone records the docs link to.

## Policy

- Every **release-candidate milestone of a root-facing phase** (RFC 0001
  Phases 1a–4) ships an immutable audit report here before the release tags.
  Patch releases inherit the most recent RC report; they do not get their own.
- A report is immutable once its release tags. Corrections land as a new
  report, never as edits.
- A clean audit still produces a report. Absence of a report means
  *unaudited*, not *clean*.
- Root-facing RC audits require an external reviewer in addition to
  maintainer self-review. Self-review evidence (tool output, inventories,
  fuzz results) feeds the report; it does not substitute for the external
  pass.

## Layout

```text
audits/
  README.md            this policy + index
  threat-model.md      living threat model (updated per phase)
  unsafe-inventory.md  generated unsafe-block inventory (regenerate per RC)
  v0.X.Y/
    report.md          scope, tag/commit, date, auditor, tools/versions,
                       findings/dispositions, known limitations,
                       mode/kernel test matrix, release decision
    evidence/          cargo audit/deny output, fuzz stats, checksums
```

## Report requirements

Each `report.md` records:

1. Scope (phases/tracks covered), git tag + commit, date, auditor(s)
2. Tool versions: cargo audit, cargo deny, fuzzer, kernel/test matrix
3. Findings with dispositions (fixed / accepted-risk / deferred + issue link)
4. Unsafe inventory delta since the previous report
5. Dependency review delta (new crates in the root TCB get named review)
6. Fuzz totals: duration, executions, crashes (triaged)
7. Known limitations carried forward
8. Release decision: ship / block, signed by the owner

## Index

| Date | Record | Scope |
|---|---|---|
| 2026-06-11 | [reports/2026-06-11.md](../reports/2026-06-11.md) | Pre-program review: socket auth, runtime perms (P0–P5, K0–K2) |
| 2026-07-13 | [reports/2026-07-13.md](../reports/2026-07-13.md) | Follow-up: all prior findings fixed/accepted; new hardening set |
| — | [reports/CVE.md](../reports/CVE.md) | Dependency CVE log |
| — | [threat-model.md](threat-model.md) | Living threat model |
| — | [unsafe-inventory.md](unsafe-inventory.md) | Generated unsafe inventory |
