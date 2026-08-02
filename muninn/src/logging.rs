//! Logging setup.
//!
//! Two formats behind one switch: `human` for reading, `json` for shipping. JSON
//! emits one complete object per line, so a log pipeline can parse it without
//! reassembling anything.
//!
//! Output goes to stdout and stderr. There are no log files in the container: a
//! file inside a container is a second copy nobody reads and nobody rotates, and
//! the orchestrator is already collecting the streams.

use muninn_core::config::model::{LogFormat, LoggingConfig};

use tracing_subscriber::EnvFilter;

/// Initialise the subscriber.
///
/// Called once, and only by `run`. The other commands write to stdout directly —
/// a structured log line is the wrong shape for `render-config` output that
/// someone is piping into a file.
pub fn init(config: &LoggingConfig) {
    // RUST_LOG wins when set, because that is what someone reaches for while
    // debugging and they should not have to edit the YAML to raise a level for
    // one run.
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(config.level.as_str()));

    match config.format {
        LogFormat::Json => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .json()
                // The source of a Telegraf line is carried as a field rather
                // than baked into the message, so a log pipeline can filter on
                // it.
                .flatten_event(true)
                .init();
        }
        LogFormat::Human => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(true)
                .init();
        }
    }
}

#[cfg(test)]
mod tests {
    use muninn_core::config::model::LogLevel;

    /// The level reaches `EnvFilter` as a string, so a rename that broke the
    /// mapping would silently produce a filter that matches nothing.
    #[test]
    fn every_level_is_a_directive_env_filter_understands() {
        for level in [
            LogLevel::Trace,
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
        ] {
            let directive = level.as_str();
            assert!(
                super::EnvFilter::try_new(directive).is_ok(),
                "{directive} is not a filter directive EnvFilter accepts"
            );
        }
    }
}
