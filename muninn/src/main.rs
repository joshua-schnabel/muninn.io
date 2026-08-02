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
//! Ordered, and every step can only fail *before* anything irreversible happens:
//!
//! ```text
//!  1 parse CLI arguments and environment
//!  2 read the YAML file
//!  3 validate schema version, then structure, then semantics
//!  4 read and check every referenced secret file
//!  5 check runtime preconditions for enabled modules (mounts, socket, host OS)
//!  6 initialise the enabled modules
//!  7 render a deterministic Telegraf configuration
//!  8 write it to the ephemeral runtime directory
//!  9 verify it with `telegraf config check`
//! 10 start Telegraf as a child process
//! 11 report readiness only once Telegraf is actually running
//! 12 supervise; forward signals; on stop, wait for a clean exit, then SIGKILL
//!    after the grace period
//! ```
//!
//! Steps 1–9 touch nothing outside the container's own tmpfs, so a bad config
//! costs an exit code and a log line — never a half-started agent.
//!
//! # Supervision, deliberately boring
//!
//! muninn does not restart Telegraf in a loop. If Telegraf dies, readiness goes
//! false immediately and muninn exits with
//! [`muninn_core::exit::TELEGRAF_EXITED`], leaving the restart decision to
//! Docker or the orchestrator. The failure mode this avoids is the expensive
//! one: a container that looks healthy from the outside while Telegraf
//! crash-loops invisibly inside it.
//!
//! # Commands
//!
//! `run` · `validate` · `render-config` · `check-runtime` · `healthcheck` ·
//! `version`. Precedence for every setting is CLI argument, then environment
//! variable, then YAML, then default.
//!
//! # Status
//!
//! This tree is the WP0 design package: documentation, schema and the verified
//! reference Telegraf configuration. The binary is not implemented yet — start
//! at `docs/roadmap.md`, then `docs/architecture.md`.

fn main() -> std::process::ExitCode {
    // Deliberately not a panic and not exit code 1: 1 is reserved for
    // unexpected failure, and this is an entirely expected state for the
    // design-package commit. Anyone who builds and runs the tree today should
    // be told where the work actually is.
    eprintln!(
        "muninn {} is not implemented yet — this tree is the design package (WP0).\n\
         Start with docs/roadmap.md, then docs/architecture.md.",
        env!("CARGO_PKG_VERSION"),
    );
    std::process::ExitCode::from(muninn_core::exit::INTERNAL)
}
