//! muninn.io — supervisor and configuration layer around Telegraf.
//!
//! muninn is PID 1 in its container. It reads one small YAML file, generates a
//! complete Telegraf configuration from it, has Telegraf verify that
//! configuration, starts Telegraf as a child process, and then stays alive
//! supervising it and answering health checks. Telegraf remains the telemetry
//! engine; muninn is the layer that makes it configurable without knowing TOML.
//!
//! # Startup sequence
//!
//! Ordered, and every step can only fail *before* anything irreversible
//! happens:
//!
//! ```text
//!  1 parse CLI arguments and environment
//!  2 read the YAML file
//!  3 validate schema version, then structure, then semantics
//!  4 read and check every referenced secret file
//!  5 check runtime preconditions for enabled modules
//!  6 initialise the enabled modules
//!  7 render a deterministic Telegraf configuration
//!  8 write it to the ephemeral runtime directory
//!  9 verify it with `telegraf config check`
//! 10 start Telegraf as a child process
//! 11 report readiness only once Telegraf is actually running
//! 12 supervise; forward signals; SIGKILL after the grace period
//! ```
//!
//! Steps 1–9 touch nothing outside the container's own tmpfs, so a bad config
//! costs an exit code and a log line — never a half-started agent.
//!
//! Steps 1–8 are reachable on their own through `validate` and `render-config`,
//! which start nothing. A module that checks itself — today, updates — does so
//! after step 11, because holding readiness for a check that takes seconds would
//! delay an orchestrator over something unrelated to collecting metrics.

use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser;
use muninn_core::MuninnError;
use muninn_core::config::{self, Overrides};
use muninn_modules::RenderContext;
use muninn_modules::image_updates::registry_auth::{self, RegistryCredential};

mod cli;
mod generated_config;
mod logging;
mod probe;
mod runtime_check;
mod supervisor;

use cli::{Cli, Command};

fn main() -> ExitCode {
    let args = Cli::parse();

    match dispatch(&args) {
        // Through the constant, not `ExitCode::SUCCESS`. `exit::OK` was
        // declared as part of the exit-code contract and referenced by nothing,
        // so the module that calls itself "the single source of truth the code
        // uses" was not the source of this one (N-05).
        Ok(()) => ExitCode::from(muninn_core::exit::OK),
        Err(e) => {
            // Errors are printed rather than logged: they can occur before the
            // subscriber exists, and a startup failure should reach stderr the
            // same way whatever the configured log format is.
            eprintln!("muninn: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}

fn dispatch(args: &Cli) -> muninn_core::Result<()> {
    match args.selected() {
        Command::Validate { with_telegraf } => validate(args, *with_telegraf),
        Command::RenderConfig {
            output,
            unsafe_show_secrets,
        } => render_config(args, output.as_deref(), *unsafe_show_secrets),
        Command::Version => {
            println!("muninn {}", env!("CARGO_PKG_VERSION"));
            println!(
                "built for telegraf {}",
                muninn_telegraf::version::EXPECTED_VERSION
            );
            // Reported separately from the pin, because "what we were built for"
            // and "what is actually here" are different facts and only the
            // second one can be wrong.
            let binary = muninn_telegraf::version::binary_path();
            match muninn_telegraf::version::query(&binary) {
                Ok(v) => println!("telegraf {v} at {}", binary.display()),
                Err(e) => println!("telegraf not available: {e}"),
            }
            Ok(())
        }
        Command::Run => run(args),
        Command::CheckRuntime => check_runtime(args),
        Command::Healthcheck => healthcheck(args),
        Command::UpdateCheck {
            hostfs,
            no_security_metric,
        } => {
            update_check(hostfs.as_deref(), !no_security_metric);
            Ok(())
        }
        Command::ImageCheck {
            endpoint,
            timeout_secs,
            registry_timeout_secs,
            budget_secs,
            include,
            exclude,
        } => {
            image_check(
                endpoint,
                *timeout_secs,
                *registry_timeout_secs,
                *budget_secs,
                include,
                exclude,
                &args.config,
            );
            Ok(())
        }
    }
}

/// Load, validate and resolve, emitting any warnings.
fn load(args: &Cli) -> muninn_core::Result<muninn_core::Config> {
    let overrides =
        Overrides::from_env().merge_cli(args.log_level.clone(), args.log_format.clone());
    let (cfg, warnings) = config::load_and_resolve(&args.config, &overrides)?;

    // Warnings go to stderr so they do not contaminate `render-config` output
    // that a caller may be piping into a file.
    for w in &warnings {
        eprintln!("muninn: warning: {w}");
    }

    Ok(cfg)
}

/// Where `validate --with-telegraf` may put a file that holds resolved secrets.
///
/// The directory of `runtime.generated_config_path`, when it already exists —
/// in the shipped image that is the tmpfs at `/run/muninn`, and it is the only
/// writable place there: the root filesystem is read-only and `/tmp` with it,
/// so `tempfile` alone fails with `Read-only file system` on exactly the
/// deployment this command is most useful in.
///
/// Only when it already exists. Run on a developer's machine against a
/// production configuration, creating `/run/muninn` would be muninn making a
/// directory outside a container to hold a credential — the system temp
/// directory is both writable and correct there.
fn scratch_directory(generated_config_path: &str) -> Option<std::path::PathBuf> {
    let dir = std::path::Path::new(generated_config_path).parent()?;
    dir.is_dir().then(|| dir.to_path_buf())
}

fn validate(args: &Cli, with_telegraf: bool) -> muninn_core::Result<()> {
    let cfg = load(args)?;

    if with_telegraf {
        // Render to a temporary file and let Telegraf judge it. Opt-in because
        // it needs the binary, which in practice means running inside the image.
        let binary = muninn_telegraf::version::binary_path();
        muninn_telegraf::version::check(&binary)?;

        let rendered = muninn_telegraf::render(
            &muninn_modules::build(&RenderContext::new(&cfg)),
            env!("CARGO_PKG_VERSION"),
        );

        // The scratch file holds resolved secrets, so it goes through the same
        // writer as the real one and is removed as soon as Telegraf has read it.
        //
        // A random name, not `telegraf.check.<pid>.conf`. A pid is guessable and
        // reusable, so that name could be pre-placed — as a symlink, which the
        // writer would then have followed, or simply to be read afterwards.
        // `mkstemp` names it and `O_EXCL` means muninn creates it or fails
        // (F-09).
        let _guard;
        let dir = match scratch_directory(&cfg.runtime.generated_config_path) {
            Some(dir) => dir,
            None => {
                _guard = tempfile::tempdir().map_err(|e| {
                    MuninnError::internal(format!("cannot create a scratch directory: {e}"))
                })?;
                _guard.path().to_path_buf()
            }
        };
        // `into_temp_path` drops the open handle and keeps the name and the
        // delete-on-drop. The handle has to go: the writer below renames a fresh
        // file onto this path, and on Windows a rename over an open file fails.
        let scratch = tempfile::Builder::new()
            .prefix(".muninn-check-")
            .suffix(".conf")
            .tempfile_in(&dir)
            .map_err(|e| {
                MuninnError::internal(format!(
                    "cannot create a scratch file in '{}': {e}",
                    dir.display()
                ))
            })?
            .into_temp_path();
        let path = scratch.to_path_buf();
        generated_config::write(&path, &rendered)?;

        let verdict = muninn_telegraf::validator::check_config(&binary, &path, &cfg.redactor());

        // Before the `?`: a rejected configuration is the expected outcome of
        // this command and must not leave a file holding a token behind.
        //
        // And a failure to remove it is an error rather than a shrug. It was
        // `let _ = remove_file(...)`, which meant the one case worth hearing
        // about — a credential still on disk — was the one case that said
        // nothing. Reported even when Telegraf's own verdict is also an error,
        // because the leftover file is the more urgent of the two.
        let removed = scratch.close();
        verdict?;
        removed.map_err(|e| {
            MuninnError::internal(format!(
                "the scratch configuration at '{}' could not be removed: {e}. It holds resolved \
                 secrets — delete it by hand",
                path.display()
            ))
        })?;

        println!("Telegraf accepted the generated configuration.");
    }

    println!("{} is valid.", args.config.display());
    println!("  modules: {}", join(&cfg.modules.enabled_names()));
    println!(
        "  outputs: {}",
        join(
            &cfg.outputs
                .influxdb
                .iter()
                .map(|_| "influxdb")
                .chain(cfg.outputs.prometheus.iter().map(|_| "prometheus"))
                .collect::<Vec<_>>()
        )
    );
    Ok(())
}

fn render_config(
    args: &Cli,
    output: Option<&std::path::Path>,
    show_secrets: bool,
) -> muninn_core::Result<()> {
    let cfg = load(args)?;

    if show_secrets {
        eprintln!(
            "muninn: warning: --unsafe-show-secrets is set. The output below contains real \
             credentials in plaintext — do not paste it anywhere."
        );
    }

    let ctx = if show_secrets {
        RenderContext::new(&cfg)
    } else {
        RenderContext::redacted(&cfg)
    };
    let rendered = muninn_telegraf::render(&muninn_modules::build(&ctx), env!("CARGO_PKG_VERSION"));

    match output {
        Some(path) => {
            // The same writer the supervisor uses, so the file lands 0600
            // whichever command produced it. With --unsafe-show-secrets this
            // output holds a real token, and a redacted one still describes the
            // deployment; neither is something to leave world-readable in
            // whatever directory the operator happened to point at.
            generated_config::write(path, &rendered)?;
            eprintln!("muninn: wrote {}", path.display());
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(rendered.as_bytes())?;
        }
    }
    Ok(())
}

/// Report every unmet runtime precondition.
///
/// Separate from `validate` because it answers a different question: `validate`
/// asks whether the configuration is coherent, this asks whether the machine can
/// do what it says. Hence exit 12 rather than 10 — the YAML is right, the
/// deployment is not.
fn check_runtime(args: &Cli) -> muninn_core::Result<()> {
    let cfg = load(args)?;
    let findings = runtime_check::check(&cfg);

    if findings.is_empty() {
        println!(
            "{}: every runtime precondition is met.",
            args.config.display()
        );
        return Ok(());
    }

    for f in &findings {
        let label = match f.severity {
            runtime_check::Severity::Error => "error",
            runtime_check::Severity::Warning => "warning",
        };
        println!("{label}: {}: {}", f.subject, f.message);
    }

    if runtime_check::has_errors(&findings) {
        let count = findings
            .iter()
            .filter(|f| f.severity == runtime_check::Severity::Error)
            .count();
        Err(MuninnError::runtime(format!(
            "{count} runtime precondition(s) not met — see the report above"
        )))
    } else {
        println!(
            "
No blocking problems; the warnings above are worth reading."
        );
        Ok(())
    }
}

/// Query the local health endpoint, for a container `HEALTHCHECK`.
///
/// Reads the configuration only to learn where to look, and now genuinely so:
/// it takes `health.listen` through a probe that validates nothing else, reads
/// no secret and no TLS file, and ignores every other key.
///
/// That is the whole of F-08. The doc comment here always claimed as much,
/// while the code called the full pipeline — so a configuration edited after
/// startup, or a secret mount that went away, marked a **healthy running
/// process** unhealthy. The container health check runs on a short interval
/// with retries, so the orchestrator would then restart the container into the
/// broken configuration: a diagnostic mismatch turned into an outage, which is
/// exactly the failure this comment said it was avoiding.
///
/// A raw request rather than an HTTP client: this runs on every health-check
/// interval inside the container, and one endpoint on loopback does not justify
/// pulling in a TLS stack and a connection pool.
fn healthcheck(args: &Cli) -> muninn_core::Result<()> {
    use std::io::{Read as _, Write as _};

    let listen = config::loader::health_listen(&args.config)?;

    // Port 0 means "any free port", which the running instance was given by the
    // kernel and this process has no way to learn — a separate process cannot
    // read another's ephemeral port out of a configuration file. Saying so is
    // better than connecting to port 0 and reporting the container unhealthy
    // for a reason that has nothing to do with its health (F-18).
    if listen.port() == 0 {
        return Err(MuninnError::runtime(format!(
            "health.listen is '{listen}' — port 0 asks the kernel for any free port, so this \
             command cannot know which one the running instance was given. `muninn healthcheck` \
             needs a fixed port; the container's default is 8080"
        )));
    }

    let mut stream =
        std::net::TcpStream::connect_timeout(&listen, std::time::Duration::from_secs(3)).map_err(
            |e| MuninnError::runtime(format!("cannot reach the health endpoint on {listen}: {e}")),
        )?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .ok();

    stream
        .write_all(b"GET /health/ready HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .map_err(|e| MuninnError::runtime(format!("health request failed: {e}")))?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| MuninnError::runtime(format!("health response failed: {e}")))?;

    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| MuninnError::runtime("health endpoint returned no status line"))?;

    if status == 200 {
        println!("ready");
        Ok(())
    } else {
        // Non-zero so Docker marks the container unhealthy. The body says which
        // state it is in, which is the useful part in `docker inspect`.
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, b)| b)
            .unwrap_or("");
        Err(MuninnError::runtime(format!(
            "not ready (HTTP {status}): {}",
            body.trim()
        )))
    }
}

/// Report the host's pending package updates, as Telegraf's `inputs.exec` reads
/// them.
///
/// Returns nothing and takes no `Result`: this command has no failure mode that
/// belongs in an exit code. Whatever happens, the line protocol on stdout is the
/// answer — either counts, or `check_success=0` with a reason — and stderr
/// carries the detail for the log. See `crates/muninn-modules/src/updates/`.
fn update_check(hostfs: Option<&std::path::Path>, security_metric: bool) {
    use muninn_modules::updates::debian;

    let hostfs = hostfs.unwrap_or(std::path::Path::new("/"));
    // Honours TMPDIR, which the rendered configuration sets to the runtime
    // directory — the one writable place a read-only deployment has. Run by
    // hand on a host it is the shared /tmp instead, which is what the rule
    // below is about: `Scratch::create` answers it by creating an
    // unpredictably-named directory exclusively and 0700, so an existing path
    // is an error rather than somewhere to write. See
    // crates/muninn-modules/src/updates/debian.rs.
    // nosemgrep: rust.lang.security.temp-dir.temp-dir
    let scratch = std::env::temp_dir();

    // The second pass that separates the security subset runs only when there
    // is a security series to publish — `--no-security-metric` is exactly the
    // operator saying there is not.
    let report = debian::check(
        hostfs,
        &scratch,
        muninn_modules::updates::APT_TIMEOUT,
        security_metric,
    );

    if let Some(detail) = &report.detail {
        // Telegraf logs the plugin's stderr, so this is where an operator finds
        // the path or the apt error behind a low-cardinality reason tag.
        eprintln!("muninn: update check: {detail}");
    }

    print!("{}", report.line_protocol(security_metric));
}

/// Report, per running container, whether a newer image is available under
/// the tag it is running, as Telegraf's `inputs.exec` reads it.
///
/// Like [`update_check`], this has no failure mode that belongs in an exit
/// code: the line protocol on stdout is the answer, whether that is a verdict
/// per container or `check_success=0` with a reason, and stderr carries the
/// detail. See `crates/muninn-modules/src/image_updates/`.
fn image_check(
    endpoint: &str,
    timeout_secs: u64,
    registry_timeout_secs: u64,
    budget_secs: u64,
    include: &[String],
    exclude: &[String],
    config_path: &std::path::Path,
) {
    use muninn_modules::image_updates::check;
    use std::time::Duration;

    let report = check::check(
        endpoint,
        Duration::from_secs(timeout_secs),
        Duration::from_secs(registry_timeout_secs),
        Duration::from_secs(budget_secs),
        include,
        exclude,
        registry_credentials(config_path),
    );

    if let Some(detail) = &report.detail {
        eprintln!("muninn: image update check: {detail}");
    }
    for c in &report.containers {
        if let Some(detail) = &c.detail {
            eprintln!(
                "muninn: image update check: container '{}' ({}): {detail}",
                c.name, c.image
            );
        }
    }

    print!("{}", report.line_protocol());
}

/// The registry credentials from the configuration, each password read from
/// the file the configuration names.
///
/// Read here rather than passed as flags, and this is the reason the whole
/// feature is shaped this way: the rendered `inputs.exec` command line is what
/// Telegraf executes, so a credential on it would sit in the generated config
/// *and* in the process table. Secrets are file paths in this project, and a
/// path resolved inside the process that uses it is the only form that keeps
/// them out of argv.
///
/// **Deliberately non-fatal.** A missing or unreadable configuration leaves
/// the list empty, and an unreadable password file drops that one entry. Both
/// then surface as `distribution_query_failed` on exactly the containers
/// affected — a per-container failure an operator can act on, where refusing to
/// run would take down the check for every container including the public ones
/// that never needed a credential. Nothing reports a healthy value either way,
/// which is the invariant that matters.
fn registry_credentials(config_path: &std::path::Path) -> Vec<RegistryCredential> {
    let overrides = Overrides::from_env();
    let (cfg, _warnings) = match config::load(config_path, &overrides) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!(
                "muninn: image update check: no registry credentials — {} could not be read: {e}",
                config_path.display()
            );
            return Vec::new();
        }
    };

    let (credentials, problems) = registry_auth::resolve(&cfg.modules.image_updates.registry_auth);
    // Names the registry and the path, never the contents — the rule every
    // other secret error in this project follows.
    for p in &problems {
        eprintln!("muninn: image update check: {p}");
    }
    credentials
}

/// The full lifecycle. This is what the container runs.
fn run(args: &Cli) -> muninn_core::Result<()> {
    let cfg = load(args)?;

    // Logging is initialised only here. The other commands print to stdout and
    // stderr directly: a structured log line is the wrong shape for
    // `render-config` output that someone is piping into a file.
    logging::init(&cfg.logging);

    let health = muninn_health::HealthState::new();

    // A current-thread runtime would be enough for one child and a few tasks,
    // but the health server (WP7) and the output forwarders are genuinely
    // concurrent, and the multi-threaded runtime is what tokio's own defaults
    // assume.
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| MuninnError::internal(format!("cannot start the async runtime: {e}")))?;

    runtime.block_on(async move {
        // Bind before spawning so a port collision is reported as a startup
        // failure rather than as a log line from a task nobody is watching.
        let listener = muninn_health::bind(cfg.health.listen).await.map_err(|e| {
            MuninnError::runtime(format!(
                "cannot bind the health listener on {}: {e}. In a container this must be                  0.0.0.0 — a published port never reaches the container's loopback",
                cfg.health.listen
            ))
        })?;

        // What the kernel actually gave us, which with port 0 is not what the
        // configuration says. Recorded before anything else can fail, so it is
        // on `/status` for the whole life of the process (F-18).
        match listener.local_addr() {
            Ok(bound) => {
                tracing::info!(%bound, "health server listening");
                health.update(|d| d.bound_listen = Some(bound.to_string()));
            }
            // Not fatal: the listener is bound, and not being able to name it
            // costs a diagnostic rather than a capability.
            Err(e) => tracing::warn!(error = %e, "cannot determine the bound health address"),
        }

        // The server is stopped by dropping this sender, which happens when the
        // supervisor returns — so shutdown needs no separate signalling path.
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server_state = muninn_health::ServerState {
            health: health.clone(),
            muninn_version: env!("CARGO_PKG_VERSION"),
        };
        let mut server = tokio::spawn(async move {
            muninn_health::serve_on(listener, server_state, async {
                let _ = stop_rx.await;
            })
            .await
        });

        // The health server is supervised, not merely spawned.
        //
        // Its result and its join result were both discarded, so a listener
        // that died — an accept loop error it could not recover from, a panic
        // in a handler — left muninn running and reporting nothing, while
        // `docs/supervision.md` promised a permanently failed health server
        // exits. An orchestrator polling `/health/ready` then sees connection
        // refused and restarts the container, which is the right outcome
        // reached by the wrong route: muninn should say why (F-05).
        let result = tokio::select! {
            r = supervisor::run(cfg, health) => r,
            joined = &mut server => {
                return Err(health_server_died(joined));
            }
        };

        drop(stop_tx);
        let _ = server.await;
        result
    })
}

/// Turn the health task ending early into the error muninn exits on.
///
/// Three ways it can end before the supervisor asks it to, and they are worth
/// distinguishing because only one of them is a muninn bug:
///
/// - the serve loop returned an I/O error — the listener is gone;
/// - the task panicked — a bug in a handler;
/// - `Ok(())`, meaning it observed a shutdown nobody signalled.
///
/// All three exit [`muninn_core::exit::INTERNAL`], which is the code
/// `docs/supervision.md` documents for a health server that will not come back.
fn health_server_died(joined: Result<std::io::Result<()>, tokio::task::JoinError>) -> MuninnError {
    let detail = match joined {
        Ok(Err(e)) => format!("the listener stopped with an error: {e}"),
        Ok(Ok(())) => "the listener stopped without being asked to".to_string(),
        Err(e) if e.is_panic() => "a request handler panicked".to_string(),
        Err(e) => format!("the health task ended unexpectedly: {e}"),
    };
    MuninnError::internal(format!(
        "the health server is gone while muninn is still supervising Telegraf — {detail}. \
         muninn exits rather than run on without the endpoints an orchestrator uses to \
         decide whether it is alive"
    ))
}

fn join(items: &[&str]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_names_an_empty_list_rather_than_printing_nothing() {
        assert_eq!(join(&[]), "none");
        assert_eq!(join(&["cpu", "memory"]), "cpu, memory");
    }

    /// The tmpfs the deployment provides is the one writable directory in the
    /// image, and the scratch file belongs there rather than in a `/tmp` the
    /// read-only root filesystem does not offer.
    #[test]
    fn the_scratch_directory_is_the_runtime_directory_when_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let generated = dir.path().join("telegraf.conf");

        assert_eq!(
            scratch_directory(&generated.display().to_string()).as_deref(),
            Some(dir.path())
        );
    }

    /// And nothing is created when it does not: `muninn validate --with-telegraf`
    /// on a laptop, against a production configuration, must not make a
    /// directory outside a container to hold a resolved credential.
    #[test]
    fn no_scratch_directory_is_invented_when_the_runtime_one_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("run").join("muninn").join("telegraf.conf");

        assert_eq!(scratch_directory(&absent.display().to_string()), None);
        assert!(!absent.parent().unwrap().exists());
    }
}
