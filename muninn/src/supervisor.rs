//! The supervisor: the startup sequence, the state machine, and the wait loop.
//!
//! # Why the state is explicit
//!
//! Readiness is not a boolean somebody sets in three places. It is a question
//! about a state, and the states are named so that "was muninn ready?" has one
//! answer rather than three call sites that might disagree.
//!
//! # Why there is no restart loop
//!
//! A dead Telegraf sends muninn to [`State::Failed`] and out with exit code 22.
//! The orchestrator restarts the container. What that avoids is the expensive
//! failure — a container reporting healthy from the outside while Telegraf
//! crash-loops invisibly inside it. See
//! `docs/adr/0002-supervisor-no-restart-loop.md`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use muninn_core::Config;
use muninn_core::error::{MuninnError, Result};
use muninn_modules::RenderContext;
use muninn_telegraf::process::Telegraf;
use muninn_telegraf::{validator, version};
use tracing::{error, info, warn};

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

// `is_ready` and `is_live` have no caller in the binary yet — the health server
// that asks these questions lands in WP7. They are defined and tested here
// rather than there because they are properties of the state machine, and
// deciding them next to the states is what keeps readiness from becoming three
// call sites that disagree.
#[allow(dead_code)]
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
    /// Deliberately a different question from readiness: a brief InfluxDB outage
    /// must not fail liveness, because muninn is fine and restarting would help
    /// nothing.
    pub fn is_live(&self) -> bool {
        !matches!(self, State::Failed | State::Stopped)
    }

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

    fn from_u8(v: u8) -> State {
        match v {
            0 => State::Starting,
            1 => State::LoadingConfiguration,
            2 => State::ValidatingConfiguration,
            3 => State::CheckingRuntime,
            4 => State::GeneratingTelegrafConfiguration,
            5 => State::ValidatingTelegrafConfiguration,
            6 => State::StartingTelegraf,
            7 => State::Ready,
            8 => State::Degraded,
            9 => State::Stopping,
            10 => State::Failed,
            _ => State::Stopped,
        }
    }
}

/// The state, shared with the health server.
///
/// An atomic rather than a lock: the health handler reads it on every request
/// and must never be able to block the supervisor, or a slow reader would delay
/// the shutdown it is supposed to observe.
#[derive(Debug, Clone)]
pub struct SharedState(Arc<AtomicU8>);

impl SharedState {
    pub fn new() -> Self {
        SharedState(Arc::new(AtomicU8::new(State::Starting as u8)))
    }

    pub fn get(&self) -> State {
        State::from_u8(self.0.load(Ordering::Acquire))
    }

    pub fn set(&self, state: State) {
        let previous = self.get();
        if previous != state {
            info!(from = previous.as_str(), to = state.as_str(), "state");
        }
        self.0.store(state as u8, Ordering::Release);
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

/// Run the full lifecycle: generate, verify, start, supervise, stop.
pub async fn run(config: Config, state: SharedState) -> Result<()> {
    // Signal handlers are installed BEFORE any startup work, and this ordering
    // is load-bearing rather than tidy.
    //
    // Until a handler is registered, SIGTERM has its default disposition:
    // terminate. muninn is PID 1, and an orchestrator may send SIGTERM at any
    // moment — including two seconds into startup, while Telegraf is being
    // verified. Installing the handler inside the supervise loop leaves exactly
    // that window open, and a `docker stop` landing in it kills muninn outright
    // instead of shutting it down. Found by the lifecycle test, which was
    // fast enough to hit the window.
    //
    // tokio's signal streams buffer, so a signal that arrives during startup is
    // not lost: it is delivered the moment the supervise loop first polls.
    let mut signals = StopSignals::install();

    let binary = version::binary_path();

    // Before anything is written: is this the Telegraf muninn generates
    // configuration for? A mismatch here is cheaper than a config that parses
    // and means something subtly different.
    state.set(State::CheckingRuntime);
    let telegraf_version = version::check(&binary)?;
    info!(version = telegraf_version, binary = %binary.display(), "Telegraf found");

    state.set(State::GeneratingTelegrafConfiguration);
    let rendered = muninn_telegraf::render(
        &muninn_modules::build(&RenderContext::new(&config)),
        env!("CARGO_PKG_VERSION"),
    );
    let config_path = Path::new(&config.runtime.generated_config_path);
    write_config(config_path, &rendered)?;
    info!(path = %config_path.display(), bytes = rendered.len(), "wrote Telegraf configuration");

    state.set(State::ValidatingTelegrafConfiguration);
    validator::check_config(&binary, config_path)?;
    info!("Telegraf accepted the generated configuration");

    state.set(State::StartingTelegraf);
    let host_env = config.runtime.host_env();
    let mut telegraf = Telegraf::spawn(&binary, config_path, &host_env)?;

    // Readiness only after Telegraf is confirmed running. `config check`
    // initialises without starting, so up to this point nothing has proved the
    // process can actually run.
    confirm_running(&mut telegraf, config.runtime.telegraf_start_timeout.inner()).await?;
    state.set(State::Ready);
    info!(pid = telegraf.pid(), "muninn is ready");

    supervise(&mut telegraf, &state, &config, &mut signals).await
}

/// Wait for a stop signal, or for Telegraf to die first.
async fn supervise(
    telegraf: &mut Telegraf,
    state: &SharedState,
    config: &Config,
    signals: &mut StopSignals,
) -> Result<()> {
    tokio::select! {
        // Telegraf exited on its own. Whatever the code, muninn did not ask for
        // this.
        exit = telegraf.wait() => {
            let exit = exit?;
            state.set(State::Failed);
            error!(
                pid = telegraf.pid(),
                status = %exit.describe(),
                "Telegraf exited unexpectedly — muninn is exiting so the orchestrator can restart the container"
            );
            Err(MuninnError::TelegrafExited(format!(
                "Telegraf stopped with {}. muninn does not restart it internally, so a crash \
                 is never invisible inside a seemingly-healthy container — see \
                 docs/adr/0002-supervisor-no-restart-loop.md",
                exit.describe()
            )))
        }

        signal = signals.wait() => {
            info!(signal, "stop signal received");
            // Readiness goes false first, so orchestrators and load balancers
            // stop counting on this instance before anything is torn down.
            state.set(State::Stopping);

            let exit = telegraf
                .shutdown(config.runtime.shutdown_grace_period.inner())
                .await?;

            if !exit.is_clean_shutdown() {
                warn!(status = %exit.describe(), "Telegraf did not stop cleanly");
            }
            state.set(State::Stopped);
            Ok(())
        }
    }
}

/// Confirm Telegraf is still alive a moment after spawning.
///
/// A binary that exits immediately — a config Telegraf accepts at check time but
/// refuses at start, a missing shared library — would otherwise be reported
/// ready. `config check` cannot see this, because initialising is not running.
async fn confirm_running(telegraf: &mut Telegraf, timeout: std::time::Duration) -> Result<()> {
    // A short settle window, not the full start timeout: this is looking for an
    // immediate exit, and waiting the whole timeout would delay every healthy
    // start by that long.
    let settle = std::time::Duration::from_millis(500).min(timeout);
    tokio::time::sleep(settle).await;

    match telegraf.try_exit()? {
        None => Ok(()),
        Some(exit) => Err(MuninnError::TelegrafStart(format!(
            "Telegraf exited immediately after starting, with {}. The generated configuration \
             passed `config check`, so this is something only visible at run time — a missing \
             mount, an address already in use, or a permission it does not have",
            exit.describe()
        ))),
    }
}

/// The stop signals, registered once and polled for the life of the process.
///
/// Constructed at the very start of [`run`]. See the comment there for why the
/// timing matters more than it looks.
pub struct StopSignals {
    #[cfg(unix)]
    term: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    hangup: Option<tokio::signal::unix::Signal>,
}

impl StopSignals {
    /// Register the handlers.
    ///
    /// SIGTERM as well as SIGINT: SIGTERM is what `docker stop` and systemd
    /// send, and handling only SIGINT means the shutdown path never runs under
    /// either — the container is killed ten seconds later instead, every time.
    pub fn install() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            let term = match signal(SignalKind::terminate()) {
                Ok(s) => Some(s),
                Err(e) => {
                    // Not fatal, but it does mean `docker stop` will kill rather
                    // than ask, so it is a warning an operator should see.
                    warn!(error = %e, "could not install a SIGTERM handler — Ctrl+C only");
                    None
                }
            };
            let hangup = signal(SignalKind::hangup()).ok();
            StopSignals { term, hangup }
        }
        #[cfg(not(unix))]
        {
            StopSignals {}
        }
    }

    /// Resolve when the OS asks muninn to stop, returning the signal's name.
    #[cfg(unix)]
    pub async fn wait(&mut self) -> &'static str {
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return "SIGINT",
                _ = async {
                    match self.term.as_mut() {
                        Some(s) => { s.recv().await; }
                        // No handler: park this arm rather than spinning the loop.
                        None => std::future::pending::<()>().await,
                    }
                } => return "SIGTERM",
                _ = async {
                    match self.hangup.as_mut() {
                        Some(s) => { s.recv().await; }
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    // Logged and ignored: there is no configuration reload.
                    // Change the YAML and restart the container — that model is
                    // what lets the generated configuration be ephemeral.
                    info!("SIGHUP ignored — muninn has no configuration reload; change the YAML and restart");
                }
            }
        }
    }

    /// Windows has no SIGTERM. muninn's artefact is a Linux container; this path
    /// exists so the tree builds and tests on a developer's machine.
    #[cfg(not(unix))]
    pub async fn wait(&mut self) -> &'static str {
        let _ = tokio::signal::ctrl_c().await;
        "SIGINT"
    }
}

/// Write the generated configuration, creating its directory.
///
/// The file holds resolved secrets, so on Unix it is created 0600 — a
/// world-readable configuration on a shared tmpfs would undo the reason it is
/// not persisted in the first place.
fn write_config(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| {
            MuninnError::internal(format!("cannot create '{}': {e}", dir.display()))
        })?;
    }

    std::fs::write(path, contents)
        .map_err(|e| MuninnError::internal(format!("cannot write '{}': {e}", path.display())))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            MuninnError::internal(format!(
                "cannot restrict permissions on '{}': {e}",
                path.display()
            ))
        })?;
    }

    Ok(())
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
        for s in [
            State::Starting,
            State::LoadingConfiguration,
            State::ValidatingConfiguration,
            State::CheckingRuntime,
            State::GeneratingTelegrafConfiguration,
            State::ValidatingTelegrafConfiguration,
            State::StartingTelegraf,
            State::Stopping,
            State::Failed,
            State::Stopped,
        ] {
            assert!(!s.is_ready(), "{s:?} must not report ready");
        }
    }

    /// Liveness and readiness answer different questions. Everything before
    /// `Ready` is live — muninn is working, it is just not finished starting —
    /// and a restart would only put it back at the beginning.
    #[test]
    fn liveness_is_a_different_question_from_readiness() {
        for s in [
            State::Starting,
            State::CheckingRuntime,
            State::StartingTelegraf,
            State::Ready,
            State::Degraded,
            State::Stopping,
        ] {
            assert!(s.is_live(), "{s:?} should be live");
        }
        assert!(!State::Failed.is_live());
        assert!(!State::Stopped.is_live());
    }

    #[test]
    fn every_state_has_a_distinct_stable_name() {
        let all = [
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
        let mut seen = std::collections::HashSet::new();
        for s in all {
            assert!(
                seen.insert(s.as_str()),
                "{s:?} shares a name with another state"
            );
            assert_eq!(
                State::from_u8(s as u8),
                s,
                "{s:?} does not survive the round trip through the shared atomic"
            );
        }
        assert_eq!(
            seen.len(),
            12,
            "the documented state machine has twelve states"
        );
    }

    #[test]
    fn shared_state_starts_at_starting_and_is_neither_ready_nor_dead() {
        let s = SharedState::new();
        assert_eq!(s.get(), State::Starting);
        assert!(!s.get().is_ready());
        assert!(s.get().is_live());
    }

    #[test]
    fn shared_state_is_visible_through_a_clone() {
        // The health server holds a clone; if the two diverged, readiness would
        // report a state the supervisor has already left.
        let a = SharedState::new();
        let b = a.clone();
        a.set(State::Ready);
        assert_eq!(b.get(), State::Ready);
        b.set(State::Stopping);
        assert_eq!(a.get(), State::Stopping);
    }

    #[test]
    fn writing_the_configuration_creates_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/telegraf.conf");
        write_config(&path, "[agent]\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[agent]\n");
    }

    /// The generated file holds resolved secrets. On a shared tmpfs a
    /// world-readable copy would undo the reason it is never persisted.
    #[cfg(unix)]
    #[test]
    fn the_generated_configuration_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegraf.conf");
        write_config(&path, "token = \"secret\"\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o} is readable by others");
    }

    #[test]
    fn writing_the_configuration_replaces_a_previous_one() {
        // The file is regenerated from scratch on every start; a leftover from a
        // previous configuration would be worse than no file at all.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegraf.conf");
        write_config(&path, "old\n").unwrap();
        write_config(&path, "new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
    }
}
