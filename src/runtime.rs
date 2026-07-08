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
/// Represents runtime context.
struct RuntimeContext {
    mode: RuntimeMode,
    state_dir: PathBuf,
    log_dir: PathBuf,
    config_dirs: Vec<PathBuf>,
    drop_privileges: bool,
    activation_fds: Vec<RawFd>,
}

static CONTEXT: OnceLock<RwLock<RuntimeContext>> = OnceLock::new();

/// Handles context lock.
fn context_lock() -> &'static RwLock<RuntimeContext> {
    CONTEXT.get_or_init(|| RwLock::new(RuntimeContext::from_mode(RuntimeMode::User)))
}

impl RuntimeContext {
    /// Handles from mode.
    fn from_mode(mode: RuntimeMode) -> Self {
        match mode {
            RuntimeMode::User => Self::user_directories(),
            RuntimeMode::System => Self::system_directories(),
        }
    }

    /// Handles user directories.
    fn user_directories() -> Self {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Self::from_user_home(home)
    }

    /// Handles from user home.
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

    /// Handles system directories.
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

/// Creates a directory (and parents) restricted to the owner (mode `0700` on Unix).
///
/// The final component's permissions are tightened after creation so existing
/// directories are also re-secured. Parent components are created with the
/// process umask; only the leaf is guaranteed owner-only.
pub fn create_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(crate::constants::PRIVATE_DIR_MODE),
        )?;
    }
    Ok(())
}

/// Writes `contents` to `path`, restricting the file to the owner (mode `0600` on Unix).
pub fn write_private_file(
    path: &std::path::Path,
    contents: impl AsRef<[u8]>,
) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(crate::constants::PRIVATE_FILE_MODE),
        )?;
    }
    Ok(())
}

/// Validates that an open config file is not attacker-controlled.
///
/// Operates on the metadata of an already-open descriptor (`fstat`) so the check
/// and the subsequent read cannot straddle a path swap. Rejects files that are
/// group/other-writable or owned by a different non-root user.
#[cfg(unix)]
fn validate_trusted_metadata(
    metadata: &std::fs::Metadata,
    path: &std::path::Path,
) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing to load config {path:?}: not a regular file"),
        ));
    }

    let mode = metadata.mode();
    if mode & 0o022 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "refusing to load config {path:?}: writable by group or others (mode {:o})",
                mode & 0o777
            ),
        ));
    }

    let owner = unsafe { libc::getuid() };
    let file_owner = metadata.uid();
    if file_owner != owner && file_owner != 0 && owner != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "refusing to load config {path:?}: owned by uid {file_owner}, not the supervisor owner ({owner})"
            ),
        ));
    }

    Ok(())
}

/// Opens a config file only if it is not attacker-controlled.
///
/// Loading a config executes the commands it declares with the supervisor's
/// privileges, so a path supplied over the control socket must not be writable
/// by anyone other than the supervisor's owner (or root). Opening with
/// `O_NOFOLLOW` (so the final component cannot be a symlink) and validating the
/// resulting descriptor with `fstat` closes the check-to-use race that a
/// stat-then-reopen sequence would leave open ([CWE-367]/[CWE-59]): the file
/// that is validated is exactly the file that is read.
///
/// [CWE-367]: https://cwe.mitre.org/data/definitions/367.html
/// [CWE-59]: https://cwe.mitre.org/data/definitions/59.html
#[cfg(unix)]
pub fn open_trusted_config(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    validate_trusted_metadata(&file.metadata()?, path)?;
    Ok(file)
}

/// Opens a config file, accepting it as-is on non-Unix targets.
#[cfg(not(unix))]
pub fn open_trusted_config(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// Rejects a config path that an untrusted user could control.
///
/// Thin wrapper over [`open_trusted_config`] for callers that only need the
/// yes/no decision; the descriptor is dropped immediately.
#[cfg(unix)]
pub fn ensure_trusted_config(path: &std::path::Path) -> std::io::Result<()> {
    open_trusted_config(path).map(|_| ())
}

/// Rejects a config path that an untrusted user could control.
#[cfg(not(unix))]
pub fn ensure_trusted_config(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
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

    // Mark inherited activation sockets close-on-exec so they are not leaked
    // into spawned (and possibly lower-privileged) service processes.
    for fd in &fds {
        set_cloexec(*fd);
    }

    set_activation_fds(fds);

    unsafe {
        env::remove_var("LISTEN_PID");
        env::remove_var("LISTEN_FDS");
        env::remove_var("LISTEN_FDNAMES");
    }
}

/// Sets the close-on-exec flag on `fd`, leaving other descriptor flags intact.
#[cfg(unix)]
fn set_cloexec(fd: RawFd) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return;
    }
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
    }
}

#[cfg(not(unix))]
/// Captures socket activation.
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

        set_drop_privileges(false); // Reset to clean state
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
        set_drop_privileges(false); // Reset to clean state
        init(RuntimeMode::System);

        assert_eq!(state_dir(), PathBuf::from("/var/lib/systemg"));
        assert_eq!(log_dir(), PathBuf::from("/var/log/systemg"));
        assert_eq!(config_dirs(), vec![PathBuf::from("/etc/systemg")]);
        assert!(!should_drop_privileges());
    }

    #[cfg(unix)]
    #[test]
    fn private_dir_and_file_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let dir = temp.path().join("state");
        create_private_dir(&dir).expect("create private dir");
        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, crate::constants::PRIVATE_DIR_MODE);

        let file = dir.join("secret");
        write_private_file(&file, b"data").expect("write private file");
        let file_mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, crate::constants::PRIVATE_FILE_MODE);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_trusted_config_rejects_group_or_world_writable() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("sysg.yaml");
        std::fs::write(&path, b"version: 1\n").expect("write config");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("chmod owner-only");
        assert!(ensure_trusted_config(&path).is_ok());

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
            .expect("chmod world-writable");
        let err = ensure_trusted_config(&path).expect_err("world-writable rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[cfg(unix)]
    #[test]
    fn open_trusted_config_rejects_symlinked_final_component() {
        let temp = tempdir().expect("tempdir");
        let target = temp.path().join("real.yaml");
        std::fs::write(&target, b"version: 1\n").expect("write target");
        let link = temp.path().join("link.yaml");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let err = open_trusted_config(&link).expect_err("symlink rejected");
        assert!(matches!(
            err.raw_os_error(),
            Some(code) if code == libc::ELOOP || code == libc::EMLINK
        ));
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
