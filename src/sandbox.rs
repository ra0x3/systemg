//! Kernel-enforced sandboxing (Phase 3, Linux only).
//!
//! The parent validates the requested policy and probes kernel support before
//! any spawn (fail-closed: an unenforceable request refuses the service rather
//! than running it unprotected). The child applies the enforcement between
//! `fork` and `exec` using only the prepared plan, in a fixed order:
//! `no_new_privs` → Landlock. Filesystem paths are opened in the parent and
//! their `O_PATH` descriptors carried into the child so the child does no path
//! resolution of its own.

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
        pub fn prepare(landlock: Option<&LandlockConfig>) -> io::Result<Self> {
            let landlock = match landlock {
                Some(cfg) if !cfg.ro_paths.is_empty() || !cfg.rw_paths.is_empty() => {
                    Some(LandlockPlan::prepare(cfg)?)
                }
                _ => None,
            };
            Ok(Self { landlock })
        }

        /// Whether this plan enforces anything.
        pub fn is_empty(&self) -> bool {
            self.landlock.is_none()
        }

        /// Applies the plan in the child, in fixed order. Must run after the
        /// UID/GID switch and capability trimming, immediately before `exec`.
        ///
        /// # Safety
        /// Call only between `fork` and `exec` in the child.
        pub unsafe fn apply(&self) -> io::Result<()> {
            set_no_new_privs()?;
            if let Some(plan) = &self.landlock {
                plan.apply()?;
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
        pub fn prepare(landlock: Option<&LandlockConfig>) -> io::Result<Self> {
            let requested = landlock
                .is_some_and(|c| !c.ro_paths.is_empty() || !c.rw_paths.is_empty());
            if requested {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "landlock is only available on Linux",
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
