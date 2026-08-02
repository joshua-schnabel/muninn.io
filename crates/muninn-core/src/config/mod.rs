//! Configuration: the YAML shape, how it is loaded, what makes it valid, and
//! what the rest of muninn sees.
//!
//! ```text
//!   muninn.yaml
//!       │  loader::load        read, check version, parse, apply overrides
//!       ▼
//!   ConfigV1                   model::  — mirrors the file, nothing resolved
//!       │  validation::validate    semantic rules; returns warnings
//!       ▼
//!   Config                     normalised:: — addresses parsed, secrets read,
//!                                             disabled outputs are None
//! ```
//!
//! Two layers rather than one because the shape that is right for
//! deserialising — everything a string, everything optional, disabled things
//! still present — is wrong for everything afterwards.

pub mod loader;
pub mod model;
pub mod normalised;
pub mod validation;

pub use loader::{Overrides, load};
pub use model::{ConfigV1, LogFormat, LogLevel, SCHEMA_VERSION};
pub use normalised::Config;

use crate::error::Result;

/// The whole pipeline: read a file, validate it, resolve it.
///
/// Returns the resolved configuration and any warnings, which the caller emits
/// once logging is up.
pub fn load_and_resolve(
    path: impl AsRef<std::path::Path>,
    overrides: &Overrides,
) -> Result<(Config, Vec<String>)> {
    let (v1, warnings) = loader::load(path, overrides)?;
    let config = Config::from_v1(v1)?;
    Ok((config, warnings))
}

#[cfg(test)]
mod tests;
