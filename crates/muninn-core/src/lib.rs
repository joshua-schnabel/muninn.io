//! Configuration model, secret handling and error types for muninn.io.
//!
//! This crate owns everything between "an operator wrote a YAML file" and "a
//! validated, resolved model the rest of the program can render from". It knows
//! nothing about Telegraf.
//!
//! ```text
//!   muninn.yaml ──► ConfigV1 ──► validate ──► Config
//!                   (the file)   (the rules)  (resolved)
//! ```
//!
//! # Two invariants this crate exists to hold
//!
//! **Unknown keys are errors.** Every config struct carries
//! `#[serde(deny_unknown_fields)]`. A typo'd key must fail the load rather than
//! be silently ignored — a monitoring agent that quietly drops half its
//! configuration is worse than one that refuses to start.
//!
//! **A secret value never leaves this crate as a printable string.** Secrets are
//! wrapped in [`secret::Secret`], whose `Debug` and `Display` render `***`; the
//! real value is reachable only through an explicit accessor, called in exactly
//! one place downstream. Errors name the *path* of a secret file, never its
//! contents — which is why [`error::MuninnError::Secret`] has no field that
//! could hold a value.
//!
//! ```
//! use muninn_core::config::{self, Overrides};
//!
//! # fn example() -> muninn_core::error::Result<()> {
//! let (config, warnings) = config::load_and_resolve("/etc/muninn/muninn.yaml", &Overrides::from_env())?;
//! // Warnings are returned rather than logged: this runs before the tracing
//! // subscriber exists, because the level to initialise it with comes from
//! // this very file.
//! for w in &warnings {
//!     eprintln!("warning: {w}");
//! }
//! # let _ = config;
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod duration;
pub mod error;
pub mod exit;
pub mod secret;

pub use config::Config;
pub use error::{MuninnError, Result};
