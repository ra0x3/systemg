//! Kernel-enforced sandboxing (Phase 3, Linux only).
//!
//! The parent validates the requested policy and probes kernel support before
//! any spawn (fail-closed: an unenforceable request refuses the service rather
//! than running it unprotected). The child applies the enforcement between
//! `fork` and `exec` using only the prepared plan, in a fixed order:
//! `no_new_privs` → Landlock → seccomp. Filesystem paths are opened in the
//! parent (their `O_PATH` descriptors carried into the child) and the seccomp
//! `BpfProgram` is compiled in the parent, so the child does no path
//! resolution or allocation — only async-signal-safe raw syscalls.

#[cfg(target_os = "linux")]
mod imp {
    use std::{io, path::PathBuf};

    use landlock::{
        ABI, Access, AccessFs, PathBeneath, PathFd, RestrictionStatus, Ruleset,
        RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    };

    use crate::config::LandlockConfig;

    /// A validated, kernel-supported sandbox plan built in the parent. The
    /// path `PathFd`s own their descriptors and are carried into the child, so
    /// the child does no path resolution and the ruleset is rebuilt from the
    /// prepared fds immediately before `exec`.
    #[derive(Debug)]
    pub struct SandboxPlan {
        landlock: Option<LandlockPlan>,
        seccomp: Option<seccompiler::BpfProgram>,
    }

    #[derive(Debug)]
    struct LandlockPlan {
        ro: Vec<PathFd>,
        rw: Vec<PathFd>,
    }

    impl SandboxPlan {
        /// Builds and validates the plan for a service. Returns an error (to
        /// refuse the spawn) when a requested control cannot be enforced on the
        /// running kernel — the fail-closed contract.
        pub fn prepare(
            landlock: Option<&LandlockConfig>,
            seccomp: Option<&str>,
        ) -> io::Result<Self> {
            let landlock = match landlock {
                Some(cfg) if !cfg.ro_paths.is_empty() || !cfg.rw_paths.is_empty() => {
                    Some(LandlockPlan::prepare(cfg)?)
                }
                _ => None,
            };
            let seccomp = match seccomp {
                Some(profile) if !profile.is_empty() => {
                    Some(build_seccomp_program(profile)?)
                }
                _ => None,
            };
            Ok(Self { landlock, seccomp })
        }

        /// Whether this plan enforces anything.
        pub fn is_empty(&self) -> bool {
            self.landlock.is_none() && self.seccomp.is_none()
        }

        /// Applies the plan in the child, in fixed order: no_new_privs →
        /// Landlock → seccomp. seccomp goes last because it can forbid the very
        /// syscalls Landlock setup needs. Must run after the UID/GID switch and
        /// capability trimming, immediately before `exec`.
        ///
        /// # Safety
        /// Call only between `fork` and `exec` in the child. Only
        /// async-signal-safe operations are performed (raw syscalls, no alloc).
        pub unsafe fn apply(&self) -> io::Result<()> {
            set_no_new_privs()?;
            if let Some(plan) = &self.landlock {
                plan.apply()?;
            }
            if let Some(program) = &self.seccomp {
                seccompiler::apply_filter(program).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!("seccomp filter could not be applied: {e} (SG0722)"),
                    )
                })?;
            }
            Ok(())
        }
    }

    impl LandlockPlan {
        fn prepare(cfg: &LandlockConfig) -> io::Result<Self> {
            let open = |paths: &[PathBuf]| -> io::Result<Vec<PathFd>> {
                paths
                    .iter()
                    .map(|p| {
                        PathFd::new(p).map_err(|e| {
                            io::Error::new(
                                io::ErrorKind::NotFound,
                                format!(
                                    "landlock path '{}' could not be opened: {e}",
                                    p.display()
                                ),
                            )
                        })
                    })
                    .collect()
            };
            // Probe kernel Landlock support in the PARENT: a child pre_exec
            // failure only surfaces a bare errno (EINVAL/ENOSYS), losing the
            // SG0724 identity. Detecting it here refuses the spawn with a clear
            // message instead. landlock_create_ruleset with the version-probe
            // flag returns the ABI version, or -1/ENOSYS when unsupported.
            const LANDLOCK_CREATE_RULESET: libc::c_long = 444;
            const LANDLOCK_CREATE_RULESET_VERSION: libc::c_ulong = 1;
            let abi = unsafe {
                libc::syscall(
                    LANDLOCK_CREATE_RULESET,
                    std::ptr::null::<libc::c_void>(),
                    0usize,
                    LANDLOCK_CREATE_RULESET_VERSION,
                )
            };
            if abi < 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "landlock is not available on this kernel (needs Linux 5.13+); schema v3 refuses the service rather than run it unconfined (SG0724)",
                ));
            }

            let plan = Self {
                ro: open(&cfg.ro_paths)?,
                rw: open(&cfg.rw_paths)?,
            };
            // Also build the ruleset once so a malformed policy fails here.
            plan.build_ruleset()?;
            Ok(plan)
        }

        fn build_ruleset(&self) -> io::Result<landlock::RulesetCreated> {
            let abi = ABI::V1;
            let ro_access = AccessFs::from_read(abi);
            let rw_access = AccessFs::from_all(abi);
            let mut ruleset = Ruleset::default()
                .handle_access(AccessFs::from_all(abi))
                .map_err(to_io)?
                .create()
                .map_err(to_io)?;
            for fd in &self.ro {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(fd, ro_access))
                    .map_err(to_io)?;
            }
            for fd in &self.rw {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(fd, rw_access))
                    .map_err(to_io)?;
            }
            Ok(ruleset)
        }

        fn apply(&self) -> io::Result<()> {
            let status: RestrictionStatus =
                self.build_ruleset()?.restrict_self().map_err(to_io)?;
            match status.ruleset {
                RulesetStatus::FullyEnforced => Ok(()),
                RulesetStatus::PartiallyEnforced | RulesetStatus::NotEnforced => {
                    Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "landlock could not be fully enforced on this kernel (SG0724)",
                    ))
                }
            }
        }
    }

    fn to_io<E: std::fmt::Display>(e: E) -> io::Error {
        io::Error::new(io::ErrorKind::Unsupported, format!("landlock: {e}"))
    }

    /// The target architecture for seccomp filter compilation. seccomp filters
    /// are architecture-specific (the syscall ABI differs), so the wrong arch
    /// would silently mismatch every rule.
    fn target_arch() -> io::Result<seccompiler::TargetArch> {
        #[cfg(target_arch = "x86_64")]
        {
            Ok(seccompiler::TargetArch::x86_64)
        }
        #[cfg(target_arch = "aarch64")]
        {
            Ok(seccompiler::TargetArch::aarch64)
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "seccomp is only supported on x86_64 and aarch64 (SG0722)",
            ))
        }
    }

    /// Builds the BPF program for a named seccomp profile in the PARENT, so a
    /// bad profile or unsupported arch refuses the spawn before fork. Only
    /// versioned built-ins are accepted — never a mutable `default`/`strict`
    /// alias whose meaning could drift under a manifest.
    fn build_seccomp_program(profile: &str) -> io::Result<seccompiler::BpfProgram> {
        use std::collections::BTreeMap;

        use seccompiler::{SeccompAction, SeccompFilter};

        let allow = match profile {
            "baseline-v1" => baseline_v1_syscalls(),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "unknown seccomp profile '{other}'; the only built-in is 'baseline-v1' (SG0722)"
                    ),
                ));
            }
        };

        let rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> =
            allow.iter().map(|nr| (*nr, Vec::new())).collect();

        let filter = SeccompFilter::new(
            rules,
            // Deny-by-default: unlisted syscalls return EPERM rather than
            // killing the process, so a service degrades visibly instead of
            // vanishing.
            SeccompAction::Errno(libc::EPERM as u32),
            SeccompAction::Allow,
            target_arch()?,
        )
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("seccomp filter could not be built: {e} (SG0722)"),
            )
        })?;

        seccompiler::BpfProgram::try_from(filter).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("seccomp filter could not be compiled: {e} (SG0722)"),
            )
        })
    }

    /// The `baseline-v1` allowlist: syscalls a typical long-running service
    /// needs (file and socket I/O, memory, threads, signals, time, exec). This
    /// list is FROZEN — a stricter or different policy ships as `baseline-v2`,
    /// never as an edit here, so a manifest's guarantee cannot change under it.
    fn baseline_v1_syscalls() -> Vec<i64> {
        use libc::*;
        let mut v: Vec<i64> = [
            // process / exec (portable across x86_64 and aarch64)
            SYS_execve,
            SYS_exit,
            SYS_exit_group,
            SYS_wait4,
            SYS_clone,
            SYS_getpid,
            SYS_getppid,
            SYS_gettid,
            SYS_set_tid_address,
            SYS_set_robust_list,
            SYS_prctl,
            // memory
            SYS_brk,
            SYS_mmap,
            SYS_munmap,
            SYS_mprotect,
            SYS_mremap,
            SYS_madvise,
            SYS_rt_sigaction,
            SYS_rt_sigprocmask,
            SYS_rt_sigreturn,
            SYS_sigaltstack,
            // files
            SYS_openat,
            SYS_read,
            SYS_write,
            SYS_readv,
            SYS_writev,
            SYS_pread64,
            SYS_pwrite64,
            SYS_close,
            SYS_lseek,
            SYS_fstat,
            SYS_newfstatat,
            SYS_statx,
            SYS_fcntl,
            SYS_ioctl,
            SYS_getdents64,
            SYS_readlinkat,
            SYS_dup,
            SYS_dup3,
            SYS_pipe2,
            SYS_fsync,
            SYS_ftruncate,
            SYS_faccessat,
            // sockets / net
            SYS_socket,
            SYS_connect,
            SYS_accept4,
            SYS_bind,
            SYS_listen,
            SYS_sendto,
            SYS_recvfrom,
            SYS_sendmsg,
            SYS_recvmsg,
            SYS_shutdown,
            SYS_getsockname,
            SYS_getpeername,
            SYS_getsockopt,
            SYS_setsockopt,
            SYS_ppoll,
            SYS_epoll_create1,
            SYS_epoll_ctl,
            SYS_epoll_pwait,
            // time / sched
            SYS_clock_gettime,
            SYS_clock_nanosleep,
            SYS_nanosleep,
            SYS_sched_yield,
            SYS_futex,
            SYS_getrandom,
            SYS_uname,
            SYS_sysinfo,
            // identity (post-drop reads)
            SYS_getuid,
            SYS_geteuid,
            SYS_getgid,
            SYS_getegid,
            SYS_getcwd,
        ]
        .into_iter()
        .collect();

        // Syscalls present only on x86_64 (aarch64 dropped the legacy
        // multiplexed/`arch_prctl` forms in favour of clone/ppoll/epoll_pwait).
        #[cfg(target_arch = "x86_64")]
        v.extend([
            SYS_fork,
            SYS_vfork,
            SYS_arch_prctl,
            SYS_poll,
            SYS_epoll_wait,
            SYS_gettimeofday,
        ]);

        v
    }

    fn set_no_new_privs() -> io::Result<()> {
        let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        if rc != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "prctl(PR_SET_NO_NEW_PRIVS) failed (SG0723)",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn baseline_v1_builds_a_nonempty_program() {
            let program =
                build_seccomp_program("baseline-v1").expect("baseline-v1 must compile");
            assert!(!program.is_empty());
        }

        #[test]
        fn unknown_profile_is_refused() {
            let err = build_seccomp_program("strict")
                .expect_err("only baseline-v1 is a built-in");
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains("baseline-v1"));
        }

        #[test]
        fn baseline_v1_denies_chmod() {
            // chmod-family syscalls must NOT be in the allowlist.
            let allow = baseline_v1_syscalls();
            assert!(!allow.contains(&libc::SYS_fchmodat));
            assert!(allow.contains(&libc::SYS_openat));
        }

        #[test]
        fn prepare_seccomp_only_is_not_empty() {
            let plan = SandboxPlan::prepare(None, Some("baseline-v1"))
                .expect("seccomp-only plan prepares");
            assert!(!plan.is_empty());
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::SandboxPlan;

#[cfg(not(target_os = "linux"))]
mod stub {
    use std::io;

    use crate::config::LandlockConfig;

    /// Non-Linux stub: sandboxing is unsupported. `prepare` refuses any
    /// effective request so callers fail closed under schema v3.
    #[derive(Debug)]
    pub struct SandboxPlan;

    impl SandboxPlan {
        /// Refuses any effective sandbox request on non-Linux, so schema-v3
        /// services fail closed rather than run unconfined.
        pub fn prepare(
            landlock: Option<&LandlockConfig>,
            seccomp: Option<&str>,
        ) -> io::Result<Self> {
            let requested = landlock
                .is_some_and(|c| !c.ro_paths.is_empty() || !c.rw_paths.is_empty())
                || seccomp.is_some_and(|s| !s.is_empty());
            if requested {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "kernel-enforced sandboxing is only available on Linux",
                ));
            }
            Ok(Self)
        }

        /// Always true off Linux: nothing is enforced.
        pub fn is_empty(&self) -> bool {
            true
        }

        /// # Safety
        /// No-op; safe to call, present for API parity with the Linux path.
        pub unsafe fn apply(&self) -> io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub use stub::SandboxPlan;
