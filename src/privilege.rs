//! Privilege and resource management helpers for service spawning.
#[cfg(target_os = "linux")]
use crate::config::CgroupConfig;
use crate::config::{IsolationConfig, LimitValue, LimitsConfig, ServiceConfig};
use crate::runtime;
use libc::{RLIM_INFINITY, RLIMIT_MEMLOCK, c_int, id_t, rlimit};
#[cfg(target_os = "linux")]
use libc::{c_uint, size_t};
use nix::unistd::{Group, Uid, User, getgid, getuid};
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::collections::HashSet;
#[cfg(not(target_os = "linux"))]
use std::convert::TryInto;
use std::io;
use std::path::PathBuf;
use tracing::warn;

#[cfg(target_os = "linux")]
use tracing::info;

#[cfg(target_os = "linux")]
use std::fs;

#[cfg(target_os = "linux")]
use {
    caps::{CapSet, Capability, errors::CapsError},
    nix::{
        sched::{self, CpuSet},
        unistd::Pid,
    },
    std::str::FromStr,
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
}

impl PrivilegeContext {
    /// Analyses a service definition and records the privilege adjustments that
    /// should be applied before `exec` (e.g. UID/GID switch, limits, caps).
    pub fn from_service(service_name: &str, service: &ServiceConfig) -> io::Result<Self> {
        let mut context = PrivilegeContext {
            service_name: service_name.to_string(),
            service_hash: service.compute_hash(),
            limits: service.limits.clone(),
            capabilities: service.capabilities.clone().unwrap_or_default(),
            isolation: service.isolation.clone(),
            ..PrivilegeContext::default()
        };

        let euid = getuid();
        let requested_user = service.user.clone().or_else(|| {
            if runtime::drop_privileges_requested() && euid.is_root() {
                Some("nobody".to_string())
            } else {
                None
            }
        });

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
        Ok(context)
    }

    /// Executes all privilege adjustments inside the child process before
    /// `exec`, returning early if any step fails.
    ///
    /// # Safety
    /// Call this only between `fork` and `exec` in the child process. Invoking
    /// it in the supervisor context will mutate the supervisor's privileges and
    /// can leave the process in an inconsistent state.
    pub unsafe fn apply_pre_exec(&self) -> io::Result<()> {
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

    fn apply_limits(&self) -> io::Result<()> {
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

    fn apply_nice(&self) -> io::Result<()> {
        let Some(limits) = &self.limits else {
            return Ok(());
        };
        if let Some(nice) = limits.nice {
            let res = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice as c_int) };
            if res != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    fn apply_cpu_affinity(&self) -> io::Result<()> {
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
                set.set(*cpu as usize).map_err(io::Error::other)?;
            }
            sched::sched_setaffinity(Pid::from_raw(0), &set).map_err(io::Error::other)?;
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = cpus;
            warn!("CPU affinity requested but unsupported on this platform");
        }

        Ok(())
    }

    unsafe fn apply_user_switch(&self) -> io::Result<()> {
        if self.user.uid.is_none()
            && self.user.gid.is_none()
            && self.user.supplementary.is_empty()
        {
            return Ok(());
        }

        // Apply supplementary groups first
        if !self.user.supplementary.is_empty() {
            let mut buf = self.user.supplementary.clone();
            buf.insert(0, self.user.gid.unwrap_or_else(|| getgid().as_raw()));
            #[cfg(target_os = "linux")]
            let group_len: size_t = buf.len();
            #[cfg(not(target_os = "linux"))]
            let group_len: c_int = buf.len().try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "too many groups")
            })?;
            if unsafe { libc::setgroups(group_len, buf.as_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }

        if let Some(gid) = self.user.gid
            && unsafe { libc::setgid(gid as id_t) } != 0
        {
            return Err(io::Error::last_os_error());
        }

        if let Some(uid) = self.user.uid
            && unsafe { libc::setuid(uid as id_t) } != 0
        {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn apply_capabilities_pre_user(&self) -> io::Result<()> {
        // Capability management requires root privileges. Skip if not running as root.
        if !getuid().is_root() {
            return Ok(());
        }

        if self.capabilities.is_empty() {
            for set in [
                CapSet::Effective,
                CapSet::Permitted,
                CapSet::Inheritable,
                CapSet::Bounding,
                CapSet::Ambient,
            ] {
                caps::clear(None, set).map_err(caps_err)?;
            }
            return Ok(());
        }

        caps::securebits::set_keepcaps(true).map_err(caps_err)?;
        let caps = parse_caps(&self.capabilities)?;

        for set in [
            CapSet::Effective,
            CapSet::Permitted,
            CapSet::Inheritable,
            CapSet::Bounding,
        ] {
            caps::set(None, set, &caps).map_err(caps_err)?;
        }

        // Ambient capabilities are set after the user switch.
        caps::clear(None, CapSet::Ambient).map_err(caps_err)?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn apply_capabilities_pre_user(&self) -> io::Result<()> {
        if !self.capabilities.is_empty() {
            warn!("Capabilities requested but unsupported on this platform");
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn apply_capabilities_post_user(&self) -> io::Result<()> {
        // Capability management requires root privileges. Skip if not running as root.
        // Note: After a user switch, getuid() reflects the new non-root user, but if we
        // started as root we would have already handled capabilities in apply_capabilities_pre_user.
        // This function is a no-op for non-root processes.
        if self.user.uid.is_none() && !getuid().is_root() {
            return Ok(());
        }

        if self.capabilities.is_empty() {
            caps::clear(None, CapSet::Ambient).map_err(caps_err)?;
            return Ok(());
        }

        let caps = parse_caps(&self.capabilities)?;
        caps::set(None, CapSet::Ambient, &caps).map_err(caps_err)?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn apply_capabilities_post_user(&self) -> io::Result<()> {
        Ok(())
    }

    fn apply_isolation(&self) -> io::Result<()> {
        let Some(isolation) = &self.isolation else {
            return Ok(());
        };

        #[cfg(target_os = "linux")]
        {
            use nix::errno::Errno;
            use nix::sched::CloneFlags;

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
                match sched::unshare(flags) {
                    Ok(()) => {}
                    Err(err) => {
                        let io_err = io::Error::other(err);
                        match err {
                            Errno::EPERM => {
                                warn!(
                                    "Failed to unshare namespaces ({flags:?}) due to EPERM; continuing without isolation"
                                );
                            }
                            Errno::EINVAL => {
                                warn!(
                                    "Kernel does not support requested namespaces ({flags:?}); continuing without isolation"
                                );
                            }
                            _ => return Err(io_err),
                        }
                    }
                }
            }

            if isolation.private_devices.unwrap_or(false) {
                warn!(
                    "PrivateDevices requested; additional mount setup not yet implemented"
                );
            }
            if isolation.private_tmp.unwrap_or(false) {
                warn!("PrivateTmp requested; additional mount setup not yet implemented");
            }
            if isolation.seccomp.is_some() {
                warn!("Seccomp profiles not yet implemented; running without filters");
            }
            if isolation.apparmor_profile.is_some() {
                warn!(
                    "AppArmor profiles not yet implemented; running without confinement"
                );
            }
            if isolation.selinux_context.is_some() {
                warn!(
                    "SELinux contexts not yet implemented; running without adjustments"
                );
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
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "isolation features are only available on Linux",
                ));
            }
        }

        Ok(())
    }

    #[cfg(target_os = "linux")]
    /// Performs post-spawn privilege work (e.g. cgroup attachments) that must
    /// run after the child PID is known.
    pub fn apply_post_spawn(&self, pid: libc::pid_t) -> io::Result<()> {
        if let Some(limits) = &self.limits
            && let Some(cgroup_cfg) = &limits.cgroup
        {
            if getuid().is_root() {
                if let Err(err) =
                    apply_cgroup_settings(&self.service_hash, cgroup_cfg, pid)
                {
                    warn!(
                        "Failed to configure cgroup for '{}': {}",
                        self.service_name, err
                    );
                }
            } else {
                warn!(
                    "Cgroup configuration requested for '{}' but systemg is not running as root",
                    self.service_name
                );
            }
        }

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
    /// No-op on non-Linux targets; logs when unsupported features were
    /// requested so the supervisor can surface actionable warnings.
    pub fn apply_post_spawn(&self, _pid: libc::pid_t) -> io::Result<()> {
        if let Some(limits) = &self.limits
            && limits.cgroup.is_some()
        {
            warn!(
                "Cgroup configuration requested for '{}' but is only supported on Linux",
                self.service_name
            );
        }
        Ok(())
    }
}

fn set_rlimit(which: c_int, value: &LimitValue) -> io::Result<()> {
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
    let res = unsafe { libc::setrlimit(which as c_uint, &rlim as *const rlimit) };
    #[cfg(not(target_os = "linux"))]
    let res = unsafe { libc::setrlimit(which, &rlim as *const rlimit) };
    if res != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
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
fn caps_err(err: CapsError) -> io::Error {
    io::Error::other(err.to_string())
}

#[cfg(target_os = "linux")]
fn apply_cgroup_settings(
    service_hash: &str,
    cfg: &CgroupConfig,
    pid: libc::pid_t,
) -> io::Result<()> {
    let root = cfg
        .root
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/fs/cgroup/systemg"));

    let unit_dir = root.join(sanitize_for_fs(service_hash));
    fs::create_dir_all(&unit_dir)?;

    fs::write(unit_dir.join("cgroup.procs"), pid.to_string())?;

    if let Some(memory_max) = &cfg.memory_max {
        fs::write(unit_dir.join("memory.max"), memory_max.as_bytes())?;
    }

    if let Some(cpu_max) = &cfg.cpu_max {
        fs::write(unit_dir.join("cpu.max"), cpu_max.as_bytes())?;
    }

    if let Some(weight) = cfg.cpu_weight {
        fs::write(unit_dir.join("cpu.weight"), weight.to_string())?;
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn sanitize_for_fs(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime;
    use std::io::ErrorKind;

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
        let ctx = PrivilegeContext::from_service("demo", &service)
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

        let err = PrivilegeContext::from_service("demo", &service)
            .expect_err("user switch should fail without root");
        assert_eq!(err.kind(), ErrorKind::PermissionDenied);
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
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn apply_cgroup_settings_writes_files_to_custom_root() {
        let root = tempdir().expect("tempdir");
        let cfg = CgroupConfig {
            root: Some(root.path().to_string_lossy().into()),
            memory_max: Some("256M".into()),
            cpu_max: Some("200000 100000".into()),
            cpu_weight: Some(500),
        };

        apply_cgroup_settings("demo.service", &cfg, 4242).expect("cgroup settings");

        let unit_dir = root.path().join("demo_service");
        let contents = std::fs::read_to_string(unit_dir.join("cgroup.procs"))
            .expect("cgroup.procs exists");
        assert_eq!(contents.trim(), "4242");

        let memory = std::fs::read_to_string(unit_dir.join("memory.max"))
            .expect("memory.max exists");
        assert_eq!(memory.trim(), "256M");

        let cpu_max =
            std::fs::read_to_string(unit_dir.join("cpu.max")).expect("cpu.max exists");
        assert_eq!(cpu_max.trim(), "200000 100000");

        let weight = std::fs::read_to_string(unit_dir.join("cpu.weight"))
            .expect("cpu.weight exists");
        assert_eq!(weight.trim(), "500");
    }

    #[test]
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

        // On non-root CI this will log a warning (EPERM) but should not error.
        assert!(ctx.apply_isolation().is_ok());
    }
}
