//! The image_updates module: whether a newer image is available, under the
//! tag it is running, for every running container.
//!
//! Like [`crate::updates`], this is a module that does work rather than only
//! describing a Telegraf plugin — Telegraf has no plugin that can answer this
//! question either, so the result arrives through `inputs.exec` running
//! muninn again, with [`check::check`] behind it.
//!
//! # Why this reaches the Docker API, and updates does not reach a registry
//!
//! `docker_api` calls the Docker Engine API on every check, not only once at
//! startup — a deliberate departure from the rest of the codebase, where
//! `muninn/src/probe.rs` sends exactly one `_ping` and Telegraf does the rest.
//! [`docker_api`] explains why the daemon rather than the registry directly.
//! See [ADR-0013](../../../../docs/adr/0013-image-updates-via-docker-api.md).
//!
//! # Why a failing check does not stop muninn
//!
//! The same reasoning as `updates`, applied per container rather than to one
//! host-wide count: an unreachable registry, a private image, or an image
//! built locally each produce `check_success=0` with a reason on that
//! container's own series, never a silent "up to date". A container whose
//! image cannot be judged does not stop the others' verdicts from being
//! reported, and does not stop muninn.

pub mod check;
pub mod docker_api;

use muninn_core::Config;

use crate::inputs::{RANK_IMAGE_UPDATES, parse_docker_endpoint};
use crate::updates::MUNINN_BINARY;
use crate::{MonitoringModule, PluginInstance, RenderContext, Requirements};

/// The subcommand that performs the check.
pub const CHECK_SUBCOMMAND: &str = "image-check";

/// Whether a newer image is available, under the same tag, per running
/// container.
pub struct ImageUpdates;

impl MonitoringModule for ImageUpdates {
    fn id(&self) -> &'static str {
        "image_updates"
    }

    fn enabled(&self, c: &Config) -> bool {
        c.modules.image_updates.enabled
    }

    fn requirements(&self, c: &Config) -> Requirements {
        // Exactly the docker module's reasoning: the requirement is derived
        // from the configured endpoint rather than fixed, because the
        // recommended deployment is a socket proxy reached over TCP, which has
        // no socket file at all. See ADR-0010.
        let mut req = Requirements::default();
        let m = &c.modules.image_updates;
        if let Some(endpoint) = parse_docker_endpoint(&m.endpoint, m.timeout.inner()) {
            if let crate::EndpointKind::UnixSocket(path) = &endpoint.kind {
                req.absolute_paths.push(path.clone());
            }
            req.endpoints.push(endpoint);
        }
        req
    }

    fn render(&self, ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
        let m = &ctx.config.modules.image_updates;

        let mut command = vec![
            MUNINN_BINARY.to_string(),
            CHECK_SUBCOMMAND.to_string(),
            "--endpoint".to_string(),
            m.endpoint.clone(),
            "--timeout-secs".to_string(),
            m.timeout.as_secs().to_string(),
        ];
        for pattern in &m.container_include {
            command.push("--include".to_string());
            command.push(pattern.clone());
        }
        for pattern in &m.container_exclude {
            command.push("--exclude".to_string());
            command.push(pattern.clone());
        }

        vec![
            PluginInstance::input("exec", RANK_IMAGE_UPDATES)
                .from_module("image_updates")
                .scalar("commands", vec![command])
                // Its own schedule: registry lookups are rate-limited and run
                // once per container, so this is deliberately not tied to
                // agent.interval. Same reasoning as `updates`.
                .scalar("interval", m.interval.as_telegraf())
                // Generous and fixed rather than derived from a container
                // count muninn cannot know at render time: worst case is one
                // `modules.image_updates.timeout` per Docker API call, two
                // calls per container, run sequentially. A host with more
                // containers than this comfortably covers should narrow
                // `container_include`/`container_exclude` rather than widen
                // this — see docs/modules.md#image_updates.
                .scalar("timeout", "120s")
                .scalar("data_format", "influx")
                // As with `updates`: the check reports its own failure as data
                // (check_success=0) and exits 0, so a non-zero exit means the
                // helper itself is broken and Telegraf should say so rather
                // than swallow it.
                .scalar("ignore_error", false),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::config_with;

    fn rendered(config: &Config) -> PluginInstance {
        ImageUpdates.render(&RenderContext::new(config)).remove(0)
    }

    fn scalar(instance: &PluginInstance, key: &str) -> String {
        format!("{:?}", instance.scalars().find(|(k, _)| k == key))
    }

    #[test]
    fn the_command_is_the_binary_the_image_installs() {
        let cfg = config_with(|c| c.modules.image_updates.enabled = true);
        let out = scalar(&rendered(&cfg), "commands");
        assert!(
            out.contains("/usr/local/bin/muninn") && out.contains("image-check"),
            "the rendered command must match the Dockerfile's install path: {out}"
        );
    }

    #[test]
    fn the_endpoint_and_timeout_reach_the_helper() {
        let cfg = config_with(|c| {
            c.modules.image_updates.enabled = true;
            c.modules.image_updates.endpoint = "tcp://docker-socket-proxy:2375".to_string();
            c.modules.image_updates.timeout = muninn_core::duration::ConfigDuration::from_secs(9);
        });
        let out = scalar(&rendered(&cfg), "commands");
        assert!(out.contains("--endpoint") && out.contains("tcp://docker-socket-proxy:2375"));
        assert!(out.contains("--timeout-secs") && out.contains("\"9\""));
    }

    #[test]
    fn include_and_exclude_patterns_each_render_as_their_own_flag() {
        let cfg = config_with(|c| {
            c.modules.image_updates.enabled = true;
            c.modules.image_updates.container_include = vec!["app-*".to_string()];
            c.modules.image_updates.container_exclude =
                vec!["build-*".to_string(), "ci-*".to_string()];
        });
        let out = scalar(&rendered(&cfg), "commands");
        assert!(out.contains("--include") && out.contains("app-*"));
        assert!(out.contains("--exclude") && out.contains("build-*") && out.contains("ci-*"));
    }

    /// A unix endpoint needs the socket file *and* a live daemon behind it; a
    /// TCP endpoint (the recommended proxy deployment) needs only the service.
    /// Exactly the docker module's rule — ADR-0010.
    #[test]
    fn requirements_follow_the_configured_endpoint_scheme() {
        let unix_cfg = config_with(|c| {
            c.modules.image_updates.enabled = true;
            c.modules.image_updates.endpoint = "unix:///var/run/docker.sock".to_string();
        });
        let req = ImageUpdates.requirements(&unix_cfg);
        assert_eq!(req.absolute_paths, vec!["/var/run/docker.sock".to_string()]);
        assert_eq!(req.endpoints.len(), 1);

        let tcp_cfg = config_with(|c| {
            c.modules.image_updates.enabled = true;
            c.modules.image_updates.endpoint = "tcp://docker-socket-proxy:2375".to_string();
        });
        let req = ImageUpdates.requirements(&tcp_cfg);
        assert!(req.absolute_paths.is_empty());
        assert_eq!(req.endpoints.len(), 1);
    }
}
