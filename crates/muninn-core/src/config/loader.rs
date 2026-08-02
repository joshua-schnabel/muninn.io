//! Reading `muninn.yaml`, applying overrides, and validating.
//!
//! # Order matters
//!
//! The schema version is checked **before** the full parse. A version 2 file fed
//! to a version 1 build has keys that moved, keys that were removed and keys
//! that changed meaning; parsing it first produces a pile of "unknown field"
//! errors that bury the one fact worth reporting. So the loader probes for the
//! version, refuses an unknown one outright, and only then parses.
//!
//! # Precedence
//!
//! CLI argument → environment variable → YAML → default. The loader owns the
//! environment layer; the binary applies its CLI layer through [`Overrides`].

use std::path::Path;

use serde::Deserialize;

use crate::config::model::{ConfigV1, LogFormat, LogLevel, SCHEMA_VERSION};
use crate::config::validation;
use crate::error::{MuninnError, Result};

/// Settings that may come from the CLI or the environment, overriding the file.
///
/// Deliberately a small, closed set. Module and output settings are *not*
/// environment-overridable: the YAML is meant to be the single readable
/// description of what this agent does, and a deployment that half-configures
/// itself through environment variables defeats that.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub log_level: Option<String>,
    pub log_format: Option<String>,
}

impl Overrides {
    /// Read `MUNINN_LOG_LEVEL` and `MUNINN_LOG_FORMAT`.
    pub fn from_env() -> Self {
        Self {
            log_level: std::env::var("MUNINN_LOG_LEVEL").ok(),
            log_format: std::env::var("MUNINN_LOG_FORMAT").ok(),
        }
    }

    /// CLI values win over environment ones. `None` means "not given", which is
    /// why these are `Option` and not defaulted strings: with a default, "not
    /// given" and "explicitly the default value" become indistinguishable and
    /// the layer below can never be overridden back.
    pub fn merge_cli(mut self, log_level: Option<String>, log_format: Option<String>) -> Self {
        if log_level.is_some() {
            self.log_level = log_level;
        }
        if log_format.is_some() {
            self.log_format = log_format;
        }
        self
    }
}

/// Load, override and validate.
///
/// Returns the configuration and any warnings. Warnings are returned rather than
/// logged because this runs before the tracing subscriber exists — the log level
/// to initialise it with comes from this very file.
pub fn load(path: impl AsRef<Path>, overrides: &Overrides) -> Result<(ConfigV1, Vec<String>)> {
    let path = path.as_ref();

    let text = std::fs::read_to_string(path).map_err(|e| {
        MuninnError::config(format!(
            "cannot read '{}': {}",
            path.display(),
            match e.kind() {
                std::io::ErrorKind::NotFound => "file does not exist".to_string(),
                std::io::ErrorKind::PermissionDenied => "file is not readable".to_string(),
                _ => e.to_string(),
            }
        ))
    })?;

    from_str(&text, overrides)
}

/// The same pipeline, from a string. The tests use this; `load` is the thin file
/// wrapper around it.
pub fn from_str(text: &str, overrides: &Overrides) -> Result<(ConfigV1, Vec<String>)> {
    check_version(text)?;

    let mut cfg: ConfigV1 = serde_yaml_ng::from_str(text)
        .map_err(|e| MuninnError::config(format!("invalid configuration: {e}")))?;

    let mut warnings = apply_overrides(&mut cfg, overrides);
    warnings.extend(validation::validate(&cfg)?);

    Ok((cfg, warnings))
}

/// Probe for `version` without parsing the rest.
///
/// This struct deliberately does *not* deny unknown fields — it has to be able
/// to read the version out of a file whose other keys it will not recognise,
/// which is exactly the situation it exists for.
#[derive(Deserialize)]
struct VersionProbe {
    version: Option<u32>,
}

fn check_version(text: &str) -> Result<()> {
    let probe: VersionProbe = serde_yaml_ng::from_str(text).map_err(|e| {
        // A file too malformed to read a single scalar out of is a YAML problem,
        // not a version problem, and saying so is more useful than "missing
        // version" for a file with a tab in it.
        MuninnError::config(format!("invalid YAML: {e}"))
    })?;

    match probe.version {
        None => Err(MuninnError::config(format!(
            "missing required key `version`. Add `version: {SCHEMA_VERSION}` at the top of the file"
        ))),
        Some(v) if v == SCHEMA_VERSION => Ok(()),
        Some(v) => Err(MuninnError::config(format!(
            "unsupported schema version {v}; this build of muninn understands version \
             {SCHEMA_VERSION}. Either use a muninn that supports version {v}, or migrate the \
             file — see docs/configuration.md"
        ))),
    }
}

/// Apply the environment and CLI layers, returning a warning for every value
/// that was set but unusable.
///
/// A bad value **keeps the previous setting** rather than falling back to a
/// default. `MUNINN_LOG_FORMAT=jsn` quietly becoming `human` makes a typo in a
/// deployment look exactly like a deliberate choice, and the operator then
/// spends an afternoon wondering why their log pipeline sees no JSON.
fn apply_overrides(cfg: &mut ConfigV1, overrides: &Overrides) -> Vec<String> {
    let mut warnings = Vec::new();

    if let Some(raw) = &overrides.log_level {
        match raw.to_ascii_lowercase().as_str() {
            "trace" => cfg.logging.level = LogLevel::Trace,
            "debug" => cfg.logging.level = LogLevel::Debug,
            "info" => cfg.logging.level = LogLevel::Info,
            "warn" => cfg.logging.level = LogLevel::Warn,
            "error" => cfg.logging.level = LogLevel::Error,
            _ => warnings.push(format!(
                "log level '{raw}' is not one of trace/debug/info/warn/error — \
                 keeping logging.level={}",
                cfg.logging.level.as_str()
            )),
        }
    }

    if let Some(raw) = &overrides.log_format {
        match raw.to_ascii_lowercase().as_str() {
            "human" => cfg.logging.format = LogFormat::Human,
            "json" => cfg.logging.format = LogFormat::Json,
            _ => warnings.push(format!(
                "log format '{raw}' is not one of human/json — keeping logging.format={:?}",
                cfg.logging.format
            )),
        }
    }

    warnings
}
