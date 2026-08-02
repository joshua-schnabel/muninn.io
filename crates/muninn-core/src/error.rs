//! The error type, and its mapping onto the process exit codes.
//!
//! Errors carry enough structure for the caller to know *which* exit code to
//! use, so no call site has to invent one. See [`crate::exit`] and
//! `docs/supervision.md`.

use thiserror::Error;

use crate::exit;

pub type Result<T> = std::result::Result<T, MuninnError>;

#[derive(Debug, Error)]
pub enum MuninnError {
    /// The configuration is unreadable, malformed, or breaks a rule.
    ///
    /// The message must name the offending YAML key. An operator reading
    /// "configuration error: invalid value" learns nothing; one reading
    /// "outputs.prometheus.listen: ..." knows exactly what to edit.
    #[error("configuration error: {0}")]
    Config(String),

    /// A secret file is missing, unreadable or empty.
    ///
    /// Note the shape: `path` and `message`, never the file's contents. This is
    /// the reason secrets get their own variant rather than folding into
    /// [`MuninnError::Config`] — a variant that cannot carry the value cannot
    /// leak it.
    #[error("secret file '{path}': {message}")]
    Secret { path: String, message: String },

    /// A runtime precondition for an enabled module is absent: an unmounted host
    /// path, an unreachable socket, an unsupported host OS.
    ///
    /// Distinct from [`MuninnError::Config`] because the fix is different. The
    /// configuration is right; the deployment around it is not.
    #[error("runtime requirement: {0}")]
    Runtime(String),

    /// The generated Telegraf configuration was rejected by `telegraf config
    /// check`. Never operator error — they never write TOML.
    #[error("generated Telegraf configuration was rejected: {0}")]
    TelegrafConfig(String),

    /// Telegraf could not be started, or reports an unexpected version.
    #[error("Telegraf did not start: {0}")]
    TelegrafStart(String),

    /// Telegraf exited on its own while being supervised.
    #[error("Telegraf exited unexpectedly: {0}")]
    TelegrafExited(String),

    /// An I/O failure with no more specific meaning.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// An invariant broke. Always a bug.
    #[error("internal error: {0}")]
    Internal(String),
}

impl MuninnError {
    /// The process exit code this error should produce.
    ///
    /// Centralised so that adding a variant forces a decision here, rather than
    /// each call site picking a number and the program growing three different
    /// codes for "bad config".
    pub fn exit_code(&self) -> u8 {
        match self {
            MuninnError::Config(_) => exit::CONFIG,
            MuninnError::Secret { .. } => exit::SECRET,
            MuninnError::Runtime(_) => exit::RUNTIME,
            MuninnError::TelegrafConfig(_) => exit::TELEGRAF_CONFIG,
            MuninnError::TelegrafStart(_) => exit::TELEGRAF_START,
            MuninnError::TelegrafExited(_) => exit::TELEGRAF_EXITED,
            // An I/O error that reached here without being classified is a gap
            // in the code that produced it, not something an operator can fix.
            MuninnError::Io(_) | MuninnError::Internal(_) => exit::INTERNAL,
        }
    }

    /// Shorthand for a configuration error, so call sites read as one line.
    pub fn config(message: impl Into<String>) -> Self {
        MuninnError::Config(message.into())
    }

    /// Shorthand for a runtime-requirement error.
    pub fn runtime(message: impl Into<String>) -> Self {
        MuninnError::Runtime(message.into())
    }

    /// Shorthand for an internal error.
    pub fn internal(message: impl Into<String>) -> Self {
        MuninnError::Internal(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_error_names_the_key() {
        let e = MuninnError::config("outputs.influxdb.url must not be empty");
        assert!(e.to_string().contains("outputs.influxdb.url"));
    }

    #[test]
    fn secret_error_carries_the_path() {
        let e = MuninnError::Secret {
            path: "/run/secrets/influxdb_token".into(),
            message: "file is empty".into(),
        };
        let rendered = e.to_string();
        assert!(rendered.contains("/run/secrets/influxdb_token"));
        assert!(rendered.contains("file is empty"));
    }

    /// Each error class maps to its documented exit code. The numbers are
    /// written out literally rather than referencing the constants a second
    /// time: this test exists to make a change to the mapping deliberate.
    #[test]
    fn errors_map_to_their_documented_exit_codes() {
        assert_eq!(MuninnError::config("x").exit_code(), 10);
        assert_eq!(
            MuninnError::Secret {
                path: "p".into(),
                message: "m".into()
            }
            .exit_code(),
            11
        );
        assert_eq!(MuninnError::runtime("x").exit_code(), 12);
        assert_eq!(MuninnError::TelegrafConfig("x".into()).exit_code(), 20);
        assert_eq!(MuninnError::TelegrafStart("x".into()).exit_code(), 21);
        assert_eq!(MuninnError::TelegrafExited("x".into()).exit_code(), 22);
        assert_eq!(MuninnError::internal("x").exit_code(), 30);
    }

    /// An unclassified I/O error is a gap in the code, not operator error, so it
    /// must not masquerade as a configuration problem.
    #[test]
    fn bare_io_error_is_internal_not_config() {
        let e = MuninnError::Io(std::io::Error::other("disk went away"));
        assert_eq!(e.exit_code(), exit::INTERNAL);
    }

    /// No error may exit 0 — a failure that reports success is worse than a
    /// failure.
    #[test]
    fn no_error_exits_successfully() {
        let all = [
            MuninnError::config("x"),
            MuninnError::Secret {
                path: "p".into(),
                message: "m".into(),
            },
            MuninnError::runtime("x"),
            MuninnError::TelegrafConfig("x".into()),
            MuninnError::TelegrafStart("x".into()),
            MuninnError::TelegrafExited("x".into()),
            MuninnError::Io(std::io::Error::other("x")),
            MuninnError::internal("x"),
        ];
        for e in all {
            assert_ne!(e.exit_code(), exit::OK, "{e} must not exit 0");
        }
    }
}
