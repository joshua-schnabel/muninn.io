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
///
/// # Two halves, and why the seam is here
///
/// [`start`] does everything up to a confirmed-running Telegraf and may fail;
/// everything after it supervises one. The seam exists so that **every** way
/// startup can fail reaches [`State::Failed`] in one place rather than six.
///
/// It used to reach it in none. `Failed` was set on exactly one path — Telegraf
/// exiting after it had been confirmed running — while the version check, the
/// runtime preconditions, writing the configuration, `telegraf config check`,
/// the spawn and the start confirmation each returned an error and left the
/// state where it was. The health listener is bound and serving before any of
/// them, so `/health/live` answered 200 and `muninn_state` reported a startup
/// state right up to the moment the process exited non-zero (F-05).
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

    let mut telegraf = match start(&config, &state).await {
        Ok(t) => t,
        Err(e) => {
            // The one place startup failure becomes observable. Liveness goes
            // false here rather than at process exit, so a probe in the moments
            // between the failure and the exit reports the truth.
            transition(&state, State::Failed);
            return Err(e);
        }
    };

    // The PID is what makes `muninn_telegraf_running` true, so it is recorded
    // only once the process is confirmed — not at spawn time.
    state.update(|d| d.telegraf_pid = Some(telegraf.pid()));
    transition(&state, State::Ready);
    info!(pid = telegraf.pid(), "muninn is ready");

    // The self-checks run *beside* supervision, not before it.
    //
    // They used to be awaited here, between readiness and the `select!` that
    // multiplexes signals — so a check that did not return meant a SIGTERM
    // nothing answered, and `docker stop` reached its kill instead of stopping
    // (F-06). apt is bounded and killed on its own deadline now as well; both
    // are needed, because a `spawn_blocking` task cannot be cancelled and
    // dropping the runtime waits for it.
    let checks = tokio::spawn(self_checks(config.clone(), state.clone()));

    let result = supervise(&mut telegraf, &state, &config, &mut signals).await;

    // Nothing left to report to: the process is on its way out.
    checks.abort();
    result
}

/// Everything up to a Telegraf confirmed to be running.
///
/// Every error here is a startup failure, and [`run`] turns all of them into
/// [`State::Failed`].
async fn start(config: &Config, state: &HealthState) -> Result<Telegraf> {
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

    transition(state, State::CheckingRuntime);

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
    let findings = runtime_check::preconditions(config);
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

    transition(state, State::GeneratingTelegrafConfiguration);
    let generation_started = Instant::now();
    let rendered = muninn_telegraf::render(
        &muninn_modules::build(&RenderContext::new(config)),
        env!("CARGO_PKG_VERSION"),
    );
    let config_path = Path::new(&config.runtime.generated_config_path);
    crate::generated_config::write(config_path, &rendered)?;
    let generation = generation_started.elapsed();
    state.update(|d| d.config_generation = Some(generation));
    info!(path = %config_path.display(), bytes = rendered.len(), "wrote Telegraf configuration");

    transition(state, State::ValidatingTelegrafConfiguration);
    let validation_started = Instant::now();
    // The file being checked holds resolved secrets, so Telegraf's complaints
    // about it are scrubbed before they can reach the error — the same redactor
    // the child's stdout and stderr go through below.
    let redactor = config.redactor();
    validator::check_config(&binary, config_path, &redactor)?;
    let validation = validation_started.elapsed();
    state.update(|d| d.telegraf_validation = Some(validation));
    info!("Telegraf accepted the generated configuration");

    transition(state, State::StartingTelegraf);
    let host_env = config.runtime.host_env();
    // Everything Telegraf prints goes through muninn's logger, and the config it
    // is about to read holds resolved secrets — so the child's output is scrubbed
    // of them first. `Secret`'s type-level redaction cannot reach text another
    // process formatted.
    let mut telegraf = Telegraf::spawn(&binary, config_path, &host_env, redactor)?;

    // Readiness only after Telegraf is confirmed running. `config check`
    // initialises without starting, so up to this point nothing has proved the
    // process can actually run.
    confirm_running(&mut telegraf).await?;

    Ok(telegraf)
}

/// The startup self-checks, and their retry.
///
/// Runs beside [`supervise`], never before it — see [`run`].
///
/// # What these are for, and what they are not
///
/// Telegraf runs the same checks on each module's interval through
/// `inputs.exec`, and *those* results are the data path: they reach the
/// outputs. These exist because an hour is a long time to wait to discover
/// that a deployment cannot read the host's package state at all, and because
/// `/status` should be able to answer that without a metrics database in the
/// loop.
///
/// # Why a failure retries and a success does not
///
/// A successful check is a fact about startup and stays one; repeating it
/// hourly would mean apt parsing the host's entire package index twice per
/// interval, once for Telegraf and once for a number nobody reads differently
/// the second time.
///
/// A *failed* check is different, because it also sets [`State::Degraded`], and
/// a state with no way out is a state that outlives its cause. A registry that
/// was unreachable for two seconds during startup used to mark muninn degraded
/// for the life of the container (N-02). So a failure — and only a failure —
/// is retried on the module's own interval until it succeeds, at which point
/// `Degraded` clears and the retry stops.
async fn self_checks(config: Config, state: HealthState) {
    let mut tasks = Vec::new();

    if config.modules.updates.enabled {
        let (c, s) = (config.clone(), state.clone());
        tasks.push(tokio::spawn(async move {
            retry_until_it_works("updates", c.modules.updates.interval.inner(), &s, || {
                check_updates_once(&c, &s)
            })
            .await
        }));
    }

    // Independently, rather than after `updates`: one module's stalled host
    // mount is not a reason to delay the other's first result by an hour.
    if config.modules.image_updates.enabled {
        let (c, s) = (config.clone(), state.clone());
        tasks.push(tokio::spawn(async move {
            retry_until_it_works(
                "image_updates",
                c.modules.image_updates.interval.inner(),
                &s,
                || check_image_updates_once(&c, &s),
            )
            .await
        }));
    }

    for t in tasks {
        let _ = t.await;
    }
}

/// Run `check` now; if it failed, run it again every `interval` until it works.
///
/// `check` records its own result and sets [`State::Degraded`] on failure —
/// this only decides whether to ask again, and clears `Degraded` when the
/// answer finally changes.
async fn retry_until_it_works<F, Fut>(
    module: &str,
    interval: std::time::Duration,
    state: &HealthState,
    mut check: F,
) where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    if check().await {
        return;
    }

    loop {
        tokio::time::sleep(interval).await;

        // Nothing to retry towards once muninn is stopping, and a retry that
        // succeeded during shutdown would be reporting about a process that is
        // going away.
        if !matches!(state.get(), State::Ready | State::Degraded) {
            return;
        }

        if check().await {
            info!(
                module,
                "the module's self-check succeeded on a retry — no longer degraded"
            );
            clear_degraded_if_nothing_is_failing(state);
            return;
        }
    }
}

/// Leave [`State::Degraded`] once no module is reporting a failed self-check.
///
/// Conditional on the state still *being* `Degraded`: a plain `set` would
/// overwrite `Stopping` if a retry succeeded as a stop signal arrived, and
/// `/health/ready` would answer yes again while Telegraf is being torn down.
fn clear_degraded_if_nothing_is_failing(state: &HealthState) {
    let all_ok = state
        .details()
        .module_checks
        .values()
        .all(|check| check.success);
    if all_ok && state.transition_from(State::Degraded, State::Ready) {
        info!(from = "degraded", to = "ready", "state");
    }
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
///
/// The returned `bool` is what [`retry_until_it_works`] reads. It is the same
/// value recorded as `muninn_module_check_success`, so the retry decision and
/// the metric can never disagree.
async fn check_updates_once(config: &Config, state: &HealthState) -> bool {
    use muninn_modules::updates;

    let hostfs = std::path::PathBuf::from(updates::host_prefix(config));
    let Some(scratch) = updates::scratch_directory(config) else {
        warn!(
            "the updates module is enabled but runtime.generated_config_path has no directory, \
             so apt has nowhere to write its cache"
        );
        state.record_module_check("updates", false);
        transition(state, State::Degraded);
        // Nothing a retry can change — the configuration will not move under a
        // running process — but returning `false` keeps this function's
        // contract simple, and the retry costs one early return per interval.
        return false;
    };

    // On a blocking thread: apt parses the host's entire package index, which is
    // seconds of CPU, and the reactor is also serving health checks. apt itself
    // is bounded and killed at `APT_TIMEOUT`, because a blocking task cannot be
    // cancelled and dropping the runtime waits for it.
    let classify_security = config.modules.updates.security_only_metric;
    let report = tokio::task::spawn_blocking(move || {
        updates::debian::check(&hostfs, &scratch, updates::APT_TIMEOUT, classify_security)
    })
    .await;

    let report = match report {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "the updates check did not complete");
            state.record_module_check("updates", false);
            transition(state, State::Degraded);
            return false;
        }
    };

    let succeeded = report.succeeded();
    state.record_module_check("updates", succeeded);

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

    succeeded
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
///
/// **And it is bounded twice.** The check carries its own budget, so a host
/// with many containers reports the ones it did not reach rather than running
/// forever — but a single call blocked below the timeout the socket was given
/// is still possible. The wait is therefore capped as well, and a check that
/// overruns it is abandoned: the blocking thread cannot be cancelled, but
/// muninn stops waiting for it.
///
/// That cap used to be the *only* thing standing between a wedged Docker
/// socket and a SIGTERM nothing answered, because this ran before
/// [`supervise`] began multiplexing signals. It now runs beside it, which is
/// the structural fix; the cap stays, because a blocking task the runtime
/// waits for at shutdown is still worth bounding (F-06).
async fn check_image_updates_once(config: &Config, state: &HealthState) -> bool {
    use muninn_modules::image_updates::{budget, check, exec_timeout};

    let m = &config.modules.image_updates;
    let endpoint = m.endpoint.clone();
    let timeout = m.timeout.inner();
    let registry_timeout = m.registry_timeout.inner();
    let budget = budget(m.interval.inner());
    let include = m.container_include.clone();
    let exclude = m.container_exclude.clone();

    // Resolved here, in the agent that already holds a validated configuration,
    // rather than passed down as flags: the rendered `inputs.exec` command line
    // is what Telegraf executes, so a credential on it would sit in the
    // generated config and in the process table both.
    //
    // An unreadable file drops that one entry and is logged. The containers on
    // that registry then report `distribution_query_failed` — a per-container
    // failure with a cause an operator can act on, rather than a check that
    // refuses to run at all and takes the public registries down with it.
    let (registry_auth, problems) =
        muninn_modules::image_updates::registry_auth::resolve(&m.registry_auth);
    for p in &problems {
        warn!(detail = %p, "a registry credential could not be read");
    }

    // The same cap Telegraf puts on the same check, for the same reason. If
    // the check honoured its budget this never fires.
    let cap = exec_timeout(m.interval.inner());

    // On a blocking thread: this makes one or more real network calls per
    // running container, and the reactor is also serving health checks.
    let task = tokio::task::spawn_blocking(move || {
        check::check(
            &endpoint,
            timeout,
            registry_timeout,
            budget,
            &include,
            &exclude,
            registry_auth,
        )
    });

    let report = match tokio::time::timeout(cap, task).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            warn!(error = %e, "the image update check did not complete");
            state.record_module_check("image_updates", false);
            transition(state, State::Degraded);
            return false;
        }
        Err(_) => {
            warn!(
                after_seconds = cap.as_secs(),
                "the image update check did not return in time and was abandoned — muninn \
                 continues supervising, and Telegraf runs the same check on its own schedule"
            );
            state.record_module_check("image_updates", false);
            transition(state, State::Degraded);
            return false;
        }
    };

    // Every selected container has to have a verdict, not merely the daemon
    // having answered. The aggregate used to be `daemon_succeeded()`, so it
    // reported success while every container carried a failure reason (F-11).
    let succeeded = report.succeeded();
    state.record_module_check("image_updates", succeeded);

    match report.daemon_outcome {
        Ok(count) => {
            let updates_available = report
                .containers
                .iter()
                .filter(|c| matches!(c.outcome, Ok(true)))
                .count();
            let (with_verdict, selected) = report.verdicts();
            let failed = selected - with_verdict;
            info!(
                containers_checked = count,
                updates_available, failed, "image update check"
            );
            if failed > 0 {
                // The daemon answered, so this is not a deployment problem —
                // it is some containers muninn could not answer for, and the
                // per-container series name which and why.
                warn!(
                    failed,
                    selected,
                    "the image update check reached the Docker daemon but could not produce a \
                     verdict for every container — the module reports failure rather than a \
                     partial answer, and the per-container metrics carry the reason"
                );
                transition(state, State::Degraded);
            }
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

    succeeded
}

/// Wait for a stop signal, or for Telegraf to die first.
///
/// # When both happen at once
///
/// A container being stopped while Telegraf crashes in the same instant leaves
/// two arms ready together, and the two answers are opposite: exit 0 with a
/// clean `Stopped`, or exit 22 with `Failed`. `tokio::select!` picks a ready arm
/// at random by default, so the same event could be reported either way from one
/// run to the next — which is worse than either answer, because an orchestrator's
/// restart policy is written against the code.
///
/// `biased` makes it a rule instead: **a stop signal wins.** muninn was asked to
/// stop, and turning an operator's `docker stop` into a crash code would invite
/// a restart into a container that was deliberately being taken down. The crash
/// is not swallowed — the shutdown path already reaps the child, records the
/// real exit in `/status`, and warns when it was not clean, so a Telegraf that
/// died on the way out is visible in the logs and the diagnostics. Only the exit
/// code says "you asked for this".
async fn supervise(
    telegraf: &mut Telegraf,
    state: &HealthState,
    config: &Config,
    signals: &mut StopSignals,
) -> Result<()> {
    tokio::select! {
        biased;

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
    }
}

/// How long muninn waits before deciding Telegraf did not fall over at once.
///
/// A fixed window rather than a configurable one, and that is the whole of
/// finding F-03 of the 1.0 review. `runtime.telegraf_start_timeout` was
/// documented as the deadline for Telegraf to become ready, and was only ever
/// a cap on this sleep — so every value above it did nothing, and a value below
/// it shortened a window that is not the operator's to tune.
///
/// There is no honest configurable deadline to offer in its place, because
/// muninn has no *measurable* readiness signal from Telegraf: `config check`
/// initialises without starting, and the running process announces nothing
/// muninn observes. What it can measure is "did it die immediately", which
/// needs a settle window, not a deadline. So the window stays a constant with
/// its reason next to it rather than a key that reads like a guarantee.
const SETTLE_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

/// Confirm Telegraf is still alive a moment after spawning.
///
/// A binary that exits immediately — a config Telegraf accepts at check time but
/// refuses at start, a missing shared library — would otherwise be reported
/// ready. `config check` cannot see this, because initialising is not running.
async fn confirm_running(telegraf: &mut Telegraf) -> Result<()> {
    tokio::time::sleep(SETTLE_WINDOW).await;

    match telegraf.try_exit()? {
        None => Ok(()),
        Some(exit) => {
            // The one case where Telegraf's own last words *are* the diagnosis:
            // the configuration passed `config check`, so whatever it said on
            // the way down is all there is to go on. Drained before the error is
            // built, so those lines are logged ahead of it rather than lost when
            // the runtime unwinds (F-10).
            telegraf.drain_output().await;
            Err(MuninnError::TelegrafStart(format!(
                "Telegraf exited immediately after starting, with {}. The generated \
                 configuration passed `config check`, so this is something only visible at run \
                 time — a missing mount, an address already in use, or a permission it does not \
                 have",
                exit.describe()
            )))
        }
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

    /// The way out of `Degraded` that did not exist before N-02: one module's
    /// retry succeeding, with nothing else failing.
    #[test]
    fn a_successful_retry_leaves_degraded() {
        let health = HealthState::new();
        health.record_module_check("updates", false);
        transition(&health, State::Degraded);

        health.record_module_check("updates", true);
        clear_degraded_if_nothing_is_failing(&health);

        assert_eq!(health.get(), State::Ready);
    }

    /// But only when *nothing* is failing. Two modules degrade independently,
    /// and one recovering does not speak for the other — clearing on the first
    /// success would report a health muninn does not have.
    #[test]
    fn one_module_recovering_does_not_clear_another_failure() {
        let health = HealthState::new();
        health.record_module_check("updates", false);
        health.record_module_check("image_updates", false);
        transition(&health, State::Degraded);

        health.record_module_check("updates", true);
        clear_degraded_if_nothing_is_failing(&health);
        assert_eq!(health.get(), State::Degraded, "image_updates still fails");

        health.record_module_check("image_updates", true);
        clear_degraded_if_nothing_is_failing(&health);
        assert_eq!(health.get(), State::Ready);
    }

    /// A retry that completes during shutdown must not put readiness back.
    #[test]
    fn a_retry_during_shutdown_does_not_reopen_readiness() {
        let health = HealthState::new();
        health.record_module_check("updates", true);
        transition(&health, State::Stopping);

        clear_degraded_if_nothing_is_failing(&health);

        assert_eq!(health.get(), State::Stopping);
        assert!(!health.is_ready());
    }

    /// The settle window is muninn's own number now, not an operator's.
    /// `runtime.telegraf_start_timeout` used to cap it and was documented as
    /// something else entirely (F-03).
    #[test]
    fn the_settle_window_is_short_enough_not_to_delay_a_healthy_start() {
        assert!(SETTLE_WINDOW <= std::time::Duration::from_secs(1));
    }
}
