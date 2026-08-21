//! Running a project's units as wide as their dependencies allow.
//!
//! Order between units is declared by `depends_on` and nothing else. A bulk
//! start therefore has no reason to walk a flattened list one unit at a time —
//! that made boot latency the SUM of every unit's readiness wait instead of the
//! longest dependency chain's.
//!
//! The dispatcher owns every decision that must stay deterministic: which unit
//! becomes runnable, which unit is gated because a dependency never held, and
//! the order both are reported in. Workers own only the part that blocks —
//! skip conditions, spawn, settle, health. Nothing is shared between them but
//! the completion channel, so there is no scheduler lock for daemon code to
//! deadlock against.
//!
//! With a limit of one this walks the same units in the same topological order
//! as the serial loop it replaces, so `max_concurrent: 1` is the old behavior
//! rather than an approximation of it.

use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::mpsc,
    thread::{self, ScopedJoinHandle},
};

/// The terminal state of one unit in a schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The unit is alive.
    Running,
    /// The unit ran to a clean finish.
    Completed,
    /// The unit was not started, by flag or by condition.
    Skipped,
    /// The unit was started and did not come up.
    Failed,
}

impl Resolution {
    /// Whether a dependent may proceed past a dependency in this state.
    ///
    /// A skipped dependency is NOT satisfied: a dependent that ran behind one
    /// would be running against something that never came up.
    fn satisfies_dependents(self) -> bool {
        matches!(self, Self::Running | Self::Completed)
    }
}

/// Why a unit resolved without ever being run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// A dependency was skipped, so this unit can never be satisfied.
    DependencySkipped,
    /// A dependency was started and did not come up.
    DependencyFailed,
}

/// The work a schedule delegates to its caller.
///
/// `static_skip` and `gated` run on the dispatcher thread, so what they report
/// keeps the order the topological sort produced. `start` runs on a worker and
/// is the only method that may block.
///
/// An `Err` from any method is fatal and abandons the schedule — it is for a
/// supervisor that can no longer record what it is doing, never for a unit that
/// merely failed to start. A unit that failed is `Ok(Resolution::Failed)`.
pub trait Units: Sync {
    /// What a fatal failure of the supervisor's own bookkeeping looks like.
    type Error: Send;

    /// Whether the unit is skipped by a decision needing no work — a `skip`
    /// flag, or a kind of unit a bulk start does not launch.
    ///
    /// Returns the resolution to record, or `None` to run the unit.
    fn static_resolution(&self, service: &str)
    -> Result<Option<Resolution>, Self::Error>;

    /// Runs the unit: skip condition, dependency conditions, spawn, readiness.
    ///
    /// `deps` carries the terminal state of every dependency this unit
    /// declared, all of which already satisfy it.
    fn start(
        &self,
        service: &str,
        deps: &HashMap<String, Resolution>,
    ) -> Result<Resolution, Self::Error>;

    /// Reports a unit resolved without running because a dependency did not
    /// hold. `dependency` is the first declared one that failed to, so the
    /// reason names the same unit the serial walk would have named.
    fn gated(
        &self,
        service: &str,
        dependency: &str,
        gate: Gate,
    ) -> Result<Resolution, Self::Error>;

    /// Whether the boot this schedule belongs to is still wanted.
    fn active(&self) -> bool;
}

/// A project's units and the edges between them, ready to run.
pub struct Schedule<'a> {
    /// Units in topological order. Rank in this list breaks every tie, so a
    /// narrowed schedule visits units in the order the serial walk did.
    order: &'a [String],
    /// Declared dependencies per unit, in declaration order, restricted to
    /// units this schedule covers.
    deps: HashMap<&'a str, Vec<&'a str>>,
}

impl<'a> Schedule<'a> {
    /// Builds a schedule over `order`, keeping only edges between units it
    /// covers.
    ///
    /// A filtered start drops the edges with it: selecting one unit out of a
    /// project starts that unit, and never silently pulls its dependencies in.
    pub fn new(order: &'a [String], edges: impl Fn(&str) -> Vec<&'a str>) -> Self {
        let covered: Vec<&str> = order.iter().map(String::as_str).collect();
        let deps = order
            .iter()
            .map(|service| {
                let kept = edges(service)
                    .into_iter()
                    .filter(|dep| covered.contains(dep))
                    .collect();
                (service.as_str(), kept)
            })
            .collect();
        Self { order, deps }
    }

    /// Runs every unit as soon as its own dependencies resolve, at most `limit`
    /// at once, or without a limit when `None`.
    ///
    /// Returns the terminal state of every unit, including those that never
    /// ran. A fatal error abandons dispatch but still drains what is in flight,
    /// so the schedule never returns while it owns a running worker.
    pub fn run<U: Units>(
        &self,
        units: &U,
        limit: Option<NonZeroUsize>,
    ) -> Result<HashMap<String, Resolution>, U::Error> {
        let limit = limit.map_or(usize::MAX, NonZeroUsize::get);
        let mut resolved: HashMap<String, Resolution> = HashMap::new();
        let mut pending: Vec<usize> = (0..self.order.len()).collect();
        let mut checked = vec![false; self.order.len()];
        let mut fatal: Option<(usize, U::Error)> = None;

        thread::scope(|scope| {
            let (tx, rx) = mpsc::channel();
            let mut workers: Vec<ScopedJoinHandle<'_, ()>> = Vec::new();
            let mut inflight = 0usize;

            loop {
                let mut dispatched = false;
                let mut index = 0;
                while index < pending.len() {
                    if inflight >= limit {
                        break;
                    }
                    let rank = pending[index];
                    let service = &self.order[rank];

                    // Asked BEFORE the dependency gate, and answered once: a
                    // unit that is skipped or is not a bulk start's to launch
                    // is that whatever its dependencies did, and reporting it
                    // as a casualty of one would be a different unit's failure
                    // wearing its name.
                    if !checked[rank] {
                        checked[rank] = true;
                        match units.static_resolution(service) {
                            Ok(Some(state)) => {
                                pending.remove(index);
                                resolved.insert(service.clone(), state);
                                dispatched = true;
                                continue;
                            }
                            Ok(None) => {}
                            Err(err) => {
                                pending.remove(index);
                                Self::keep_first(&mut fatal, rank, err);
                                pending.clear();
                                break;
                            }
                        }
                    }

                    match self.gate_of(service, &resolved) {
                        DepState::Waiting => {
                            index += 1;
                            continue;
                        }
                        DepState::Gated(dependency, gate) => {
                            pending.remove(index);
                            match units.gated(service, dependency, gate) {
                                Ok(state) => {
                                    resolved.insert(service.clone(), state);
                                }
                                Err(err) => {
                                    Self::keep_first(&mut fatal, rank, err);
                                    pending.clear();
                                    break;
                                }
                            }
                            dispatched = true;
                            continue;
                        }
                        DepState::Ready => {}
                    }

                    if fatal.is_some() || !units.active() {
                        pending.clear();
                        break;
                    }

                    pending.remove(index);
                    let deps = self.dep_states(service, &resolved);
                    let name = service.clone();
                    // The report is armed BEFORE the unit runs and sent when it
                    // drops, so a worker that panics still reports. Leaving the
                    // send to the end of the closure would hang the dispatcher
                    // on `recv` forever, since it holds a sender of its own and
                    // the channel therefore never closes.
                    let mut report = Report {
                        tx: tx.clone(),
                        message: Some((rank, name.clone(), Ok(Resolution::Failed))),
                    };
                    workers.push(scope.spawn(move || {
                        let outcome = units.start(&name, &deps);
                        report.finish((rank, name, outcome));
                    }));
                    inflight += 1;
                    dispatched = true;
                }

                if inflight == 0 {
                    if !dispatched || pending.is_empty() {
                        break;
                    }
                    continue;
                }

                let Ok((rank, service, outcome)) = rx.recv() else {
                    break;
                };
                inflight -= 1;
                match outcome {
                    Ok(state) => {
                        resolved.insert(service, state);
                    }
                    Err(err) => {
                        Self::keep_first(&mut fatal, rank, err);
                        resolved.insert(service, Resolution::Failed);
                        pending.clear();
                    }
                }
            }
        });

        match fatal {
            Some((_, err)) => Err(err),
            None => Ok(resolved),
        }
    }

    /// Keeps the fatal error belonging to the earliest unit in topological
    /// order, so which failure is reported does not depend on which worker
    /// happened to finish first.
    fn keep_first<E>(slot: &mut Option<(usize, E)>, rank: usize, err: E) {
        match slot {
            Some((held, _)) if *held <= rank => {}
            _ => *slot = Some((rank, err)),
        }
    }

    /// Whether a unit may run yet, given what has resolved so far.
    fn gate_of(
        &self,
        service: &str,
        resolved: &HashMap<String, Resolution>,
    ) -> DepState<'_> {
        let mut waiting = false;
        for dep in self.deps.get(service).into_iter().flatten() {
            match resolved.get(*dep) {
                None => waiting = true,
                Some(Resolution::Skipped) => {
                    return DepState::Gated(dep, Gate::DependencySkipped);
                }
                Some(Resolution::Failed) => {
                    return DepState::Gated(dep, Gate::DependencyFailed);
                }
                Some(state) if state.satisfies_dependents() => {}
                Some(_) => return DepState::Gated(dep, Gate::DependencyFailed),
            }
        }

        if waiting {
            DepState::Waiting
        } else {
            DepState::Ready
        }
    }

    /// The terminal state of each dependency a unit declared.
    fn dep_states(
        &self,
        service: &str,
        resolved: &HashMap<String, Resolution>,
    ) -> HashMap<String, Resolution> {
        self.deps
            .get(service)
            .into_iter()
            .flatten()
            .filter_map(|dep| {
                resolved.get(*dep).map(|state| ((*dep).to_string(), *state))
            })
            .collect()
    }
}

/// A worker's completion message, sent even if the worker unwinds.
///
/// A panic in a unit is still a resolution the dispatcher has to see: without
/// one it waits on a channel that never closes, and a single panicking unit
/// wedges the whole boot instead of failing it.
struct Report<E> {
    /// The dispatcher's completion channel.
    tx: mpsc::Sender<(usize, String, Result<Resolution, E>)>,
    /// What to report, armed before the unit runs and replaced by its real
    /// outcome once it finishes.
    message: Option<(usize, String, Result<Resolution, E>)>,
}

impl<E> Report<E> {
    /// Replaces the armed failure with what the unit actually resolved to.
    fn finish(&mut self, message: (usize, String, Result<Resolution, E>)) {
        self.message = Some(message);
    }
}

impl<E> Drop for Report<E> {
    /// Reports the unit, whether it returned or unwound.
    fn drop(&mut self) {
        if let Some(message) = self.message.take() {
            let _ = self.tx.send(message);
        }
    }
}

/// What a unit's dependencies say about whether it may run.
enum DepState<'a> {
    /// Every dependency resolved and is satisfied.
    Ready,
    /// At least one dependency has not resolved yet.
    Waiting,
    /// A dependency resolved in a state the dependent cannot run behind.
    Gated(&'a str, Gate),
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    /// Recording harness standing in for the daemon's start path.
    struct Recorder {
        /// Terminal state each unit's `start` should report.
        outcomes: HashMap<String, Resolution>,
        /// Units whose `static_resolution` short-circuits.
        statics: HashMap<String, Resolution>,
        /// The order units were dispatched in.
        started: Mutex<Vec<String>>,
        /// Units reported as gated, with the dependency that gated them.
        gated: Mutex<Vec<(String, String, Gate)>>,
        /// Units in flight right now.
        inflight: AtomicUsize,
        /// The highest number of units ever in flight at once.
        peak: AtomicUsize,
    }

    impl Recorder {
        /// Builds a recorder where every unit starts successfully.
        fn new() -> Self {
            Self {
                outcomes: HashMap::new(),
                statics: HashMap::new(),
                started: Mutex::new(Vec::new()),
                gated: Mutex::new(Vec::new()),
                inflight: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
            }
        }

        /// Overrides what a unit's `start` reports.
        fn outcome(mut self, service: &str, state: Resolution) -> Self {
            self.outcomes.insert(service.to_string(), state);
            self
        }

        /// Marks a unit as resolved before it is ever dispatched.
        fn statically(mut self, service: &str, state: Resolution) -> Self {
            self.statics.insert(service.to_string(), state);
            self
        }
    }

    impl Units for Recorder {
        type Error = String;

        fn static_resolution(
            &self,
            service: &str,
        ) -> Result<Option<Resolution>, Self::Error> {
            Ok(self.statics.get(service).copied())
        }

        fn start(
            &self,
            service: &str,
            _deps: &HashMap<String, Resolution>,
        ) -> Result<Resolution, Self::Error> {
            let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            self.started.lock().unwrap().push(service.to_string());
            thread::sleep(std::time::Duration::from_millis(20));
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            Ok(self
                .outcomes
                .get(service)
                .copied()
                .unwrap_or(Resolution::Running))
        }

        fn gated(
            &self,
            service: &str,
            dependency: &str,
            gate: Gate,
        ) -> Result<Resolution, Self::Error> {
            self.gated.lock().unwrap().push((
                service.to_string(),
                dependency.to_string(),
                gate,
            ));
            Ok(match gate {
                Gate::DependencySkipped => Resolution::Skipped,
                Gate::DependencyFailed => Resolution::Failed,
            })
        }

        fn active(&self) -> bool {
            true
        }
    }

    /// Builds a schedule from an explicit edge table.
    fn schedule<'a>(
        order: &'a [String],
        edges: &'a HashMap<&'a str, Vec<&'a str>>,
    ) -> Schedule<'a> {
        Schedule::new(order, |service| {
            edges.get(service).cloned().unwrap_or_default()
        })
    }

    /// Returns owned unit names for a schedule.
    fn units(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    /// Verifies units with no edges between them run at the same time.
    fn independent_units_run_concurrently() {
        let order = units(&["a", "b", "c", "d"]);
        let edges = HashMap::new();
        let recorder = Recorder::new();

        let resolved = schedule(&order, &edges).run(&recorder, None).unwrap();

        assert_eq!(resolved.len(), 4);
        assert_eq!(recorder.peak.load(Ordering::SeqCst), 4);
    }

    #[test]
    /// Verifies a limit of one reproduces the serial walk, in topological order.
    fn a_limit_of_one_walks_units_in_order() {
        let order = units(&["a", "b", "c"]);
        let edges = HashMap::new();
        let recorder = Recorder::new();

        schedule(&order, &edges)
            .run(&recorder, NonZeroUsize::new(1))
            .unwrap();

        assert_eq!(recorder.peak.load(Ordering::SeqCst), 1);
        assert_eq!(*recorder.started.lock().unwrap(), units(&["a", "b", "c"]));
    }

    #[test]
    /// Verifies a cap admits new units only as earlier ones finish.
    fn a_cap_bounds_units_in_flight() {
        let order = units(&["a", "b", "c", "d", "e"]);
        let edges = HashMap::new();
        let recorder = Recorder::new();

        schedule(&order, &edges)
            .run(&recorder, NonZeroUsize::new(2))
            .unwrap();

        assert_eq!(recorder.peak.load(Ordering::SeqCst), 2);
        assert_eq!(recorder.started.lock().unwrap().len(), 5);
    }

    #[test]
    /// Verifies a dependent waits for its own dependency and nothing else.
    fn a_dependent_waits_only_for_its_dependency() {
        let order = units(&["db", "api", "worker"]);
        let mut edges = HashMap::new();
        edges.insert("api", vec!["db"]);
        let recorder = Recorder::new();

        schedule(&order, &edges).run(&recorder, None).unwrap();

        let started = recorder.started.lock().unwrap().clone();
        let db = started.iter().position(|name| name == "db").unwrap();
        let api = started.iter().position(|name| name == "api").unwrap();
        assert!(db < api, "api started before its dependency: {started:?}");
        assert!(started.contains(&"worker".to_string()));
    }

    #[test]
    /// Verifies a failed dependency gates its dependents instead of running
    /// them, and that the gate cascades down the chain.
    fn a_failed_dependency_gates_its_dependents() {
        let order = units(&["db", "api", "web"]);
        let mut edges = HashMap::new();
        edges.insert("api", vec!["db"]);
        edges.insert("web", vec!["api"]);
        let recorder = Recorder::new().outcome("db", Resolution::Failed);

        let resolved = schedule(&order, &edges).run(&recorder, None).unwrap();

        assert_eq!(resolved.get("api"), Some(&Resolution::Failed));
        assert_eq!(resolved.get("web"), Some(&Resolution::Failed));
        assert_eq!(*recorder.started.lock().unwrap(), units(&["db"]));
        let gated = recorder.gated.lock().unwrap().clone();
        assert_eq!(
            gated[0],
            ("api".into(), "db".into(), Gate::DependencyFailed)
        );
        assert_eq!(
            gated[1],
            ("web".into(), "api".into(), Gate::DependencyFailed)
        );
    }

    #[test]
    /// Verifies a skipped dependency is never treated as satisfied.
    fn a_skipped_dependency_skips_its_dependents() {
        let order = units(&["migrations", "api"]);
        let mut edges = HashMap::new();
        edges.insert("api", vec!["migrations"]);
        let recorder = Recorder::new().statically("migrations", Resolution::Skipped);

        let resolved = schedule(&order, &edges).run(&recorder, None).unwrap();

        assert_eq!(resolved.get("api"), Some(&Resolution::Skipped));
        assert!(recorder.started.lock().unwrap().is_empty());
        assert_eq!(
            recorder.gated.lock().unwrap()[0],
            ("api".into(), "migrations".into(), Gate::DependencySkipped)
        );
    }

    #[test]
    /// Verifies a unit that is skipped in its own right stays skipped even when
    /// a dependency failed, rather than being reported as that failure's
    /// casualty.
    fn a_skipped_unit_is_not_reported_as_a_dependency_casualty() {
        let order = units(&["db", "api"]);
        let mut edges = HashMap::new();
        edges.insert("api", vec!["db"]);
        let recorder = Recorder::new()
            .outcome("db", Resolution::Failed)
            .statically("api", Resolution::Skipped);

        let resolved = schedule(&order, &edges).run(&recorder, None).unwrap();

        assert_eq!(resolved.get("api"), Some(&Resolution::Skipped));
        assert!(
            recorder.gated.lock().unwrap().is_empty(),
            "a skipped unit must not be gated by its dependency"
        );
    }

    #[test]
    /// Verifies a unit that panics resolves the schedule instead of wedging the
    /// dispatcher on a channel that never closes.
    fn a_panicking_unit_does_not_wedge_the_schedule() {
        /// Harness whose only unit panics.
        struct Exploding;

        impl Units for Exploding {
            type Error = String;

            fn static_resolution(
                &self,
                _service: &str,
            ) -> Result<Option<Resolution>, Self::Error> {
                Ok(None)
            }

            fn start(
                &self,
                _service: &str,
                _deps: &HashMap<String, Resolution>,
            ) -> Result<Resolution, Self::Error> {
                panic!("unit exploded");
            }

            fn gated(
                &self,
                _service: &str,
                _dependency: &str,
                _gate: Gate,
            ) -> Result<Resolution, Self::Error> {
                Ok(Resolution::Failed)
            }

            fn active(&self) -> bool {
                true
            }
        }

        let order = units(&["boom"]);
        let edges = HashMap::new();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome =
            std::panic::catch_unwind(|| schedule(&order, &edges).run(&Exploding, None));
        std::panic::set_hook(previous);

        assert!(outcome.is_err(), "the unit's panic must not be swallowed");
    }

    #[test]
    /// Verifies an edge to a unit the schedule does not cover is dropped, so a
    /// filtered start never waits on something it will not run.
    fn edges_outside_the_schedule_are_dropped() {
        let order = units(&["api"]);
        let mut edges = HashMap::new();
        edges.insert("api", vec!["db"]);
        let recorder = Recorder::new();

        let resolved = schedule(&order, &edges).run(&recorder, None).unwrap();

        assert_eq!(resolved.get("api"), Some(&Resolution::Running));
    }
}
