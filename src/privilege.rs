//! Privilege and resource management helpers for service spawning.
#[cfg(target_os = "linux")]
use std::collections::HashSet;
#[cfg(not(target_os = "linux"))]
use std::convert::TryInto;
#[cfg(target_os = "linux")]
use std::fs;
use std::{collections::HashMap, io, path::PathBuf};

#[cfg(target_os = "linux")]
use libc::size_t;
use libc::{RLIM_INFINITY, RLIMIT_MEMLOCK, c_int, id_t, rlimit};
#[cfg(target_os = "linux")]
use nix::errno::Errno;
use nix::unistd::{Group, Uid, User, getgid, getuid};
#[cfg(target_os = "linux")]
use tracing::info;
use tracing::warn;
#[cfg(target_os = "linux")]
use {
    caps::{CapSet, Capability, errors::CapsError},
    nix::{
        sched::{self, CpuSet},
        unistd::Pid,
    },
    std::str::FromStr,
};

#[cfg(target_os = "linux")]
use crate::config::CgroupConfig;
use crate::{
    childfault::{ApplyFault, ChildFault},
    config::{IsolationConfig, LimitValue, LimitsConfig, ServiceConfig},
    runtime,
};

/// Captures the target user, group, and home metadata that a service should
/// inherit once privilege adjustments have been applied.
#[derive(Debug, Clone, Default)]
pub struct UserContext {
    uid: Option<libc::uid_t>,
    gid: Option<libc::gid_t>,
    supplementary: Vec<libc::gid_t>,
    home: Option<PathBuf>,
    shell: Option<PathBuf>,
    username: Option<String>,
}

impl UserContext {
    /// The exact list to hand `setgroups`: the target gid first, then the
    /// configured supplementary groups.
    ///
    /// Built in the parent. Returns empty when no identity switch is
    /// configured, which the child reads as "leave groups alone".
    fn setgroups_list(&self) -> Vec<libc::gid_t> {
        if self.uid.is_none() && self.gid.is_none() && self.supplementary.is_empty() {
            return Vec::new();
        }
        let mut list = Vec::with_capacity(self.supplementary.len() + 1);
        list.push(self.gid.unwrap_or_else(|| getgid().as_raw()));
        list.extend_from_slice(&self.supplementary);
        list
    }

    /// Handles new.
    fn new() -> Self {
        Self {
            uid: None,
            gid: None,
            supplementary: Vec::new(),
            home: None,
            shell: None,
            username: None,
        }
    }

    /// Whether this context switches the process to a different user or group.
    pub fn drops_privileges(&self) -> bool {
        self.uid.is_some() || self.gid.is_some()
    }

    /// Builds the environment-variable overrides that align with the target
    /// account (e.g. `HOME`, `USER`, `LOGNAME`, `SHELL`).
    pub fn env_overrides(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();
        if let Some(home) = &self.home {
            env.insert("HOME".to_string(), home.display().to_string());
        }
        if let Some(username) = &self.username {
            env.insert("USER".to_string(), username.clone());
            env.insert("LOGNAME".to_string(), username.clone());
        }
        if let Some(shell) = &self.shell {
            env.insert("SHELL".to_string(), shell.display().to_string());
        }
        env
    }
}

/// Normalised privilege plan derived from a `ServiceConfig` prior to spawn.
#[derive(Debug, Clone, Default)]
pub struct PrivilegeContext {
    /// Name of the service this context applies to
    pub service_name: String,
    /// Unique hash identifying the service configuration
    pub service_hash: String,
    /// User context for privilege dropping operations
    pub user: UserContext,
    /// Resource limits to apply to the process
    pub limits: Option<LimitsConfig>,
    /// Linux capabilities to retain after privilege drop
    pub capabilities: Vec<String>,
    /// Namespace isolation configuration for the process
    pub isolation: Option<IsolationConfig>,
    /// Whether this manifest's schema refuses a control it cannot enforce.
    ///
    /// The same value that makes an unenforceable sandbox key refuse the
    /// service decides whether a resource ceiling that failed to apply is fatal
    /// or a warning: a v2 manifest that ran unconfined in a constrained
    /// container keeps running, a v3 one refuses.
    pub fail_closed: bool,
    /// Write handle on the unit's `cgroup.procs`, opened in the parent.
    ///
    /// The child joins the cgroup itself, between `fork` and `exec`. Attaching
    /// from the parent after the spawn returned left everything the service
    /// forked in that window in the parent cgroup, permanently — a fast-forking
    /// service escaped the ceiling its manifest declared.
    #[cfg(target_os = "linux")]
    pub cgroup_procs: Option<std::sync::Arc<fs::File>>,
    /// The exact supplementary group list to install, target gid first.
    ///
    /// Built in the parent: the child cannot allocate, so the vector it hands
    /// to `setgroups` must already exist. Empty means no group switch.
    groups: Vec<libc::gid_t>,
    /// Capabilities parsed in the parent, so the child never parses strings.
    #[cfg(target_os = "linux")]
    parsed_caps: HashSet<Capability>,
}

/// Names the first security key a service declares that systemg cannot yet
/// enforce, or `None` if every declared key is enforceable. seccomp, AppArmor,
/// SELinux, and the private-devices/tmp mounts are not yet enforced, so under
/// fail-closed they refuse rather than run unprotected.
/// Warns about security keys the schema accepts but this build cannot enforce.
///
/// Emitted in the parent: these depend only on configuration, and `warn!` after
/// `fork` allocates and takes the logger lock, which can deadlock the child.
fn warn_unenforced_keys(service_name: &str, service: &ServiceConfig) {
    let Some(isolation) = service.isolation.as_ref() else {
        return;
    };
    // isolation.seccomp and isolation.landlock are deliberately absent: they are
    // enforced by crate::sandbox, so warning about them would contradict the
    // protection actually applied.
    for key in [
        isolation
            .apparmor_profile
            .as_ref()
            .is_some_and(|v| !v.is_empty())
            .then_some("isolation.apparmor_profile"),
        isolation
            .selinux_context
            .as_ref()
            .is_some_and(|v| !v.is_empty())
            .then_some("isolation.selinux_context"),
        isolation
            .private_devices
            .unwrap_or(false)
            .then_some("isolation.private_devices"),
        isolation
            .private_tmp
            .unwrap_or(false)
            .then_some("isolation.private_tmp"),
    ]
    .into_iter()
    .flatten()
    {
        warn!(
            "service '{service_name}' declares '{key}', which this build cannot enforce; \
             the service runs without it (SG0721). A future release refuses the key \
             instead of running unprotected."
        );
    }
}

fn unenforceable_security_key(service: &ServiceConfig) -> Option<&'static str> {
    let isolation = service.isolation.as_ref()?;
    // isolation.seccomp and isolation.landlock are now enforced (see
    // crate::sandbox), so they are NOT listed here — under v3 they enforce
    // rather than refuse. Only the keys with no enforcement path remain.
    if isolation
        .apparmor_profile
        .as_ref()
        .is_some_and(|v| !v.is_empty())
    {
        return Some("isolation.apparmor_profile");
    }
    if isolation
        .selinux_context
        .as_ref()
        .is_some_and(|v| !v.is_empty())
    {
        return Some("isolation.selinux_context");
    }
    if isolation.private_devices.unwrap_or(false) {
        return Some("isolation.private_devices");
    }
    if isolation.private_tmp.unwrap_or(false) {
        return Some("isolation.private_tmp");
    }
    None
}

impl PrivilegeContext {
    /// Analyses a service definition and records the privilege adjustments that
    /// should be applied before `exec` (e.g. UID/GID switch, limits, caps).
    ///
    /// `fail_closed` (manifest schema v3): a security key that systemg cannot
    /// yet enforce refuses the service here, in the parent, before any spawn —
    /// rather than warning in the child and running unprotected.
    pub fn from_service(
        service_name: &str,
        service: &ServiceConfig,
        fail_closed: bool,
    ) -> io::Result<Self> {
        if fail_closed && let Some(unenforceable) = unenforceable_security_key(service) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "service '{service_name}' declares '{unenforceable}', which this build cannot enforce; schema v3 refuses it rather than run unprotected (remove the key or pin the manifest to version 2)"
                ),
            ));
        }

        warn_unenforced_keys(service_name, service);

        let mut context = PrivilegeContext {
            service_name: service_name.to_string(),
            service_hash: service.compute_hash(),
            limits: service.limits.clone(),
            capabilities: service.capabilities.clone().unwrap_or_default(),
            isolation: service.isolation.clone(),
            fail_closed,
            ..PrivilegeContext::default()
        };

        let euid = getuid();
        let requested_user = service.user.clone().or_else(|| {
            if runtime::should_drop_privileges() && euid.is_root() {
                Some("nobody".to_string())
            } else {
                None
            }
        });

        #[cfg(target_os = "linux")]
        {
            context.parsed_caps = parse_caps(&context.capabilities)?;
        }

        let requested_group = service.group.clone();
        let supplementary = service.supplementary_groups.clone().unwrap_or_default();

        if requested_user.is_none()
            && requested_group.is_none()
            && supplementary.is_empty()
        {
            return Ok(context);
        }

        if !euid.is_root() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "service '{service_name}' requested user/group switching but systemg is not running as root"
                ),
            ));
        }

        let mut user_ctx = UserContext::new();

        if let Some(user_name) = requested_user {
            let user = User::from_name(&user_name)
                .map_err(|err| io::Error::other(err.to_string()))?
                .ok_or_else(|| {
                    io::Error::other(format!("user '{user_name}' not found"))
                })?;
            user_ctx.uid = Some(user.uid.as_raw());
            user_ctx.gid = Some(user.gid.as_raw());
            user_ctx.home = Some(user.dir);
            user_ctx.shell = Some(user.shell);
            user_ctx.username = Some(user.name);
        }

        if let Some(group_name) = requested_group {
            let group = Group::from_name(&group_name)
                .map_err(|err| io::Error::other(err.to_string()))?
                .ok_or_else(|| {
                    io::Error::other(format!("group '{group_name}' not found"))
                })?;
            user_ctx.gid = Some(group.gid.as_raw());
        }

        for group_name in supplementary {
            let group = Group::from_name(&group_name)
                .map_err(|err| io::Error::other(err.to_string()))?
                .ok_or_else(|| {
                    io::Error::other(format!(
                        "supplementary group '{group_name}' not found"
                    ))
                })?;
            user_ctx.supplementary.push(group.gid.as_raw());
        }

        if user_ctx.gid.is_none()
            && let Some(uid) = user_ctx.uid
        {
            let user = User::from_uid(Uid::from_raw(uid))
                .map_err(|err| io::Error::other(err.to_string()))?
                .ok_or_else(|| {
                    io::Error::other(format!("failed to reload user by uid {uid}"))
                })?;
            user_ctx.gid = Some(user.gid.as_raw());
            if user_ctx.home.is_none() {
                user_ctx.home = Some(user.dir);
            }
            if user_ctx.shell.is_none() {
                user_ctx.shell = Some(user.shell);
            }
            if user_ctx.username.is_none() {
                user_ctx.username = Some(user.name);
            }
        }

        context.user = user_ctx;
        context.prepare();
        Ok(context)
    }

    /// Prepares the derived fields the child depends on.
    ///
    /// Must run in the parent: the child cannot allocate, so anything it hands
    /// to a syscall has to exist before `fork`.
    pub fn prepare(&mut self) {
        self.groups = self.user.setgroups_list();
    }

    /// Executes all privilege adjustments inside the child process before
    /// `exec`, returning early if any step fails.
    ///
    /// # Safety
    /// Call this only between `fork` and `exec` in the child process. Invoking
    /// it in the supervisor context will mutate the supervisor's privileges and
    /// can leave the process in an inconsistent state.
    pub unsafe fn apply_pre_exec(&self) -> Result<(), ApplyFault> {
        self.join_cgroup()?;
        self.apply_isolation()?;
        self.apply_limits()?;
        self.apply_nice()?;
        self.apply_cpu_affinity()?;
        self.apply_capabilities_pre_user()?;
        unsafe {
            self.apply_user_switch()?;
        }
        self.apply_capabilities_post_user()?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    /// Joins the prepared cgroup from inside the child, before `exec`.
    ///
    /// Writes to a descriptor the parent already opened, so this allocates
    /// nothing and takes no lock — the post-fork contract. `0` names the
    /// calling process.
    fn join_cgroup(&self) -> Result<(), ApplyFault> {
        use std::os::fd::AsRawFd;

        let Some(procs) = &self.cgroup_procs else {
            return Ok(());
        };
        let written = unsafe { libc::write(procs.as_raw_fd(), c"0".as_ptr().cast(), 1) };
        if written != 1 {
            // A cgroup the child could not join is a resource limit that did
            // not take effect, which is exactly what this fault reports.
            return Err(ApplyFault::last(ChildFault::ResourceLimit));
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    /// Cgroups are Linux-only.
    fn join_cgroup(&self) -> Result<(), ApplyFault> {
        Ok(())
    }

    /// Handles apply limits.
    fn apply_limits(&self) -> Result<(), ApplyFault> {
        let Some(limits) = &self.limits else {
            return Ok(());
        };

        if let Some(value) = &limits.nofile {
            set_rlimit(libc::RLIMIT_NOFILE as c_int, value)?;
        }
        if let Some(value) = &limits.nproc {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            set_rlimit(libc::RLIMIT_NPROC as c_int, value)?;

            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            warn!("nproc limit requested but unsupported on this platform");
        }
        if let Some(value) = &limits.memlock {
            set_rlimit(RLIMIT_MEMLOCK as c_int, value)?;
        }
        Ok(())
    }

    /// Handles apply nice.
    fn apply_nice(&self) -> Result<(), ApplyFault> {
        let Some(limits) = &self.limits else {
            return Ok(());
        };
        if let Some(nice) = limits.nice {
            let res = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice as c_int) };
            if res != 0 {
                return Err(ApplyFault::last(ChildFault::Priority));
            }
        }
        Ok(())
    }

    /// Handles apply cpu affinity.
    fn apply_cpu_affinity(&self) -> Result<(), ApplyFault> {
        let Some(limits) = &self.limits else {
            return Ok(());
        };
        let Some(cpus) = &limits.cpu_affinity else {
            return Ok(());
        };

        #[cfg(target_os = "linux")]
        {
            let mut set = CpuSet::new();
            for cpu in cpus {
                set.set(*cpu as usize)
                    .map_err(|_| ApplyFault::last(ChildFault::CpuAffinity))?;
            }
            sched::sched_setaffinity(Pid::from_raw(0), &set)
                .map_err(|_| ApplyFault::last(ChildFault::CpuAffinity))?;
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = cpus;
            warn!("CPU affinity requested but unsupported on this platform");
        }

        Ok(())
    }

    unsafe fn apply_user_switch(&self) -> Result<(), ApplyFault> {
        if self.user.uid.is_none()
            && self.user.gid.is_none()
            && self.user.supplementary.is_empty()
        {
            return Ok(());
        }

        // Always reset the supplementary group list when switching identity so
        // the child does not inherit the supervisor's (typically root's) groups.
        // The list is set to exactly the configured supplementary groups plus the
        // target gid; with no configuration it collapses to just the target gid.
        // The list is built in the parent. If a switch is configured but the
        // list is empty, this context never went through `from_service` and the
        // service would silently keep the supervisor's groups: refuse instead.
        if self.groups.is_empty() {
            return Err(ApplyFault::bare(ChildFault::SupplementaryGroups));
        }

        {
            #[cfg(target_os = "linux")]
            let group_len: size_t = self.groups.len();
            #[cfg(not(target_os = "linux"))]
            let group_len: c_int = match self.groups.len().try_into() {
                Ok(len) => len,
                Err(_) => return Err(ApplyFault::bare(ChildFault::SupplementaryGroups)),
            };
            if unsafe { libc::setgroups(group_len, self.groups.as_ptr()) } != 0 {
                return Err(ApplyFault::last(ChildFault::SupplementaryGroups));
            }
        }

        if let Some(gid) = self.user.gid
            && unsafe { libc::setgid(gid as id_t) } != 0
        {
            return Err(ApplyFault::last(ChildFault::PrimaryGid));
        }

        if let Some(uid) = self.user.uid
            && unsafe { libc::setuid(uid as id_t) } != 0
        {
            return Err(ApplyFault::last(ChildFault::UidSwitch));
        }

        Ok(())
    }

    #[cfg(target_os = "linux")]
    /// Handles apply capabilities pre user.
    fn apply_capabilities_pre_user(&self) -> Result<(), ApplyFault> {
        if !getuid().is_root() {
            return Ok(());
        }

        if self.capabilities.is_empty() {
            // When switching to an unprivileged user, do NOT strip the Effective
            // or Permitted sets here: they still hold CAP_SETGID/CAP_SETUID, which
            // the pending setgroups/setgid/setuid switch needs. Clearing them
            // first makes the identity switch EPERM (every `user:`-dropped service
            // failed to spawn under Docker). The `setuid` to a non-root user
            // clears the process capabilities per POSIX anyway. We only shed the
            // sets that do not gate the switch, and best-effort so a restricted
            // container (no CAP_SETPCAP, trimmed bounding set) cannot abort spawn.
            let switching_user = self.user.uid.is_some() || self.user.gid.is_some();
            if !switching_user {
                clear_cap_set_best_effort(CapSet::Effective);
                clear_cap_set_best_effort(CapSet::Permitted);
            }
            clear_cap_set_best_effort(CapSet::Inheritable);
            clear_cap_set_best_effort(CapSet::Bounding);
            clear_cap_set_best_effort(CapSet::Ambient);
            return Ok(());
        }

        caps::securebits::set_keepcaps(true)
            .map_err(|_| ApplyFault::last(ChildFault::CapabilityRetention))?;

        for set in [
            CapSet::Effective,
            CapSet::Permitted,
            CapSet::Inheritable,
            CapSet::Bounding,
        ] {
            caps::set(None, set, &self.parsed_caps)
                .map_err(|_| ApplyFault::last(ChildFault::CapabilityRetention))?;
        }

        caps::clear(None, CapSet::Ambient)
            .map_err(|_| ApplyFault::last(ChildFault::CapabilityReduction))?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    /// Handles apply capabilities pre user.
    fn apply_capabilities_pre_user(&self) -> Result<(), ApplyFault> {
        if !self.capabilities.is_empty() {
            warn!("Capabilities requested but unsupported on this platform");
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    /// Handles apply capabilities post user.
    fn apply_capabilities_post_user(&self) -> Result<(), ApplyFault> {
        if self.user.uid.is_none() && !getuid().is_root() {
            return Ok(());
        }

        if self.capabilities.is_empty() {
            clear_cap_set_best_effort(CapSet::Ambient);
            return Ok(());
        }

        caps::set(None, CapSet::Ambient, &self.parsed_caps)
            .map_err(|_| ApplyFault::last(ChildFault::CapabilityRetention))?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    /// Handles apply capabilities post user.
    fn apply_capabilities_post_user(&self) -> Result<(), ApplyFault> {
        Ok(())
    }

    /// Handles apply isolation.
    fn apply_isolation(&self) -> Result<(), ApplyFault> {
        let Some(isolation) = &self.isolation else {
            return Ok(());
        };

        #[cfg(target_os = "linux")]
        {
            use nix::{errno::Errno, sched::CloneFlags};

            let mut flags = CloneFlags::empty();
            if isolation.network.unwrap_or(false) {
                flags |= CloneFlags::CLONE_NEWNET;
            }
            if isolation.mount.unwrap_or(false) {
                flags |= CloneFlags::CLONE_NEWNS;
            }
            if isolation.pid.unwrap_or(false) {
                flags |= CloneFlags::CLONE_NEWPID;
            }
            if isolation.user.unwrap_or(false) {
                flags |= CloneFlags::CLONE_NEWUSER;
            }

            if !flags.is_empty() {
                // EPERM and EINVAL are the nested-container cases: the kernel
                // will not grant the namespace, and refusing here would break
                // every service that runs fine without it. Anything else is a
                // real failure. The parent logged the request, so the child
                // does not warn -- it cannot, without taking the logger lock.
                match sched::unshare(flags) {
                    Ok(()) | Err(Errno::EPERM) | Err(Errno::EINVAL) => {}
                    Err(_) => {
                        return Err(ApplyFault::last(ChildFault::NamespaceUnshare));
                    }
                }
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            let enable = isolation.network.unwrap_or(false)
                || isolation.mount.unwrap_or(false)
                || isolation.pid.unwrap_or(false)
                || isolation.user.unwrap_or(false)
                || isolation.private_devices.unwrap_or(false)
                || isolation.private_tmp.unwrap_or(false)
                || isolation.seccomp.is_some()
                || isolation.apparmor_profile.is_some()
                || isolation.selinux_context.is_some();
            if enable {
                return Err(ApplyFault::bare(ChildFault::KeyUnenforceable));
            }
        }

        Ok(())
    }

    #[cfg(target_os = "linux")]
    /// Creates the unit's cgroup, writes its ceilings, and opens `cgroup.procs`
    /// so the child can join before it execs.
    ///
    /// Everything happens in the parent, before `fork`. The previous design
    /// attached the pid after the spawn had already returned, which left a
    /// window in which the service could fork children into the parent cgroup;
    /// those children never moved, so a fast-forking service ran outside the
    /// ceiling its manifest declared.
    ///
    /// The schema decides what a failure means, exactly as it does for an
    /// unenforceable sandbox key: v3 refuses the service rather than run it
    /// unbounded, v2 keeps the previous warn-and-run so a manifest that works
    /// in a container without a delegated controller still works.
    pub fn prepare_resources(&mut self) -> io::Result<()> {
        let Some(cgroup_cfg) = self.limits.as_ref().and_then(|l| l.cgroup.clone()) else {
            return Ok(());
        };

        let outcome = if getuid().is_root() {
            apply_cgroup_settings(&self.service_hash, &cgroup_cfg)
                .map(|procs| self.cgroup_procs = Some(std::sync::Arc::new(procs)))
                .map_err(|err| {
                    format!(
                        "failed to configure cgroup for '{}': {err}",
                        self.service_name
                    )
                })
        } else {
            Err(format!(
                "service '{}' declares limits.cgroup, which needs a root supervisor",
                self.service_name
            ))
        };

        if let Err(reason) = outcome {
            if self.fail_closed {
                return Err(io::Error::other(format!(
                    "{reason}; schema v3 refuses it rather than run unbounded (pin the manifest to version 2 to keep the previous warn-and-run)"
                )));
            }
            warn!("{reason}; the service will run without it");
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    /// Cgroups are Linux-only; a request is reported once and ignored.
    pub fn prepare_resources(&mut self) -> io::Result<()> {
        if self.limits.as_ref().is_some_and(|l| l.cgroup.is_some()) {
            warn!(
                "Cgroup configuration requested for '{}' but is only supported on Linux",
                self.service_name
            );
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    /// Reports post-spawn facts that need the child's pid.
    pub fn apply_post_spawn(&self, pid: libc::pid_t) -> io::Result<()> {
        if let Some(isolation) = &self.isolation
            && isolation.pid.unwrap_or(false)
        {
            info!(
                "Service spawned inside PID namespace; child PID {} is isolated",
                pid
            );
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    /// No-op on non-Linux targets.
    pub fn apply_post_spawn(&self, _pid: libc::pid_t) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
/// Clears a capability set when supported by the running kernel, suppressing
/// `EINVAL` and `EPERM` so container-constrained environments can still start
/// services after the core capability sets have been cleared.
fn clear_cap_set_best_effort(set: CapSet) {
    if let Err(err) = caps::clear(None, set) {
        match caps_errno(&err) {
            Some(Errno::EINVAL) | Some(Errno::EPERM) | Some(Errno::ENOTSUP) => {
                warn!(
                    "Skipping unsupported capability set clear for {:?}: {}",
                    set, err
                );
            }
            _ => {
                warn!("Failed to clear capability set {:?}: {}", set, err);
            }
        }
    }
}

/// Sets rlimit.
fn set_rlimit(which: c_int, value: &LimitValue) -> Result<(), ApplyFault> {
    let rlim = match value {
        LimitValue::Fixed(v) => rlimit {
            rlim_cur: *v as libc::rlim_t,
            rlim_max: *v as libc::rlim_t,
        },
        LimitValue::Unlimited => rlimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        },
    };

    #[cfg(target_os = "linux")]
    let res = unsafe { libc::setrlimit(which as _, &rlim as *const rlimit) };
    #[cfg(not(target_os = "linux"))]
    let res = unsafe { libc::setrlimit(which, &rlim as *const rlimit) };
    if res != 0 {
        return Err(ApplyFault::last(ChildFault::ResourceLimit));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
/// Parses caps.
fn parse_caps(names: &[String]) -> io::Result<HashSet<Capability>> {
    let mut caps_set = HashSet::with_capacity(names.len());
    for name in names {
        let cap = Capability::from_str(name.trim()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid capability '{name}'"),
            )
        })?;
        caps_set.insert(cap);
    }
    Ok(caps_set)
}

#[cfg(target_os = "linux")]
/// Handles caps errno.
fn caps_errno(err: &CapsError) -> Option<Errno> {
    err.to_string()
        .split(':')
        .next_back()
        .map(str::trim)
        .and_then(|segment| segment.parse::<i32>().ok())
        .map(Errno::from_raw)
}

#[cfg(target_os = "linux")]
/// Handles apply cgroup settings.
fn apply_cgroup_settings(service_hash: &str, cfg: &CgroupConfig) -> io::Result<fs::File> {
    let root = cfg
        .root
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/fs/cgroup/systemg"));

    let unit_dir = root.join(sanitize_for_fs(service_hash));
    fs::create_dir_all(&unit_dir)?;

    if let Some(memory_max) = &cfg.memory_max {
        // `memory.max` accepts a decimal byte count or the literal `max`. A
        // suffixed size like `512M` is rejected by the kernel, so the manifest's
        // own documented spelling silently left the service unconfined.
        let raw = memory_max.trim();
        let encoded = if raw.eq_ignore_ascii_case("max") {
            "max".to_string()
        } else {
            crate::config::parse_byte_limit(raw)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("memory_max value `{raw}` is not a byte size"),
                    )
                })?
                .to_string()
        };
        fs::write(unit_dir.join("memory.max"), encoded.as_bytes())?;
    }

    if let Some(cpu_max) = &cfg.cpu_max {
        fs::write(unit_dir.join("cpu.max"), cpu_max.as_bytes())?;
    }

    if let Some(weight) = cfg.cpu_weight {
        fs::write(unit_dir.join("cpu.weight"), weight.to_string())?;
    }

    // Opened last, once the ceilings are in place: the child writes `0` to this
    // between fork and exec, so it is already inside the bounded cgroup before
    // it can create anything.
    fs::OpenOptions::new()
        .write(true)
        .open(unit_dir.join("cgroup.procs"))
}

#[cfg(target_os = "linux")]
/// Sanitizes for fs.
fn sanitize_for_fs(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::*;
    use crate::runtime;

    fn base_service() -> ServiceConfig {
        ServiceConfig {
            command: "sleep 1".into(),
            ..ServiceConfig::default()
        }
    }

    #[test]
    fn from_service_succeeds_without_privilege_changes() {
        runtime::set_drop_privileges(false);
        let service = base_service();
        let ctx = PrivilegeContext::from_service("demo", &service, false)
            .expect("context should build without privilege requests");
        assert!(ctx.user.uid.is_none());
        assert!(ctx.capabilities.is_empty());
    }

    #[test]
    fn from_service_rejects_user_switch_when_not_root() {
        if getuid().is_root() {
            return;
        }

        runtime::set_drop_privileges(false);
        let mut service = base_service();
        service.user = Some("nobody".into());

        let err = PrivilegeContext::from_service("demo", &service, false)
            .expect_err("user switch should fail without root");
        assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn user_switch_resets_supplementary_groups() {
        if !getuid().is_root() {
            return;
        }

        let Ok(Some(user)) = User::from_name("nobody") else {
            return;
        };

        let mut ctx = PrivilegeContext {
            user: UserContext {
                uid: Some(user.uid.as_raw()),
                gid: Some(user.gid.as_raw()),
                ..UserContext::default()
            },
            ..PrivilegeContext::default()
        };
        ctx.prepare();

        match unsafe { libc::fork() } {
            -1 => panic!("fork failed"),
            0 => {
                let code = match unsafe { ctx.apply_user_switch() } {
                    Ok(()) => {
                        let mut groups = [0 as libc::gid_t; 64];
                        let n = unsafe {
                            libc::getgroups(groups.len() as c_int, groups.as_mut_ptr())
                        };
                        if n < 0 {
                            2
                        } else if groups[..n as usize].contains(&0) {
                            1
                        } else {
                            0
                        }
                    }
                    Err(_) => 3,
                };
                unsafe { libc::_exit(code) };
            }
            pid => {
                let mut status = 0;
                unsafe { libc::waitpid(pid, &mut status, 0) };
                let exit_code = (status >> 8) & 0xff;
                assert_eq!(
                    exit_code, 0,
                    "child should have dropped root supplementary groups"
                );
            }
        }
    }

    #[test]
    fn env_overrides_populates_expected_fields() {
        let user = UserContext {
            home: Some(PathBuf::from("/home/example")),
            shell: Some(PathBuf::from("/bin/bash")),
            username: Some("example".into()),
            ..UserContext::default()
        };

        let vars = user.env_overrides();
        assert_eq!(vars.get("HOME"), Some(&"/home/example".to_string()));
        assert_eq!(vars.get("SHELL"), Some(&"/bin/bash".to_string()));
        assert_eq!(vars.get("USER"), Some(&"example".to_string()));
        assert_eq!(vars.get("LOGNAME"), Some(&"example".to_string()));
    }

    #[test]
    fn drops_privileges_tracks_uid_and_gid() {
        assert!(!UserContext::default().drops_privileges());
        assert!(
            UserContext {
                uid: Some(1000),
                ..UserContext::default()
            }
            .drops_privileges()
        );
        assert!(
            UserContext {
                gid: Some(1000),
                ..UserContext::default()
            }
            .drops_privileges()
        );
    }

    #[test]
    fn fail_closed_refuses_unenforceable_apparmor() {
        runtime::set_drop_privileges(false);
        let mut service = base_service();
        service.isolation = Some(IsolationConfig {
            apparmor_profile: Some("docker-default".into()),
            ..IsolationConfig::default()
        });
        let err = PrivilegeContext::from_service("demo", &service, true)
            .expect_err("v3 must refuse unenforceable apparmor");
        assert_eq!(err.kind(), ErrorKind::Unsupported);
        assert!(err.to_string().contains("isolation.apparmor_profile"));
    }

    #[test]
    fn v2_permits_unenforceable_apparmor() {
        runtime::set_drop_privileges(false);
        let mut service = base_service();
        service.isolation = Some(IsolationConfig {
            apparmor_profile: Some("docker-default".into()),
            ..IsolationConfig::default()
        });
        assert!(PrivilegeContext::from_service("demo", &service, false).is_ok());
    }

    #[test]
    fn fail_closed_ignores_enforceable_isolation() {
        runtime::set_drop_privileges(false);
        let mut service = base_service();
        // seccomp and landlock now have enforcement paths, so they do not
        // refuse under fail-closed at the privilege layer; the sandbox layer
        // handles them. apparmor left empty is not an effective request.
        service.isolation = Some(IsolationConfig {
            network: Some(true),
            seccomp: Some("baseline-v1".into()),
            apparmor_profile: Some(String::new()),
            ..IsolationConfig::default()
        });
        assert!(PrivilegeContext::from_service("demo", &service, true).is_ok());
    }
}

#[cfg(all(test, target_os = "linux"))]
/// Provides linux tests support.
mod linux_tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    /// Handles apply cgroup settings writes files to custom root.
    fn apply_cgroup_settings_writes_files_to_custom_root() {
        let root = tempdir().expect("tempdir");
        let cfg = CgroupConfig {
            root: Some(root.path().to_string_lossy().into()),
            memory_max: Some("256M".into()),
            cpu_max: Some("200000 100000".into()),
            cpu_weight: Some(500),
        };

        // On a real cgroupfs `cgroup.procs` already exists; the tempdir stands
        // in for the controller, so create it for the open to land on.
        let unit_dir = root.path().join("demo_service");
        std::fs::create_dir_all(&unit_dir).expect("unit dir");
        std::fs::write(unit_dir.join("cgroup.procs"), b"").expect("seed procs");

        let procs = apply_cgroup_settings("demo.service", &cfg).expect("cgroup settings");
        drop(procs);

        // The parent must NOT write a pid: the child joins itself before exec,
        // so nothing it forks can predate the ceiling.
        let contents = std::fs::read_to_string(unit_dir.join("cgroup.procs"))
            .expect("cgroup.procs exists");
        assert!(
            contents.trim().is_empty(),
            "the parent must leave the join to the child, got: {contents:?}"
        );

        // The kernel takes a decimal byte count here, not a suffixed size.
        let memory = std::fs::read_to_string(unit_dir.join("memory.max"))
            .expect("memory.max exists");
        assert_eq!(memory.trim(), (256 * 1024 * 1024).to_string());

        let cpu_max =
            std::fs::read_to_string(unit_dir.join("cpu.max")).expect("cpu.max exists");
        assert_eq!(cpu_max.trim(), "200000 100000");

        let weight = std::fs::read_to_string(unit_dir.join("cpu.weight"))
            .expect("cpu.weight exists");
        assert_eq!(weight.trim(), "500");
    }

    #[test]
    /// Handles apply isolation returns ok without capabilities.
    fn apply_isolation_returns_ok_without_capabilities() {
        let ctx = PrivilegeContext {
            isolation: Some(IsolationConfig {
                network: Some(true),
                mount: Some(true),
                pid: Some(true),
                ..IsolationConfig::default()
            }),
            ..PrivilegeContext::default()
        };

        assert!(ctx.apply_isolation().is_ok());
    }
}
