//! Monitoring modules and outputs. Each turns a slice of validated
//! configuration into Telegraf plugin instances.
//!
//! ```text
//!   Config ──► build() ──► TelegrafConfig ──► muninn_telegraf::render
//!              │
//!              ├─ agent section
//!              ├─ every enabled input module
//!              └─ every enabled output
//! ```
//!
//! # Two mappings that are not one-to-one
//!
//! **`load` and `system` render into a single plugin.** Telegraf has no
//! `inputs.load`; load averages, uptime and logged-in users are all groups of
//! `inputs.system`, selected with `include`. Two instances would collect every
//! metric twice with identical tags and nothing would complain, so both modules
//! declare the same merge key and the model unions them. See
//! `docs/adr/0008-system-and-load-merge.md`.
//!
//! **Exclusions are not plugin options.** `inputs.disk` has no mount-point
//! exclusion; `inputs.diskio` and `inputs.net` have no exclusion at all. Every
//! `exclude_*` key therefore renders into a `tagdrop` sub-table, which the model
//! is careful to emit after every scalar. See
//! `docs/adr/0007-tagdrop-and-render-order.md`.

use muninn_core::Config;
use muninn_telegraf::{PluginInstance, TelegrafConfig};

pub mod agent;
pub mod inputs;
pub mod outputs;

/// What a module needs from the host in order to report the truth.
///
/// Declared per module so `muninn check-runtime` verifies only what is actually
/// enabled, rather than demanding every mount from everyone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Requirements {
    /// Paths below the host mount prefix, e.g. `proc`, `sys`.
    pub host_paths: Vec<&'static str>,
    /// Absolute paths that are not under the prefix — the Docker socket.
    pub absolute_paths: Vec<String>,
    /// Linux capabilities. Empty for every MVP module: nothing muninn does needs
    /// one, and the hardening baseline drops them all.
    pub capabilities: Vec<&'static str>,
    /// True when the module only works on a Debian-family host.
    pub debian_family_only: bool,
}

impl Requirements {
    pub fn host(paths: &[&'static str]) -> Self {
        Requirements {
            host_paths: paths.to_vec(),
            ..Default::default()
        }
    }
}

/// Everything a module needs in order to render.
pub struct RenderContext<'a> {
    pub config: &'a Config,
    /// When true, secret values render as `***`.
    ///
    /// This is what makes `muninn render-config` safe to paste into an issue.
    /// The runtime path sets it to false, and that is the only place a real
    /// credential reaches the output.
    pub redact_secrets: bool,
}

impl<'a> RenderContext<'a> {
    pub fn new(config: &'a Config) -> Self {
        RenderContext {
            config,
            redact_secrets: false,
        }
    }

    pub fn redacted(config: &'a Config) -> Self {
        RenderContext {
            config,
            redact_secrets: true,
        }
    }

    /// A secret as it should appear in the output.
    pub fn secret(&self, value: &muninn_core::secret::Secret) -> String {
        if self.redact_secrets {
            "***".to_string()
        } else {
            value.expose().to_string()
        }
    }
}

/// A monitoring module.
///
/// Rendering and requirements are separate on purpose: `check-runtime` has to be
/// able to ask what a module needs without generating anything, and startup has
/// to be able to verify preconditions before it writes a file.
pub trait MonitoringModule {
    /// The name used in the YAML and in log messages.
    fn id(&self) -> &'static str;

    /// Whether this module is switched on.
    fn enabled(&self, config: &Config) -> bool;

    /// What the module needs from the host.
    fn requirements(&self) -> Requirements;

    /// The plugin instances this module contributes. Called only when enabled.
    fn render(&self, ctx: &RenderContext<'_>) -> Vec<PluginInstance>;
}

/// Every input module, in a fixed order.
///
/// The order here does not decide the output — instances carry an explicit rank
/// for that — but it does decide the order of `/status` listings and of any
/// diagnostics that walk the list.
pub fn all_modules() -> Vec<Box<dyn MonitoringModule>> {
    vec![
        Box::new(inputs::Cpu),
        Box::new(inputs::Memory),
        Box::new(inputs::Load),
        Box::new(inputs::System),
        Box::new(inputs::Swap),
        Box::new(inputs::Processes),
        Box::new(inputs::Disks),
        Box::new(inputs::DiskIo),
        Box::new(inputs::Network),
        Box::new(inputs::Docker),
        Box::new(inputs::Updates),
    ]
}

/// Build a complete Telegraf configuration from a validated muninn
/// configuration.
///
/// Infallible by construction: everything that could fail — unreadable secrets,
/// unparseable addresses, contradictory options — was resolved or rejected
/// before [`Config`] existed. A module that could fail here would mean the
/// validation layer had a hole.
pub fn build(ctx: &RenderContext<'_>) -> TelegrafConfig {
    let mut config = agent::render(ctx);

    for module in all_modules() {
        if module.enabled(ctx.config) {
            for instance in module.render(ctx) {
                config.add(instance);
            }
        }
    }

    for instance in outputs::render(ctx) {
        config.add(instance);
    }

    config
}

/// The requirements of every enabled module, collected.
///
/// What `muninn check-runtime` walks.
pub fn requirements_of_enabled(config: &Config) -> Vec<(&'static str, Requirements)> {
    all_modules()
        .into_iter()
        .filter(|m| m.enabled(config))
        .map(|m| (m.id(), m.requirements()))
        .collect()
}

#[cfg(test)]
mod tests;
