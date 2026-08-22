//! PID 1 wait broker.
//!
//! As container init, every orphan in the pid namespace reparents to sysg, so
//! `waitpid(-1)` is the only way to reap them — but it also consumes exit
//! statuses belonging to managed services and helper children (pre-start,
//! health probes, upgrade probes). The broker makes that safe: it is the only
//! caller of `waitpid(-1)` in init mode, and every reaped status lands in a
//! mailbox keyed by pid. All wait paths consult the mailbox first; statuses
//! nobody claims within the retention window are logged as adopted orphans
//! and dropped. Outside init mode the broker itself is a no-op.
//!
//! The same mailbox carries statuses the monitor reaped out from under a
//! named waiter, in every mode. A cron unit's completion thread waits on a pid
//! the monitor may reap first; without a routed status it would see `ECHILD`
//! and have to guess an outcome. A filed status is addressed to its claimant,
//! so only that waiter can take it.

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

/// How long a status addressed to a named waiter is held. Longer than the
/// orphan window because the waiter can be held off by work that runs before
/// it looks — an `onstart` hook alone may occupy the full pre-start timeout.
const CLAIM_RETENTION: Duration = Duration::from_secs(900);

/// A reaped exit status waiting to be claimed.
struct Filed {
    /// Exit status of the reaped process.
    status: ExitStatus,
    /// When the status was filed, for expiry.
    filed: Instant,
    /// Unit this status is addressed to, when the reaper knew the owner.
    claimant: Option<String>,
}

impl Filed {
    /// Returns how long this entry may sit unclaimed.
    fn retention(&self) -> Duration {
        if self.claimant.is_some() {
            CLAIM_RETENTION
        } else {
            RETENTION
        }
    }
}

fn mailbox() -> &'static Mutex<HashMap<i32, Filed>> {
    static MAILBOX: OnceLock<Mutex<HashMap<i32, Filed>>> = OnceLock::new();
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
            .insert(
                pid,
                Filed {
                    status: ExitStatus::from_raw(status),
                    filed: Instant::now(),
                    claimant: None,
                },
            );
    }
}

/// Claims the reaped status for `pid`, if the broker holds one.
///
/// A status addressed to a named waiter is left alone: it belongs to that
/// waiter, and handing it to a generic wait path would lose the outcome.
pub fn take(pid: i32) -> Option<ExitStatus> {
    if !runtime::init_mode() {
        return None;
    }
    reap_pending();
    let mut box_ = mailbox()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if box_.get(&pid).is_some_and(|filed| filed.claimant.is_some()) {
        return None;
    }
    box_.remove(&pid).map(|filed| filed.status)
}

/// Files a status the reaper took out from under `claimant`, so that waiter
/// reads the real outcome instead of an `ECHILD` it has to interpret. Active
/// in every runtime mode.
pub fn publish(pid: i32, claimant: &str, status: ExitStatus) {
    debug!("routing exit status of pid {pid} to '{claimant}'");
    mailbox()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            pid,
            Filed {
                status,
                filed: Instant::now(),
                claimant: Some(claimant.to_string()),
            },
        );
}

/// Claims the status filed for `pid` on behalf of `claimant`. Active in every
/// runtime mode.
pub fn take_for(pid: i32, claimant: &str) -> Option<ExitStatus> {
    reap_pending();
    let mut box_ = mailbox()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let addressed = box_
        .get(&pid)
        .is_some_and(|filed| filed.claimant.as_deref() == Some(claimant));
    if !addressed {
        return None;
    }
    box_.remove(&pid).map(|filed| filed.status)
}

/// Drops every status addressed to `claimant`.
///
/// A run claims only its own outcome: clearing the address before a new run
/// starts means anything still filed under that name belongs to a run nobody
/// is waiting on any more, and can never be read as this run's result.
pub fn drop_claims(claimant: &str) {
    mailbox()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|_, filed| filed.claimant.as_deref() != Some(claimant));
}

/// Drops statuses nobody claimed within their retention window, logging each
/// adopted orphan's exit. Called from the monitor tick.
pub fn sweep_orphans() {
    reap_pending();
    let mut box_ = mailbox()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    box_.retain(|pid, filed| {
        if filed.filed.elapsed() < filed.retention() {
            return true;
        }
        match filed.claimant.as_deref() {
            Some(claimant) => {
                info!(
                    "dropping unclaimed exit status of pid {pid} addressed to '{claimant}' ({:?})",
                    filed.status
                );
            }
            None => {
                info!("init reaped adopted orphan pid {pid} ({:?})", filed.status);
            }
        }
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

    #[test]
    fn routed_status_reaches_only_its_claimant() {
        let pid = -4242;
        publish(pid, "nightly", ExitStatus::from_raw(1024));

        assert!(take_for(pid, "someone-else").is_none());
        assert!(take(pid).is_none());

        let claimed = take_for(pid, "nightly").expect("claimant reads its own status");
        assert_eq!(claimed.code(), Some(4));
        assert!(take_for(pid, "nightly").is_none());
    }

    #[test]
    fn dropping_claims_clears_stale_runs() {
        let pid = -4243;
        publish(pid, "nightly", ExitStatus::from_raw(0));
        drop_claims("nightly");
        assert!(take_for(pid, "nightly").is_none());
    }
}
