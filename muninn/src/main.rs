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
//! # Status
//!
//! Steps 1–4 and 6–8 are implemented, reachable through `validate` and
//! `render-config`. Runtime checks (5), Telegraf validation (9) and supervision
//! (10–12) land in WP6 and WP8 — see `docs/roadmap.md`.

use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser;
use muninn_core::MuninnError;
use muninn_core::config::{self, Overrides};
use muninn_modules::RenderContext;

mod cli;

use cli::{Cli, Command};

fn main() -> ExitCode {
    let args = Cli::parse();

    match dispatch(&args) {
        Ok(()) => ExitCode::SUCCESS,
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
            // The Telegraf version comes from the binary at runtime, which WP6
            // adds. Stating "unknown" beats printing the build-time pin as
            // though it had been confirmed.
            println!("telegraf unknown (version check lands in WP6)");
            Ok(())
        }
        Command::Run => not_yet("run", "WP6 (Telegraf process management)"),
        Command::CheckRuntime => not_yet("check-runtime", "WP8 (container image)"),
        Command::Healthcheck => not_yet("healthcheck", "WP8 (container image)"),
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

fn validate(args: &Cli, with_telegraf: bool) -> muninn_core::Result<()> {
    let cfg = load(args)?;

    if with_telegraf {
        return Err(MuninnError::internal(
            "--with-telegraf is not implemented yet; it lands with the Telegraf validator in WP6"
                .to_string(),
        ));
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
            std::fs::write(path, &rendered).map_err(|e| {
                MuninnError::config(format!("cannot write '{}': {e}", path.display()))
            })?;
            eprintln!("muninn: wrote {}", path.display());
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(rendered.as_bytes())?;
        }
    }
    Ok(())
}

fn not_yet(command: &str, where_: &str) -> muninn_core::Result<()> {
    Err(MuninnError::internal(format!(
        "`{command}` is not implemented yet — it lands in {where_}. See docs/roadmap.md"
    )))
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

    /// An unimplemented command must exit non-zero and say where the work is —
    /// silently succeeding would be worse than failing.
    #[test]
    fn an_unimplemented_command_fails_with_a_pointer() {
        let err = not_yet("run", "WP6").unwrap_err();
        assert_eq!(err.exit_code(), muninn_core::exit::INTERNAL);
        assert!(err.to_string().contains("WP6"));
        assert!(err.to_string().contains("roadmap"));
    }
}
