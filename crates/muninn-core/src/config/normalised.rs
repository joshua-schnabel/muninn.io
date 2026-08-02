//! The normalised configuration — what the rest of muninn actually uses.
//!
//! [`ConfigV1`] mirrors the YAML: paths are strings, secrets are paths,
//! addresses are unparsed, and a disabled output is still present with empty
//! fields. That shape is right for deserialising and wrong for everything
//! afterwards.
//!
//! This module resolves it once, so that downstream code cannot get it wrong:
//!
//! - addresses are [`SocketAddr`], already parsed;
//! - secrets are [`Secret`], already read from disk;
//! - a **disabled output is `None`**, so it is not possible to render one by
//!   forgetting to check a boolean.
//!
//! It is also the seam a future `ConfigV2` converts into. When the schema
//! changes, `from_v2` joins `from_v1` here and nothing downstream moves — which
//! is the whole reason for having two layers rather than passing `ConfigV1`
//! around.

use std::net::SocketAddr;

use crate::config::model::{
    self, AgentConfig, ConfigV1, LoggingConfig, ModulesConfig, RuntimeConfig,
};
use crate::duration::ConfigDuration;
use crate::error::Result;
use crate::secret::Secret;

/// A validated, resolved configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub agent: AgentConfig,
    pub runtime: Runtime,
    pub logging: LoggingConfig,
    pub health: Health,
    pub modules: ModulesConfig,
    pub outputs: Outputs,
}

#[derive(Debug, Clone)]
pub struct Runtime {
    pub shutdown_grace_period: ConfigDuration,
    pub telegraf_start_timeout: ConfigDuration,
    pub generated_config_path: String,
    /// `None` means "running directly on the host, no prefix applies" — the
    /// empty string from the YAML, turned into something the type system can
    /// distinguish from a path.
    pub host_mount_prefix: Option<String>,
}

impl Runtime {
    /// The `HOST_*` environment variables Telegraf needs, derived from the one
    /// prefix the operator configures.
    ///
    /// Empty when there is no prefix: on a real host, gopsutil's defaults are
    /// already correct and setting these would be wrong.
    pub fn host_env(&self) -> Vec<(String, String)> {
        let Some(prefix) = &self.host_mount_prefix else {
            return Vec::new();
        };
        let p = prefix.trim_end_matches('/');
        vec![
            // HOST_MOUNT_PREFIX is not optional decoration: it strips the prefix
            // from reported paths, so a filesystem at /var is tagged `/var` and
            // not `/hostfs/var`. Without it every disk metric carries a path
            // that matches nothing an operator recognises.
            ("HOST_MOUNT_PREFIX".to_string(), p.to_string()),
            ("HOST_PROC".to_string(), format!("{p}/proc")),
            ("HOST_SYS".to_string(), format!("{p}/sys")),
            ("HOST_ETC".to_string(), format!("{p}/etc")),
            ("HOST_VAR".to_string(), format!("{p}/var")),
            ("HOST_RUN".to_string(), format!("{p}/run")),
        ]
    }
}

#[derive(Debug, Clone)]
pub struct Health {
    pub listen: SocketAddr,
}

#[derive(Debug, Clone)]
pub struct Outputs {
    pub influxdb: Option<Influxdb>,
    pub prometheus: Option<Prometheus>,
}

#[derive(Debug, Clone)]
pub struct Influxdb {
    pub url: String,
    pub organization: String,
    pub bucket: String,
    pub token: Secret,
    pub timeout: ConfigDuration,
    pub tls: model::TlsConfig,
}

#[derive(Debug, Clone)]
pub struct Prometheus {
    pub listen: SocketAddr,
    pub path: String,
    pub expiration_interval: ConfigDuration,
    pub basic_auth: Option<BasicAuth>,
}

#[derive(Debug, Clone)]
pub struct BasicAuth {
    pub username: String,
    pub password: Secret,
}

impl Config {
    /// Resolve a validated [`ConfigV1`].
    ///
    /// # Panics
    ///
    /// Never — but it does assume validation has already run. Addresses are
    /// re-parsed here and the failure is propagated rather than unwrapped, so a
    /// caller that skips validation gets an error instead of a panic.
    pub fn from_v1(cfg: ConfigV1) -> Result<Self> {
        let health = Health {
            listen: parse_addr(&cfg.health.listen, "health.listen")?,
        };

        let influxdb = if cfg.outputs.influxdb.enabled {
            let o = &cfg.outputs.influxdb;
            Some(Influxdb {
                url: o.url.clone(),
                organization: o.organization.clone(),
                bucket: o.bucket.clone(),
                token: Secret::from_file(&o.token_file)?,
                timeout: o.timeout,
                tls: o.tls.clone(),
            })
        } else {
            None
        };

        let prometheus = if cfg.outputs.prometheus.enabled {
            let o = &cfg.outputs.prometheus;
            let basic_auth = match (&o.basic_auth.username, &o.basic_auth.password_file) {
                (Some(user), Some(file)) => Some(BasicAuth {
                    username: user.clone(),
                    password: Secret::from_file(file)?,
                }),
                // Validation has already rejected the half-configured cases, so
                // anything else here means no authentication.
                _ => None,
            };
            Some(Prometheus {
                listen: parse_addr(&o.listen, "outputs.prometheus.listen")?,
                path: o.path.clone(),
                expiration_interval: o.expiration_interval,
                basic_auth,
            })
        } else {
            None
        };

        Ok(Config {
            agent: cfg.agent,
            runtime: Runtime {
                shutdown_grace_period: cfg.runtime.shutdown_grace_period,
                telegraf_start_timeout: cfg.runtime.telegraf_start_timeout,
                generated_config_path: cfg.runtime.generated_config_path,
                host_mount_prefix: normalise_prefix(cfg.runtime.host_mount_prefix),
            },
            logging: cfg.logging,
            health,
            modules: cfg.modules,
            outputs: Outputs {
                influxdb,
                prometheus,
            },
        })
    }
}

fn normalise_prefix(raw: String) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "/" {
        // "/" and "" mean the same thing — the host filesystem is where it
        // normally is — and collapsing them here means downstream code has one
        // case to handle rather than three.
        None
    } else {
        Some(trimmed.trim_end_matches('/').to_string())
    }
}

fn parse_addr(value: &str, key: &str) -> Result<SocketAddr> {
    value.parse::<SocketAddr>().map_err(|_| {
        crate::error::MuninnError::internal(format!(
            "{key} '{value}' reached normalisation unparsed — validation should have rejected it"
        ))
    })
}

/// Kept out of the normalised model on purpose.
///
/// `RuntimeConfig` is the raw shape; [`Runtime`] is the resolved one. This impl
/// exists so tests can build a raw runtime block without repeating the defaults.
impl From<RuntimeConfig> for Runtime {
    fn from(raw: RuntimeConfig) -> Self {
        Runtime {
            shutdown_grace_period: raw.shutdown_grace_period,
            telegraf_start_timeout: raw.telegraf_start_timeout,
            generated_config_path: raw.generated_config_path,
            host_mount_prefix: normalise_prefix(raw.host_mount_prefix),
        }
    }
}
