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

use std::time::Duration;

use muninn_core::Config;
use muninn_core::duration::ConfigDuration;

use crate::inputs::{RANK_IMAGE_UPDATES, parse_docker_endpoint};
use crate::updates::MUNINN_BINARY;
use crate::{MonitoringModule, PluginInstance, RenderContext, Requirements};

/// The subcommand that performs the check.
pub const CHECK_SUBCOMMAND: &str = "image-check";

/// The upper bound on how long one run may take, whatever the interval says.
///
/// Five minutes is far more than the check needs on any host it has been run
/// against, and still short enough that a stuck run does not overlap the next
/// one at the shortest interval validation allows.
const MAX_BUDGET: Duration = Duration::from_secs(300);

/// How much longer Telegraf waits than the check's own budget.
///
/// Only has to cover process start plus writing the report, because the check
/// stops working once its budget is spent — it does not need slack for the
/// work itself.
const EXEC_TIMEOUT_MARGIN: Duration = Duration::from_secs(15);

/// How long one run may take, derived rather than configured.
///
/// Half the interval, capped at [`MAX_BUDGET`]: a check that cannot finish
/// within half its own schedule is going to overlap itself, and the operator
/// already stated the schedule. Adding a fourth duration key for this would be
/// asking them to keep two numbers consistent that only ever have one right
/// relationship.
pub fn budget(interval: Duration) -> Duration {
    (interval / 2).min(MAX_BUDGET)
}

/// The `inputs.exec` timeout that goes with [`budget`].
///
/// Derived from it rather than fixed, because the two failure modes are not
/// symmetric: a helper that overruns Telegraf's timeout is killed and reports
/// *nothing*, losing even the verdicts it had already established, while one
/// that runs out of its own budget still emits every result it has and marks
/// the rest `budget_exceeded`. So Telegraf's number is always the larger, by
/// construction rather than by a comment asking someone to keep it that way.
pub fn exec_timeout(interval: Duration) -> Duration {
    budget(interval) + EXEC_TIMEOUT_MARGIN
}

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

        let budget = budget(m.interval.inner());

        let mut command = vec![
            MUNINN_BINARY.to_string(),
            CHECK_SUBCOMMAND.to_string(),
            "--endpoint".to_string(),
            m.endpoint.clone(),
            "--timeout-secs".to_string(),
            m.timeout.as_secs().to_string(),
            "--registry-timeout-secs".to_string(),
            m.registry_timeout.as_secs().to_string(),
            "--budget-secs".to_string(),
            budget.as_secs().to_string(),
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
                // Always longer than the budget the check was just handed, so
                // the check runs out of its own time before Telegraf runs out
                // of patience. That ordering is what turns "too many
                // containers" from a killed process reporting nothing into a
                // report where the ones not reached say `budget_exceeded`.
                .scalar(
                    "timeout",
                    ConfigDuration::new(exec_timeout(m.interval.inner())).as_telegraf(),
                )
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
    fn the_endpoint_and_both_timeouts_reach_the_helper() {
        let cfg = config_with(|c| {
            c.modules.image_updates.enabled = true;
            c.modules.image_updates.endpoint = "tcp://docker-socket-proxy:2375".to_string();
            c.modules.image_updates.timeout = ConfigDuration::from_secs(9);
            c.modules.image_updates.registry_timeout = ConfigDuration::from_secs(45);
        });
        let out = scalar(&rendered(&cfg), "commands");
        assert!(out.contains("--endpoint") && out.contains("tcp://docker-socket-proxy:2375"));
        assert!(out.contains("--timeout-secs") && out.contains("\"9\""));
        assert!(out.contains("--registry-timeout-secs") && out.contains("\"45\""));
    }

    /// The property the whole budget mechanism rests on: Telegraf's patience
    /// always outlasts the check's own, so the check reports what it found
    /// rather than being killed holding it.
    #[test]
    fn telegraf_always_waits_longer_than_the_check_budgets_for_itself() {
        for interval_secs in [60, 300, 900, 3600, 86_400] {
            let interval = Duration::from_secs(interval_secs);
            assert!(
                exec_timeout(interval) > budget(interval),
                "interval {interval_secs}s: exec timeout {:?} must exceed budget {:?}",
                exec_timeout(interval),
                budget(interval)
            );
        }
    }

    /// Half the interval, so a run cannot overlap the next one, and never more
    /// than the cap however long the interval is.
    #[test]
    fn the_budget_is_half_the_interval_up_to_the_cap() {
        assert_eq!(budget(Duration::from_secs(60)), Duration::from_secs(30));
        assert_eq!(budget(Duration::from_secs(600)), Duration::from_secs(300));
        assert_eq!(budget(Duration::from_secs(3600)), MAX_BUDGET);
        assert_eq!(budget(Duration::from_secs(86_400)), MAX_BUDGET);
    }

    #[test]
    fn the_rendered_budget_and_exec_timeout_agree_with_the_helpers() {
        let cfg = config_with(|c| {
            c.modules.image_updates.enabled = true;
            c.modules.image_updates.interval = ConfigDuration::from_secs(600);
        });
        let instance = rendered(&cfg);
        assert!(
            scalar(&instance, "commands").contains("\"300\""),
            "--budget-secs should be half of a 600s interval: {}",
            scalar(&instance, "commands")
        );
        assert!(
            scalar(&instance, "timeout").contains("315s"),
            "{}",
            scalar(&instance, "timeout")
        );
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
