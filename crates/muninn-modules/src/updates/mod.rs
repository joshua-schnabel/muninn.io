//! The updates module: pending package updates on the host.
//!
//! It has a directory of its own because it is the only module that does work
//! rather than only describing it. Every other module renders a Telegraf plugin
//! and steps out of the way; this one renders a plugin that runs muninn again,
//! with [`debian::check`] behind it.
//!
//! # Why muninn runs itself
//!
//! Telegraf has no package input plugin — all 249 of version 1.39.2 were checked
//! during the WP1 spike — so the result has to arrive through `inputs.exec` as
//! influx line protocol. What produces that line was the one question the spike
//! left open: a shell helper, or muninn itself. It is muninn itself, and
//! [ADR-0009](../../../../docs/adr/0009-updates-module-approach.md) records why.
//!
//! # Why a failing check does not stop muninn
//!
//! The Docker module refuses the start when its endpoint does not answer,
//! because a Docker module reporting nothing is indistinguishable from a host
//! with no containers. This module is the opposite case: a failed check is
//! *visible*, as `check_success=0` with a reason. There is nothing to
//! misinterpret, so muninn degrades and keeps collecting everything else.

pub mod debian;

use muninn_core::Config;

use crate::inputs::RANK_UPDATES;
use crate::{MonitoringModule, PluginInstance, RenderContext, Requirements};

/// Where the image installs muninn.
///
/// The rendered configuration names this path rather than the running
/// executable, so `muninn render-config` produces the same bytes wherever it is
/// run — the generated configuration describes the artefact, not the machine
/// that generated it. The Dockerfile is what has to agree with it, and the
/// container tests are what hold the two together.
pub const MUNINN_BINARY: &str = "/usr/local/bin/muninn";

/// The subcommand that performs the check.
pub const CHECK_SUBCOMMAND: &str = "update-check";

/// Pending package updates on the host.
pub struct Updates;

impl MonitoringModule for Updates {
    fn id(&self) -> &'static str {
        "updates"
    }

    fn enabled(&self, c: &Config) -> bool {
        c.modules.updates.enabled
    }

    fn requirements(&self, _c: &Config) -> Requirements {
        Requirements {
            // /usr is needed because /etc/os-release is a symlink into it, and a
            // mount set without it reports "not a Debian host" for a machine
            // that plainly is. Found the hard way during the spike.
            host_paths: vec!["var", "etc", "usr"],
            debian_family_only: true,
            ..Default::default()
        }
    }

    fn render(&self, ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
        let m = &ctx.config.modules.updates;

        // `commands` takes an array *per command* — `[["/usr/local/bin/muninn",
        // "update-check"]]` — rather than one string to be split. Telegraf
        // deprecated the string form in 1.39.0 and removes it in 1.45.0, and a
        // deprecation warning in the log is noise an operator cannot act on:
        // muninn wrote the configuration, not them.
        //
        // The singular `command` key looks like the modern spelling and is not:
        // the plugin still binds it to a `string`, so an array there is rejected
        // at `config check` with "cannot unmarshal TOML array into string".
        let mut command = vec![MUNINN_BINARY.to_string(), CHECK_SUBCOMMAND.to_string()];
        if !m.security_only_metric {
            command.push("--no-security-metric".to_string());
        }

        let mut environment = vec![format!("HOSTFS={}", host_prefix(ctx.config))];
        // apt writes its cache directory even when it is only simulating. The
        // documented deployment has a read-only root filesystem with one
        // writable tmpfs — the runtime directory — so the check is pointed at
        // it rather than at /tmp, which in that deployment does not exist.
        if let Some(dir) = scratch_directory(ctx.config) {
            environment.push(format!("TMPDIR={}", dir.display()));
        }

        vec![
            PluginInstance::input("exec", RANK_UPDATES)
                .from_module("updates")
                .scalar("commands", vec![command])
                .scalar("environment", environment)
                // Its own schedule: package state changes on the scale of hours,
                // and a full apt resolution is expensive next to reading /proc.
                .scalar("interval", m.interval.as_telegraf())
                // Generous, because apt has to parse the host's whole package
                // index. Well under the interval either way.
                .scalar("timeout", "30s")
                .scalar("data_format", "influx")
                // The check reports its own failure as data (check_success=0)
                // and exits 0, so a non-zero exit means the helper itself is
                // broken. Telegraf should surface that rather than swallow it.
                .scalar("ignore_error", false),
        ]
    }
}

/// The host mount prefix as the helper should see it.
///
/// Empty means muninn is running on the host itself, where the host filesystem
/// is simply `/`.
pub fn host_prefix(config: &Config) -> String {
    config
        .runtime
        .host_mount_prefix
        .clone()
        .unwrap_or_else(|| "/".to_string())
}

/// Where apt's cache directory should be created: the runtime directory, which
/// is the one writable place the documented deployment has.
///
/// `None` only if the generated configuration has no parent directory, which
/// validation does not allow — the caller decides what to do with that rather
/// than this function inventing `/tmp`, which on a read-only root filesystem
/// does not exist.
pub fn scratch_directory(config: &Config) -> Option<std::path::PathBuf> {
    std::path::Path::new(&config.runtime.generated_config_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::config_with;

    fn rendered(config: &Config) -> PluginInstance {
        Updates.render(&RenderContext::new(config)).remove(0)
    }

    fn scalar(instance: &PluginInstance, key: &str) -> String {
        format!("{:?}", instance.scalars().find(|(k, _)| k == key))
    }

    #[test]
    fn the_command_is_the_binary_the_image_installs() {
        let cfg = config_with(|c| c.modules.updates.enabled = true);
        let out = scalar(&rendered(&cfg), "commands");
        assert!(
            out.contains("/usr/local/bin/muninn") && out.contains("update-check"),
            "the rendered command must match the Dockerfile's install path: {out}"
        );
    }

    /// Telegraf 1.39 deprecated giving `commands` one space-separated string and
    /// removes that form in 1.45, so each command renders as its own argv array.
    /// A deprecation warning in the log is noise an operator cannot act on,
    /// because muninn wrote the configuration rather than them.
    ///
    /// The singular `command` key is not the fix — the plugin binds it to a
    /// string, and an array there is rejected at `config check`.
    #[test]
    fn the_command_renders_as_argv_not_as_a_string() {
        let cfg = config_with(|c| c.modules.updates.enabled = true);
        let instance = rendered(&cfg);
        assert!(
            !instance.scalars().any(|(k, _)| k == "command"),
            "the singular key takes a string; an array there fails config check"
        );
        let out = scalar(&instance, "commands");
        assert!(
            !out.contains("muninn update-check"),
            "the binary and the subcommand must be separate elements: {out}"
        );
        assert!(
            matches!(
                instance.scalars().find(|(k, _)| k == "commands").map(|(_, v)| v),
                Some(muninn_telegraf::TomlValue::Array(items))
                    if matches!(items.first(), Some(muninn_telegraf::TomlValue::Array(_)))
            ),
            "commands must be an array of argv arrays: {out}"
        );
    }

    /// The rendered path must not depend on where the rendering process happens
    /// to live, or `render-config` on a developer's machine would produce a
    /// configuration that only works on that machine.
    #[test]
    fn the_command_does_not_depend_on_the_running_executable() {
        let cfg = config_with(|c| c.modules.updates.enabled = true);
        // The rule is about trusting current_exe for a security decision. This
        // asserts the opposite: that the rendered command does NOT contain it.
        // nosemgrep: rust.lang.security.current-exe.current-exe
        let here = std::env::current_exe().unwrap();
        let out = scalar(&rendered(&cfg), "commands");
        assert!(
            !out.contains(&here.to_string_lossy().to_string()),
            "rendered the test binary's own path: {out}"
        );
    }

    #[test]
    fn switching_off_the_security_metric_reaches_the_helper() {
        let cfg = config_with(|c| {
            c.modules.updates.enabled = true;
            c.modules.updates.security_only_metric = false;
        });
        assert!(scalar(&rendered(&cfg), "commands").contains("--no-security-metric"));

        let cfg = config_with(|c| c.modules.updates.enabled = true);
        assert!(!scalar(&rendered(&cfg), "commands").contains("--no-security-metric"));
    }

    /// The helper reads the host through the same prefix every other module
    /// does. If these two disagreed, the updates count would describe a
    /// different machine than the CPU graph next to it.
    #[test]
    fn the_helper_is_told_the_same_host_prefix_the_agent_uses() {
        let cfg = config_with(|c| {
            c.modules.updates.enabled = true;
            c.runtime.host_mount_prefix = Some("/hostfs".to_string());
        });
        assert!(scalar(&rendered(&cfg), "environment").contains("HOSTFS=/hostfs"));

        let cfg = config_with(|c| {
            c.modules.updates.enabled = true;
            c.runtime.host_mount_prefix = None;
        });
        assert!(scalar(&rendered(&cfg), "environment").contains("HOSTFS=/"));
    }

    /// apt writes its cache even when simulating. On a read-only root filesystem
    /// the only writable place is the runtime directory, so that is where the
    /// helper is sent.
    #[test]
    fn the_helper_is_pointed_at_the_writable_runtime_directory() {
        let cfg = config_with(|c| {
            c.modules.updates.enabled = true;
            c.runtime.generated_config_path = "/run/muninn/telegraf.conf".to_string();
        });
        assert!(scalar(&rendered(&cfg), "environment").contains("TMPDIR=/run/muninn"));
    }
}
