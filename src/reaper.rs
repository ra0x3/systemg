//! PID 1 wait broker.
//!
//! As container init, every orphan in the pid namespace reparents to sysg, so
//! `waitpid(-1)` is the only way to reap them — but it also consumes exit
//! statuses belonging to managed services and helper children (pre-start,
//! health probes, upgrade probes). The broker makes that safe: it is the only
//! caller of `waitpid(-1)` in init mode, and every reaped status lands in a
//! mailbox keyed by pid. All wait paths consult the mailbox first; statuses
//! nobody claims within the retention window are logged as adopted orphans
//! and dropped. Outside init mode every function here is a no-op.

use std::{
    collections::HashMap,
    os::unix::process::ExitStatusExt,
    process::ExitStatus,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use tracing::{debug, info};

use crate::runtime;

/// How long an unclaimed status stays in the mailbox before it is treated as
/// an adopted orphan's exit. Covers the spawn-before-registration race.
const RETENTION: Duration = Duration::from_secs(30);

fn mailbox() -> &'static Mutex<HashMap<i32, (ExitStatus, Instant)>> {
    static MAILBOX: OnceLock<Mutex<HashMap<i32, (ExitStatus, Instant)>>> =
        OnceLock::new();
    MAILBOX.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Reaps every currently-waitable child via `waitpid(-1, WNOHANG)` and files
/// the statuses into the mailbox. No-op outside init mode.
pub fn reap_pending() {
    if !runtime::init_mode() {
        return;
    }
    loop {
        let mut status: libc::c_int = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid <= 0 {
            break;
        }
        debug!("init broker reaped pid {pid}");
        mailbox()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(pid, (ExitStatus::from_raw(status), Instant::now()));
    }
}

/// Claims the reaped status for `pid`, if the broker holds one.
pub fn take(pid: i32) -> Option<ExitStatus> {
    if !runtime::init_mode() {
        return None;
    }
    reap_pending();
    mailbox()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&pid)
        .map(|(status, _)| status)
}

/// Drops statuses nobody claimed within the retention window, logging each as
/// an adopted orphan's exit. Called from the monitor tick in init mode.
pub fn sweep_orphans() {
    if !runtime::init_mode() {
        return;
    }
    reap_pending();
    let mut box_ = mailbox()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    box_.retain(|pid, (status, filed)| {
        if filed.elapsed() < RETENTION {
            return true;
        }
        info!("init reaped adopted orphan pid {pid} ({status:?})");
        false
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_is_inert_outside_init_mode() {
        assert!(take(1).is_none());
        reap_pending();
        sweep_orphans();
        assert!(
            mailbox()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }
}
