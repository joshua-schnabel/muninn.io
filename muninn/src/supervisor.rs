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
use std::time::Instant;

use muninn_core::Config;
use muninn_core::error::{MuninnError, Result};
use muninn_health::{HealthState, State};
use muninn_modules::RenderContext;
use muninn_telegraf::process::Telegraf;
use muninn_telegraf::{validator, version};

use crate::runtime_check;
use tracing::{error, info, warn};

/// Move to `state`, logging the transition.
///
/// The logging lives here rather than in `HealthState::set` so the library stays
/// free of an opinion about how transitions are reported.
fn transition(health: &HealthState, to: State) {
    let from = health.set(to);
    if from != to {
        info!(from = from.as_str(), to = to.as_str(), "state");
    }
}

/// Run the full lifecycle: generate, verify, start, supervise, stop.
pub async fn run(config: Config, state: HealthState) -> Result<()> {
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
    // What is enabled is worth reporting before anything can fail, so `/status`
    // is informative even while muninn is still starting.
    let enabled_modules: Vec<String> = config
        .modules
        .enabled_names()
        .into_iter()
        .map(String::from)
        .collect();
    let enabled_outputs: Vec<String> = config
        .outputs
        .influxdb
        .iter()
        .map(|_| "influxdb".to_string())
        .chain(
            config
                .outputs
                .prometheus
                .iter()
                .map(|_| "prometheus".to_string()),
        )
        .collect();
    state.update(|d| {
        d.modules = enabled_modules;
        d.outputs = enabled_outputs;
    });

    transition(&state, State::CheckingRuntime);

    let telegraf_version = version::check(&binary)?;
    state.update(|d| d.telegraf_version = Some(telegraf_version.clone()));
    info!(version = telegraf_version, binary = %binary.display(), "Telegraf found");

    // The preconditions the enabled modules declare: mounts, socket paths, a
    // writable runtime directory, and — for anything with an endpoint — that
    // the service is actually answering.
    //
    // Refusing to start is the point. Every one of these failures has a
    // plausible-looking symptom rather than an obvious one: metrics about the
    // container instead of the host, or an empty container list that reads as
    // "nothing running". Starting anyway would publish confident wrong numbers,
    // which is the failure mode muninn exists to prevent.
    let findings = runtime_check::preconditions(&config);
    for f in &findings {
        match f.severity {
            runtime_check::Severity::Error => {
                error!(subject = %f.subject, "{}", f.message);
            }
            runtime_check::Severity::Warning => {
                warn!(subject = %f.subject, "{}", f.message);
            }
        }
    }
    if runtime_check::has_errors(&findings) {
        let count = findings
            .iter()
            .filter(|f| f.severity == runtime_check::Severity::Error)
            .count();
        return Err(MuninnError::runtime(format!(
            "{count} runtime precondition(s) not met — see the errors above, or run \
             `muninn check-runtime` for the full report"
        )));
    }

    transition(&state, State::GeneratingTelegrafConfiguration);
    let generation_started = Instant::now();
    let rendered = muninn_telegraf::render(
        &muninn_modules::build(&RenderContext::new(&config)),
        env!("CARGO_PKG_VERSION"),
    );
    let config_path = Path::new(&config.runtime.generated_config_path);
    crate::generated_config::write(config_path, &rendered)?;
    let generation = generation_started.elapsed();
    state.update(|d| d.config_generation = Some(generation));
    info!(path = %config_path.display(), bytes = rendered.len(), "wrote Telegraf configuration");

    transition(&state, State::ValidatingTelegrafConfiguration);
    let validation_started = Instant::now();
    validator::check_config(&binary, config_path)?;
    let validation = validation_started.elapsed();
    state.update(|d| d.telegraf_validation = Some(validation));
    info!("Telegraf accepted the generated configuration");

    transition(&state, State::StartingTelegraf);
    let host_env = config.runtime.host_env();
    let mut telegraf = Telegraf::spawn(&binary, config_path, &host_env)?;

    // Readiness only after Telegraf is confirmed running. `config check`
    // initialises without starting, so up to this point nothing has proved the
    // process can actually run.
    confirm_running(&mut telegraf, config.runtime.telegraf_start_timeout.inner()).await?;
    // The PID is what makes `muninn_telegraf_running` true, so it is recorded
    // only once the process is confirmed — not at spawn time.
    state.update(|d| d.telegraf_pid = Some(telegraf.pid()));
    transition(&state, State::Ready);
    info!(pid = telegraf.pid(), "muninn is ready");

    // Only after readiness: the check runs apt over the host's whole package
    // index and takes seconds, and delaying readiness for it would hold up an
    // orchestrator for something that is not part of collecting metrics.
    if config.modules.updates.enabled {
        check_updates_once(&config, &state).await;
    }

    // Same reasoning, and the same reason it runs after `updates`: this one
    // makes a network call per container, so it is the slower of the two.
    if config.modules.image_updates.enabled {
        check_image_updates_once(&config, &state).await;
    }

    supervise(&mut telegraf, &state, &config, &mut signals).await
}

/// Run the updates check once at startup, and record what it found.
///
/// Telegraf runs this same check on `modules.updates.interval` — hourly by
/// default — and those results go to the outputs. This one exists because an
/// hour is a long time to wait to discover that a deployment cannot read the
/// host's package state, and because `/status` should be able to answer the
/// question without a metrics database in the loop.
///
/// **A failure degrades muninn; it does not stop it.** That is the opposite of
/// the Docker module's rule, and deliberately so: an unreachable Docker endpoint
/// produces silence that reads as "no containers", while a failed update check
/// produces `check_success=0` with a reason. Nothing is being misrepresented, so
/// taking a working agent out of service would cost far more than it protects.
async fn check_updates_once(config: &Config, state: &HealthState) {
    use muninn_modules::updates;

    let hostfs = std::path::PathBuf::from(updates::host_prefix(config));
    let Some(scratch) = updates::scratch_directory(config) else {
        warn!(
            "the updates module is enabled but runtime.generated_config_path has no directory, \
             so apt has nowhere to write its cache"
        );
        state.record_module_check("updates", false);
        transition(state, State::Degraded);
        return;
    };

    // On a blocking thread: apt parses the host's entire package index, which is
    // seconds of CPU, and the reactor is also serving health checks.
    let report =
        tokio::task::spawn_blocking(move || updates::debian::check(&hostfs, &scratch)).await;

    let report = match report {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "the updates check did not complete");
            state.record_module_check("updates", false);
            transition(state, State::Degraded);
            return;
        }
    };

    state.record_module_check("updates", report.succeeded());

    match report.outcome {
        Ok(counts) => {
            info!(
                pending = counts.all,
                security = counts.security,
                lists_age_seconds = report.lists_age_seconds,
                "updates check"
            );
        }
        Err(reason) => {
            warn!(
                reason = reason.as_str(),
                detail = report.detail.as_deref().unwrap_or(""),
                "the updates check could not read the host's package state — muninn continues \
                 without it, and the metric reports the failure rather than a count"
            );
            transition(state, State::Degraded);
        }
    }
}

/// Run the image_updates check once at startup, and record what it found.
///
/// Telegraf runs this same check on `modules.image_updates.interval` — hourly
/// by default — for the reason [`check_updates_once`] already states: an hour
/// is a long time to wait to discover a deployment cannot reach the Docker
/// daemon, or a registry, at all.
///
/// **A failure degrades muninn; it does not stop it** — the same rule as
/// `updates`, and for the same reason. The daemon itself has already been
/// proven reachable by the runtime preconditions this module declares (the
/// same `GET /_ping` the docker module's endpoint gets, via
/// [`crate::probe::docker`]); what this check can still fail on is a single
/// container's registry lookup, which does not call for taking every other
/// module out of service.
async fn check_image_updates_once(config: &Config, state: &HealthState) {
    use muninn_modules::image_updates::check;

    let m = &config.modules.image_updates;
    let endpoint = m.endpoint.clone();
    let timeout = m.timeout.inner();
    let include = m.container_include.clone();
    let exclude = m.container_exclude.clone();

    // On a blocking thread: this makes one or more real network calls per
    // running container, and the reactor is also serving health checks.
    let report =
        tokio::task::spawn_blocking(move || check::check(&endpoint, timeout, &include, &exclude))
            .await;

    let report = match report {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "the image update check did not complete");
            state.record_module_check("image_updates", false);
            transition(state, State::Degraded);
            return;
        }
    };

    state.record_module_check("image_updates", report.daemon_succeeded());

    match report.daemon_outcome {
        Ok(count) => {
            let updates_available = report
                .containers
                .iter()
                .filter(|c| matches!(c.outcome, Ok(true)))
                .count();
            let failed = report
                .containers
                .iter()
                .filter(|c| c.outcome.is_err())
                .count();
            info!(
                containers_checked = count,
                updates_available, failed, "image update check"
            );
        }
        Err(reason) => {
            warn!(
                reason = reason.as_str(),
                detail = report.detail.as_deref().unwrap_or(""),
                "the image update check could not reach the Docker daemon — muninn continues \
                 without it, and the metric reports the failure rather than a verdict"
            );
            transition(state, State::Degraded);
        }
    }
}

/// Wait for a stop signal, or for Telegraf to die first.
async fn supervise(
    telegraf: &mut Telegraf,
    state: &HealthState,
    config: &Config,
    signals: &mut StopSignals,
) -> Result<()> {
    tokio::select! {
        // Telegraf exited on its own. Whatever the code, muninn did not ask for
        // this.
        exit = telegraf.wait() => {
            let exit = exit?;
            state.update(|d| {
                d.telegraf_pid = None;
                d.last_telegraf_exit = Some(exit.describe());
            });
            transition(state, State::Failed);
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
            transition(state, State::Stopping);

            let exit = telegraf
                .shutdown(config.runtime.shutdown_grace_period.inner())
                .await?;

            state.update(|d| {
                d.telegraf_pid = None;
                d.last_telegraf_exit = Some(exit.describe());
            });
            if !exit.is_clean_shutdown() {
                warn!(status = %exit.describe(), "Telegraf did not stop cleanly");
            }
            transition(state, State::Stopped);
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

#[cfg(test)]
mod tests {
    use super::*;

    // The state-machine tests moved to `muninn-health` with the state itself,
    // and writing the generated configuration to `generated_config`, next to the
    // permission rule it enforces. What is left is the supervisor's own state.

    /// A transition logs and moves; the state the health server reads is the one
    /// the supervisor last set.
    #[test]
    fn a_transition_moves_the_shared_state() {
        let health = HealthState::new();
        let observer = health.clone();
        transition(&health, State::Ready);
        assert_eq!(observer.get(), State::Ready);
        assert!(observer.is_ready());
    }
}
