# Unsafe Inventory

Syntax-filtered count of `unsafe` blocks/fns/impls (`unsafe {`, `unsafe fn`, `unsafe impl`, `unsafe extern`), comments excluded. Regenerate per RC; a `cargo geiger` pass is the follow-up for exactness.

Total sites (incl. test modules): **188**

| File | Sites | Lines |
|---|---|---|
| `src/supervisor.rs` | 41 | 2900, 2909, 3207, 3208, 3236, 4524, 6495, 6523, 6524, 6542, 6635, 6656, 6892, 6925, 7007, 7029, 7105, 7113, 7134, 7245, 7267, 7334, 7355, 7439, 7460, 7543, 7564, 7687, 7708, 7826, 7847, 8054, 8133, 8189, 8210, 8236, 8254, 8308, 8326, 8336, 8337 |
| `src/ipc.rs` | 26 | 398, 424, 446, 468, 483, 916, 930, 931, 1063, 1080, 1081, 1092, 1109, 1110, 1121, 1133, 1134, 1240, 1251, 1252, 1263, 1273, 1274, 1284, 1298, 1299 |
| `src/logs.rs` | 25 | 1056, 2222, 2231, 2239, 2243, 2251, 2255, 2593, 2698, 3368, 3517, 3684, 3702, 3724, 3751, 3773, 3794, 3816, 3838, 3860, 3891, 3954, 3976, 3998, 4022 |
| `src/daemon.rs` | 23 | 443, 445, 1281, 1348, 1814, 2054, 2547, 2558, 2872, 2873, 2896, 2988, 3766, 5009, 5019, 7191, 7639, 8756, 8766, 8769, 8802, 8817, 8877 |
| `src/bin/main.rs` | 22 | 354, 358, 385, 1285, 1843, 1919, 2078, 4916, 5022, 5035, 5038, 5040, 5048, 5050, 5130, 5136, 5159, 6670, 6696, 6917, 7842, 7858 |
| `src/privilege.rs` | 14 | 208, 214, 249, 284, 305, 311, 317, 576, 578, 717, 720, 723, 736, 740 |
| `src/cron.rs` | 12 | 1652, 1723, 1724, 2012, 2067, 2068, 2086, 2141, 2142, 2187, 2250, 2251 |
| `src/runtime.rs` | 8 | 209, 334, 361, 371, 375, 401, 419, 421 |
| `src/config/mod.rs` | 6 | 1586, 1798, 1816, 1943, 1958, 2103 |
| `src/status/mod.rs` | 6 | 3142, 3184, 3208, 3273, 3788, 3859 |
| `src/bin/sysg/ui.rs` | 3 | 915, 5079, 5130 |
| `src/upgrade.rs` | 1 | 426 |
| `src/spawn.rs` | 1 | 23 |

## Classification

| Class | Files | Review focus |
|---|---|---|
| Kernel seam (fork/exec, setsid, privilege, signals) | privilege.rs, daemon.rs, spawn.rs | ordering, error paths, async-signal-safety in pre_exec |
| Peer credentials / socket | ipc.rs | struct layout per-OS, error handling |
| FD handoff / CLOEXEC | logs.rs, upgrade.rs, supervisor.rs | leak windows, restore-on-failure |
| Misc libc (env, uname, isatty) | runtime.rs, cron.rs, main.rs, others | low risk, verify inputs |
