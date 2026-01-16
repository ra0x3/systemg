//! Runtime paths and privilege modes.
#[cfg(test)]
use std::path::Path;
use std::{
    env,
    os::fd::RawFd,
    path::PathBuf,
    sync::{OnceLock, RwLock},
};

#[cfg(unix)]
use libc;

/// Where to store state/logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    /// User home dir (~/.local/share/systemg).
    User,
    /// System dirs (/var/lib/systemg).
    System,
}

#[derive(Debug, Clone)]
struct RuntimeContext {
    mode: RuntimeMode,
    state_dir: PathBuf,
    log_dir: PathBuf,
    config_dirs: Vec<PathBuf>,
    drop_privileges: bool,
    activation_fds: Vec<RawFd>,
}

static CONTEXT: OnceLock<RwLock<RuntimeContext>> = OnceLock::new();

fn context_lock() -> &'static RwLock<RuntimeContext> {
    CONTEXT.get_or_init(|| RwLock::new(RuntimeContext::from_mode(RuntimeMode::User)))
}

impl RuntimeContext {
    fn from_mode(mode: RuntimeMode) -> Self {
        match mode {
            RuntimeMode::User => Self::user_directories(),
            RuntimeMode::System => Self::system_directories(),
        }
    }

    fn user_directories() -> Self {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Self::from_user_home(home)
    }

    fn from_user_home(home: PathBuf) -> Self {
        let state_dir = home.join(".local/share/systemg");
        let log_dir = state_dir.join("logs");
        let config_dir = home.join(".config/systemg");

        Self {
            mode: RuntimeMode::User,
            state_dir,
            log_dir,
            config_dirs: vec![config_dir],
            drop_privileges: false,
            activation_fds: Vec::new(),
        }
    }

    fn system_directories() -> Self {
        let state_dir = PathBuf::from("/var/lib/systemg");
        let log_dir = PathBuf::from("/var/log/systemg");
        let config_dir = PathBuf::from("/etc/systemg");

        Self {
            mode: RuntimeMode::System,
            state_dir,
            log_dir,
            config_dirs: vec![config_dir],
            drop_privileges: false,
            activation_fds: Vec::new(),
        }
    }
}

/// Sets runtime mode. Can be called multiple times (e.g., supervisor forks).
pub fn init(mode: RuntimeMode) {
    let mut guard = context_lock().write().expect("runtime context poisoned");
    let drop_privileges = guard.drop_privileges;
    let activation_fds = guard.activation_fds.clone();
    let mut context = RuntimeContext::from_mode(mode);
    context.drop_privileges = drop_privileges;
    context.activation_fds = activation_fds;
    *guard = context;
}

#[cfg(test)]
pub fn init_with_test_home(home: &Path) {
    let mut guard = context_lock().write().expect("runtime context poisoned");
    let drop_privileges = guard.drop_privileges;
    let activation_fds = guard.activation_fds.clone();
    let mut context = RuntimeContext::from_user_home(home.to_path_buf());
    context.drop_privileges = drop_privileges;
    context.activation_fds = activation_fds;
    *guard = context;
}

/// Returns the current runtime mode (User or System).
pub fn mode() -> RuntimeMode {
    context_lock()
        .read()
        .expect("runtime context poisoned")
        .mode
}

/// State dir (PIDs, sockets).
pub fn state_dir() -> PathBuf {
    context_lock()
        .read()
        .expect("runtime context poisoned")
        .state_dir
        .clone()
}

/// Log directory.
pub fn log_dir() -> PathBuf {
    context_lock()
        .read()
        .expect("runtime context poisoned")
        .log_dir
        .clone()
}

/// Config search paths.
pub fn config_dirs() -> Vec<PathBuf> {
    context_lock()
        .read()
        .expect("runtime context poisoned")
        .config_dirs
        .clone()
}

/// Sets privilege drop flag.
pub fn set_drop_privileges(drop: bool) {
    let mut guard = context_lock().write().expect("runtime context poisoned");
    guard.drop_privileges = drop;
}

/// Returns whether privileges should be dropped.
pub fn should_drop_privileges() -> bool {
    context_lock()
        .read()
        .expect("runtime context poisoned")
        .drop_privileges
}

/// Stores socket activation FDs (systemd LISTEN_FDS).
pub fn set_activation_fds(fds: Vec<RawFd>) {
    let mut guard = context_lock().write().expect("runtime context poisoned");
    guard.activation_fds = fds;
}

/// Returns the socket activation file descriptors.
pub fn activation_fds() -> Vec<RawFd> {
    context_lock()
        .read()
        .expect("runtime context poisoned")
        .activation_fds
        .clone()
}

/// Clears the socket activation file descriptors.
pub fn clear_activation_fds() {
    let mut guard = context_lock().write().expect("runtime context poisoned");
    guard.activation_fds.clear();
}

/// Captures socket activation FDs from init system.
#[cfg(unix)]
pub fn capture_socket_activation() {
    use std::os::unix::io::RawFd as UnixRawFd;

    let listen_pid = match env::var("LISTEN_PID")
        .ok()
        .and_then(|pid| pid.parse::<u32>().ok())
    {
        Some(pid) => pid,
        None => {
            clear_activation_fds();
            return;
        }
    };

    let current_pid = unsafe { libc::getpid() as u32 };
    if listen_pid != current_pid {
        clear_activation_fds();
        return;
    }

    let fd_count = match env::var("LISTEN_FDS")
        .ok()
        .and_then(|val| val.parse::<i32>().ok())
    {
        Some(n) if n > 0 => n,
        _ => {
            clear_activation_fds();
            return;
        }
    };

    let fds: Vec<UnixRawFd> = (0..fd_count).map(|offset| 3 + offset).collect();
    set_activation_fds(fds);

    unsafe {
        env::remove_var("LISTEN_PID");
        env::remove_var("LISTEN_FDS");
        env::remove_var("LISTEN_FDNAMES");
    }
}

#[cfg(not(unix))]
pub fn capture_socket_activation() {
    clear_activation_fds();
}

#[cfg(test)]
mod tests {
    use std::env;

    use tempfile::tempdir;

    use super::*;
    use crate::test_utils::env_lock;

    #[test]
    fn user_mode_uses_home_scoped_paths() {
        let _guard = env_lock();
        let temp = tempdir().expect("tempdir");
        let home = temp.path();
        let original_home = env::var("HOME").ok();
        unsafe {
            env::set_var("HOME", home);
        }

        init(RuntimeMode::User);
        set_drop_privileges(true);

        let expected_state = home.join(".local/share/systemg");
        let expected_logs = expected_state.join("logs");
        let expected_config = home.join(".config/systemg");

        assert_eq!(state_dir(), expected_state);
        assert_eq!(log_dir(), expected_logs);
        assert_eq!(config_dirs(), vec![expected_config]);
        assert!(should_drop_privileges());

        if let Some(previous) = original_home {
            unsafe { env::set_var("HOME", previous) };
        } else {
            unsafe { env::remove_var("HOME") };
        }
    }

    #[test]
    fn system_mode_uses_var_directories() {
        let _guard = env_lock();
        init(RuntimeMode::System);

        assert_eq!(state_dir(), PathBuf::from("/var/lib/systemg"));
        assert_eq!(log_dir(), PathBuf::from("/var/log/systemg"));
        assert_eq!(config_dirs(), vec![PathBuf::from("/etc/systemg")]);
        assert!(!should_drop_privileges());
    }

    #[test]
    fn activation_fd_setters_round_trip() {
        clear_activation_fds();
        assert!(activation_fds().is_empty());

        set_activation_fds(vec![3, 4, 5]);
        assert_eq!(activation_fds(), vec![3, 4, 5]);

        clear_activation_fds();
        assert!(activation_fds().is_empty());
    }
}
