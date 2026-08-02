//! The observable state: what muninn is doing, and what it knows about itself.
//!
//! Lives here rather than in the binary because the health server is the thing
//! that makes it observable — the supervisor writes, the endpoints read, and the
//! binary wires the two together. Keeping the definition next to the readers
//! also keeps readiness from becoming three call sites that disagree.
//!
//! # Why an atomic and a lock
//!
//! The state itself is an atomic: `/health/ready` reads it on every request and
//! must never be able to block the supervisor, or a slow reader would delay the
//! shutdown it is supposed to observe.
//!
//! Everything else — versions, module results, the last Telegraf exit — sits
//! behind a lock, because it is read by `/status` and `/metrics` at human
//! frequency and written a handful of times in a process lifetime. A guard is
//! never held across an await.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Where muninn is in its life.
///
/// The order matches the startup sequence, which is what makes
/// `docs/architecture.md`'s diagram checkable against the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum State {
    Starting = 0,
    LoadingConfiguration = 1,
    ValidatingConfiguration = 2,
    CheckingRuntime = 3,
    GeneratingTelegrafConfiguration = 4,
    ValidatingTelegrafConfiguration = 5,
    StartingTelegraf = 6,
    Ready = 7,
    Degraded = 8,
    Stopping = 9,
    Failed = 10,
    Stopped = 11,
}

impl State {
    /// Whether `/health/ready` should succeed.
    ///
    /// `Degraded` counts, and that is deliberate. If a failing updates module
    /// made muninn unready, an orchestrator would pull the container out of
    /// service — and stop collecting CPU, memory, disk and network metrics that
    /// were working perfectly — because it could not count pending packages.
    ///
    /// The rule stays narrow because `Degraded` is only reachable while Telegraf
    /// is running and collecting. Anything that stops collection is `Failed`.
    pub fn is_ready(&self) -> bool {
        matches!(self, State::Ready | State::Degraded)
    }

    /// Whether muninn's own loop is responsive.
    ///
    /// Deliberately a different question from readiness. A brief InfluxDB outage
    /// must not fail liveness — muninn is fine, the network is not, and
    /// restarting would help nothing. Everything before `Ready` is live too:
    /// muninn is working, it is just not finished starting, and a restart would
    /// only put it back at the beginning.
    pub fn is_live(&self) -> bool {
        !matches!(self, State::Failed | State::Stopped)
    }

    /// The stable name used in logs, `/status` and metrics.
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::LoadingConfiguration => "loading_configuration",
            State::ValidatingConfiguration => "validating_configuration",
            State::CheckingRuntime => "checking_runtime",
            State::GeneratingTelegrafConfiguration => "generating_telegraf_configuration",
            State::ValidatingTelegrafConfiguration => "validating_telegraf_configuration",
            State::StartingTelegraf => "starting_telegraf",
            State::Ready => "ready",
            State::Degraded => "degraded",
            State::Stopping => "stopping",
            State::Failed => "failed",
            State::Stopped => "stopped",
        }
    }

    /// Every state, in order. Used by the exhaustiveness tests.
    pub const ALL: [State; 12] = [
        State::Starting,
        State::LoadingConfiguration,
        State::ValidatingConfiguration,
        State::CheckingRuntime,
        State::GeneratingTelegrafConfiguration,
        State::ValidatingTelegrafConfiguration,
        State::StartingTelegraf,
        State::Ready,
        State::Degraded,
        State::Stopping,
        State::Failed,
        State::Stopped,
    ];

    fn from_u8(v: u8) -> State {
        // Not `unreachable!` on an unknown value: this reads from an atomic, and
        // a panic in a health handler would turn a reporting bug into an outage.
        State::ALL
            .get(v as usize)
            .copied()
            .unwrap_or(State::Stopped)
    }
}

/// The result of one module's last self-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleCheck {
    pub success: bool,
    /// Seconds since the Unix epoch. Absolute rather than relative so a scrape
    /// can tell "checked a moment ago" from "checked before the last restart".
    pub at: u64,
}

/// Everything `/status` and `/metrics` report beyond the state itself.
#[derive(Debug, Clone, Default)]
pub struct Details {
    pub telegraf_version: Option<String>,
    pub telegraf_pid: Option<u32>,
    /// Pre-formatted, e.g. "exit code 137". A `String` rather than the process
    /// crate's `Exit` so this crate does not have to depend on
    /// `muninn-telegraf` — health reports state, it does not manage processes.
    pub last_telegraf_exit: Option<String>,
    pub modules: Vec<String>,
    pub outputs: Vec<String>,
    pub config_generation: Option<Duration>,
    pub telegraf_validation: Option<Duration>,
    pub module_checks: BTreeMap<String, ModuleCheck>,
}

/// Shared, cheap to clone, safe to read from a request handler.
#[derive(Debug, Clone)]
pub struct HealthState(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    state: AtomicU8,
    telegraf_restarts: AtomicU64,
    started: Instant,
    details: RwLock<Details>,
}

impl HealthState {
    pub fn new() -> Self {
        HealthState(Arc::new(Inner {
            state: AtomicU8::new(State::Starting as u8),
            telegraf_restarts: AtomicU64::new(0),
            started: Instant::now(),
            details: RwLock::new(Details::default()),
        }))
    }

    pub fn get(&self) -> State {
        State::from_u8(self.0.state.load(Ordering::Acquire))
    }

    /// Move to `state`, returning the previous one so the caller can log the
    /// transition without reading twice.
    pub fn set(&self, state: State) -> State {
        State::from_u8(self.0.state.swap(state as u8, Ordering::AcqRel))
    }

    pub fn is_ready(&self) -> bool {
        self.get().is_ready()
    }

    pub fn is_live(&self) -> bool {
        self.get().is_live()
    }

    pub fn uptime(&self) -> Duration {
        self.0.started.elapsed()
    }

    pub fn telegraf_restarts(&self) -> u64 {
        self.0.telegraf_restarts.load(Ordering::Relaxed)
    }

    pub fn record_telegraf_restart(&self) {
        self.0.telegraf_restarts.fetch_add(1, Ordering::Relaxed);
    }

    /// Read the details. The guard is dropped before returning, so no caller can
    /// hold it across an await.
    pub fn details(&self) -> Details {
        self.0
            .details
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Update the details in place.
    ///
    /// A lock poisoned by a panic elsewhere is recovered rather than propagated:
    /// health reporting failing because something else panicked would take away
    /// the diagnostics exactly when they are needed.
    pub fn update(&self, f: impl FnOnce(&mut Details)) {
        let mut guard = self.0.details.write().unwrap_or_else(|e| e.into_inner());
        f(&mut guard);
    }

    /// Record a module's self-check result, stamped with the current time.
    pub fn record_module_check(&self, module: impl Into<String>, success: bool) {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.update(|d| {
            d.module_checks
                .insert(module.into(), ModuleCheck { success, at });
        });
    }
}

impl Default for HealthState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_holds_only_where_telegraf_is_collecting() {
        assert!(State::Ready.is_ready());
        assert!(
            State::Degraded.is_ready(),
            "a failing non-critical module must not take a working agent out of service"
        );
        for s in State::ALL {
            if s != State::Ready && s != State::Degraded {
                assert!(!s.is_ready(), "{s:?} must not report ready");
            }
        }
    }

    /// Liveness and readiness answer different questions. Everything before
    /// `Ready` is live — muninn is working, it is just not finished starting.
    #[test]
    fn liveness_is_a_different_question_from_readiness() {
        for s in State::ALL {
            let expected = !matches!(s, State::Failed | State::Stopped);
            assert_eq!(s.is_live(), expected, "{s:?}");
        }
        assert!(
            State::Starting.is_live() && !State::Starting.is_ready(),
            "starting is live but not ready — the whole reason they are separate"
        );
    }

    #[test]
    fn every_state_has_a_distinct_name_and_survives_the_atomic() {
        let mut seen = std::collections::HashSet::new();
        for s in State::ALL {
            assert!(seen.insert(s.as_str()), "{s:?} shares a name");
            assert_eq!(State::from_u8(s as u8), s, "{s:?} did not round-trip");
        }
        assert_eq!(seen.len(), 12, "the documented machine has twelve states");
    }

    /// Reading from an atomic means an out-of-range value is representable. A
    /// panic in a health handler would turn a reporting bug into an outage.
    #[test]
    fn an_unknown_discriminant_does_not_panic() {
        assert_eq!(State::from_u8(200), State::Stopped);
    }

    #[test]
    fn set_returns_the_previous_state() {
        let s = HealthState::new();
        assert_eq!(s.get(), State::Starting);
        assert_eq!(s.set(State::Ready), State::Starting);
        assert_eq!(s.get(), State::Ready);
    }

    /// The server holds a clone; if the two diverged, readiness would report a
    /// state the supervisor has already left.
    #[test]
    fn a_clone_observes_the_same_state() {
        let a = HealthState::new();
        let b = a.clone();
        a.set(State::Ready);
        assert_eq!(b.get(), State::Ready);
        b.set(State::Stopping);
        assert_eq!(a.get(), State::Stopping);
        assert!(!a.is_ready());
    }

    #[test]
    fn details_can_be_updated_and_read_back() {
        let s = HealthState::new();
        s.update(|d| {
            d.telegraf_pid = Some(17);
            d.modules = vec!["cpu".into(), "memory".into()];
        });
        let d = s.details();
        assert_eq!(d.telegraf_pid, Some(17));
        assert_eq!(d.modules, vec!["cpu", "memory"]);
    }

    #[test]
    fn module_checks_are_stamped_and_ordered() {
        let s = HealthState::new();
        s.record_module_check("updates", false);
        s.record_module_check("docker", true);
        let d = s.details();
        // BTreeMap: a fixed order, so /metrics output does not reshuffle between
        // scrapes and produce a meaningless diff.
        let names: Vec<&str> = d.module_checks.keys().map(String::as_str).collect();
        assert_eq!(names, vec!["docker", "updates"]);
        assert!(!d.module_checks["updates"].success);
        assert!(
            d.module_checks["updates"].at > 0,
            "should carry a timestamp"
        );
    }

    #[test]
    fn restarts_are_counted() {
        let s = HealthState::new();
        assert_eq!(s.telegraf_restarts(), 0);
        s.record_telegraf_restart();
        s.record_telegraf_restart();
        assert_eq!(s.telegraf_restarts(), 2);
    }

    /// A panic elsewhere must not take the diagnostics with it.
    #[test]
    fn a_poisoned_lock_is_recovered_rather_than_propagated() {
        let s = HealthState::new();
        let clone = s.clone();
        let _ = std::thread::spawn(move || {
            clone.update(|d| {
                d.telegraf_pid = Some(1);
                panic!("poison the lock");
            });
        })
        .join();

        // Still readable, and still writable.
        assert_eq!(s.details().telegraf_pid, Some(1));
        s.update(|d| d.telegraf_pid = Some(2));
        assert_eq!(s.details().telegraf_pid, Some(2));
    }
}
