//! Reconciles the supervisor's bookkeeping against the actual machine.
//!
//! The monitor loop trusts its in-memory process handles; this module trusts
//! nothing. It compares desired (manifests) against actual (procfs + port
//! ownership) and reclaims ports held by ghosts — untracked processes left
//! behind by a killed supervisor or an old binary — so a fresh instance can bind.
//!
//! Detection is read-verify only. Repair kills strays and lets the existing
//! restart machinery rebind, so there is never a second writer racing the owner.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use tracing::{info, warn};

use crate::config::{Config, ServiceConfig};

/// Extracts the TCP port a service is expected to own, if one can be inferred
/// from its health-check URL or a `PORT` entry in its environment.
pub fn service_port(service: &ServiceConfig) -> Option<u16> {
    if let Some(port) = service
        .deployment
        .as_ref()
        .and_then(|deployment| deployment.health_check.as_ref())
        .and_then(|health| health.url.as_deref())
        .and_then(port_from_url)
    {
        return Some(port);
    }
    port_from_env(service)
}

/// Parses the port out of an `http://host:port/...` style URL.
fn port_from_url(url: &str) -> Option<u16> {
    let without_scheme = url.split("://").nth(1).unwrap_or(url);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    host_port.rsplit(':').next().and_then(|p| p.parse().ok())
}

/// Reads a `PORT` value from a service's declared environment.
fn port_from_env(service: &ServiceConfig) -> Option<u16> {
    service
        .env
        .as_ref()
        .and_then(|env| env.vars.as_ref())
        .and_then(|vars| vars.get("PORT"))
        .and_then(|value| value.parse().ok())
}

/// Returns the PID currently listening on `port`, if any (Linux/procfs).
#[cfg(target_os = "linux")]
pub fn port_holder(port: u16) -> Option<u32> {
    let inodes = listening_inodes(port);
    if inodes.is_empty() {
        return None;
    }
    pid_owning_inode(&inodes)
}

/// Non-Linux fallback: port ownership is a prod (Linux) reconciliation concern.
#[cfg(not(target_os = "linux"))]
pub fn port_holder(_port: u16) -> Option<u32> {
    None
}

/// Hex state code for a TCP socket in LISTEN, as it appears in `/proc/net/tcp`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const TCP_LISTEN: &str = "0A";

/// Parses one `/proc/net/tcp` row and returns its socket inode when it is a
/// LISTEN socket bound to `port`.
///
/// Columns: `sl(0) local(1) rem(2) st(3) tx:rx(4) tr:tm(5) retr(6) uid(7)
/// timeout(8) inode(9)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn listening_inode_on_line(line: &str, port: u16) -> Option<u64> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 10 || fields[3] != TCP_LISTEN {
        return None;
    }
    let local_port = fields[1]
        .rsplit(':')
        .next()
        .and_then(|hex| u16::from_str_radix(hex, 16).ok())?;
    if local_port != port {
        return None;
    }
    fields[9].parse::<u64>().ok()
}

/// Collects socket inodes in LISTEN state bound to `port` from
/// `/proc/net/tcp` and `/proc/net/tcp6`.
#[cfg(target_os = "linux")]
fn listening_inodes(port: u16) -> HashSet<u64> {
    let mut inodes = HashSet::new();

    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(contents) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in contents.lines().skip(1) {
            if let Some(inode) = listening_inode_on_line(line, port) {
                inodes.insert(inode);
            }
        }
    }

    inodes
}

/// Finds the PID whose open file descriptors include one of `inodes`.
#[cfg(target_os = "linux")]
fn pid_owning_inode(inodes: &HashSet<u64>) -> Option<u32> {
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.filter_map(Result::ok) {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.filter_map(Result::ok) {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            if let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|rest| rest.strip_suffix(']'))
                .and_then(|raw| raw.parse::<u64>().ok())
                && inodes.contains(&inode)
            {
                return Some(pid);
            }
        }
    }
    None
}

/// A port held by a process the supervisor does not manage.
#[derive(Debug, Clone)]
pub struct Ghost {
    /// Service whose port is being squatted.
    pub service: String,
    /// The port under contention.
    pub port: u16,
    /// PID of the untracked holder.
    pub pid: u32,
}

/// The processes and process groups the supervisor currently owns. A port
/// holder is ours — never a ghost — if its PID is tracked or it belongs to a
/// tracked process group, which covers descendants (a shell that execs the real
/// server, a worker forked by the recorded child) that share their unit's group.
#[derive(Default)]
pub struct Tracked {
    /// PIDs recorded directly in the pid file.
    pub pids: HashSet<u32>,
    /// Process groups the supervisor's units run in.
    pub pgids: HashSet<i32>,
}

impl Tracked {
    /// Returns whether `pid` (or its process group) belongs to a managed unit.
    fn owns(&self, pid: u32) -> bool {
        if self.pids.contains(&pid) {
            return true;
        }
        pgid_of(pid).is_some_and(|pgid| self.pgids.contains(&pgid))
    }
}

/// Returns the process group id of `pid`, if it is alive.
fn pgid_of(pid: u32) -> Option<i32> {
    let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
    if pgid >= 0 { Some(pgid) } else { None }
}

/// Scans managed services for ports held by processes the supervisor does not
/// track, and returns the ghosts to reclaim.
pub fn find_port_ghosts<'a, I>(services: I, tracked: &Tracked) -> Vec<Ghost>
where
    I: IntoIterator<Item = (&'a str, &'a ServiceConfig)>,
{
    let mut ghosts = Vec::new();
    for (name, service) in services {
        let Some(port) = service_port(service) else {
            continue;
        };
        let Some(holder) = port_holder(port) else {
            continue;
        };
        if tracked.owns(holder) {
            continue;
        }
        warn!(
            "Port {port} for service '{name}' is held by untracked PID {holder}; will reclaim"
        );
        ghosts.push(Ghost {
            service: name.to_string(),
            port,
            pid: holder,
        });
    }
    ghosts
}

/// Logs a reclaimed ghost for the audit trail.
pub fn log_reclaimed(ghost: &Ghost) {
    info!(
        "Reclaimed port {} for service '{}' by terminating ghost PID {}",
        ghost.port, ghost.service, ghost.pid
    );
}

/// A snapshot of everything the supervisor currently manages, taken each tick so
/// the reconciler always compares against fresh desired/recorded state.
pub struct ManagedSnapshot {
    /// Loaded manifests across every project.
    pub configs: Vec<Arc<Config>>,
    /// Processes and groups the supervisor owns, so its own units are never ghosts.
    pub tracked: Tracked,
}

/// Background thread that reclaims ports held by ghosts in steady state, as a
/// backstop to the boot sweep. Kept separate from the monitor loop so the two
/// repair paths never race.
pub struct Reconciler {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Reconciler {
    /// Spawns the reconciler. `snapshot` yields the current managed state each
    /// tick; `reclaim` terminates a ghost through the supervisor's kill path.
    pub fn spawn<S, R>(interval: Duration, snapshot: S, reclaim: R) -> Self
    where
        S: Fn() -> ManagedSnapshot + Send + 'static,
        R: Fn(&Ghost) + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            // A ghost is only reclaimed after it persists across two consecutive
            // ticks with the same PID. This skips a unit's own restart window,
            // where the pid file briefly lags the freshly launched instance, so
            // the reconciler never races the restart machinery into killing a
            // legitimate new process.
            let mut pending: HashSet<(String, u32)> = HashSet::new();
            while !stop_clone.load(Ordering::SeqCst) {
                let managed = snapshot();
                let services = managed
                    .configs
                    .iter()
                    .flat_map(|config| config.services.iter())
                    .filter(|(_, service)| service.cron.is_none())
                    .map(|(name, service)| (name.as_str(), service));
                let mut seen = HashSet::new();
                for ghost in find_port_ghosts(services, &managed.tracked) {
                    let key = (ghost.service.clone(), ghost.pid);
                    seen.insert(key.clone());
                    if pending.contains(&key) {
                        reclaim(&ghost);
                    }
                }
                pending = seen;

                let mut slept = Duration::ZERO;
                while slept < interval {
                    if stop_clone.load(Ordering::SeqCst) {
                        return;
                    }
                    let step = Duration::from_millis(200).min(interval - slept);
                    thread::sleep(step);
                    slept += step;
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    /// Signals the reconciler to stop and joins its thread.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_port_from_health_url() {
        assert_eq!(port_from_url("http://127.0.0.1:8100/health"), Some(8100));
        assert_eq!(port_from_url("https://api.example.com:443/x"), Some(443));
        assert_eq!(port_from_url("http://localhost/health"), None);
    }

    #[test]
    fn extracts_listen_inode_for_matching_port() {
        // 0x1F9E = 8094; a LISTEN (0A) row bound to 127.0.0.1:8094 with inode 54321.
        let line = "   3: 0100007F:1F9E 00000000:0000 0A 00000000:00000000 \
                    00:00000000 00000000  1000        0 54321 1 ffff 100";
        assert_eq!(listening_inode_on_line(line, 8094), Some(54321));
        assert_eq!(listening_inode_on_line(line, 9000), None);
    }

    #[test]
    fn tracked_owns_pid_directly() {
        let tracked = Tracked {
            pids: HashSet::from([4242]),
            pgids: HashSet::new(),
        };
        assert!(tracked.owns(4242));
        assert!(!tracked.owns(4243));
    }

    #[test]
    fn tracked_owns_by_process_group() {
        let own_pgid = pgid_of(std::process::id()).expect("own pgid");
        let tracked = Tracked {
            pids: HashSet::new(),
            pgids: HashSet::from([own_pgid]),
        };
        assert!(tracked.owns(std::process::id()));
    }

    #[test]
    fn ignores_non_listen_rows() {
        // Same port but state 01 (ESTABLISHED) must not be treated as a holder.
        let line = "   3: 0100007F:1F9E 0100007F:ABCD 01 00000000:00000000 \
                    00:00000000 00000000  1000        0 54321 1 ffff 100";
        assert_eq!(listening_inode_on_line(line, 8094), None);
    }
}
