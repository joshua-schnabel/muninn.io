//! The `[agent]` section.
//!
//! Every option is emitted explicitly, including the ones set to Telegraf's own
//! default. That is deliberate: the generated file is what an operator reads
//! when a metric is missing, and "not mentioned" versus "explicitly off" is a
//! distinction they should not have to look up. It also pins behaviour across
//! Telegraf releases — `skip_processors_after_aggregators` changes its default
//! in 1.40, and a config that states its choice does not change meaning when
//! that lands.

use muninn_telegraf::TelegrafConfig;

use crate::RenderContext;

/// Telegraf's own defaults for the two buffering options muninn does not expose.
///
/// Not configurable, because the values that matter for a host agent are the
/// collection and flush intervals, and every additional knob is one more thing
/// to get wrong. Stated here so the number has one home.
const METRIC_BATCH_SIZE: i64 = 1000;
const METRIC_BUFFER_LIMIT: i64 = 10_000;

pub fn render(ctx: &RenderContext<'_>) -> TelegrafConfig {
    let agent = &ctx.config.agent;
    let (debug, quiet) = ctx.config.logging.level.telegraf_flags();

    TelegrafConfig::new()
        .agent_option("interval", agent.interval.as_telegraf())
        // Align collection to interval boundaries, so metrics from a fleet land
        // on the same timestamps and are comparable.
        .agent_option("round_interval", true)
        .agent_option("metric_batch_size", METRIC_BATCH_SIZE)
        .agent_option("metric_buffer_limit", METRIC_BUFFER_LIMIT)
        // No jitter. It exists to spread load across many agents hitting one
        // endpoint; muninn monitors one host, and jitter would only make
        // timestamps harder to line up.
        .agent_option("collection_jitter", "0s")
        .agent_option("flush_interval", agent.flush_interval.as_telegraf())
        .agent_option("flush_jitter", "0s")
        // Empty means "use the output's own precision", which for InfluxDB v2 is
        // nanoseconds. Rounding here would discard resolution for no gain.
        .agent_option("precision", "")
        .agent_option("hostname", agent.hostname.clone())
        .agent_option("omit_hostname", agent.omit_hostname)
        .agent_option("debug", debug)
        .agent_option("quiet", quiet)
        // Empty means stdout. muninn captures Telegraf's output and re-emits it
        // through its own logger, so a log file inside the container would be a
        // second copy nobody reads and nobody rotates.
        .agent_option("logfile", "")
        // Stated explicitly because Telegraf changes this default in 1.40.
        // muninn generates no processors or aggregators, so the value is
        // immaterial today — but an unstated default that flips underneath us is
        // exactly the drift ADR-0011 pins the version against.
        .agent_option("skip_processors_after_aggregators", true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::config_with;
    use muninn_core::config::LogLevel;

    #[test]
    fn intervals_come_from_the_configuration() {
        let cfg = config_with(|c| {
            c.agent.interval = muninn_core::duration::ConfigDuration::from_secs(10);
            c.agent.flush_interval = muninn_core::duration::ConfigDuration::from_secs(60);
        });
        let rendered = render(&RenderContext::new(&cfg));
        let get = |key: &str| {
            rendered
                .agent
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.render())
        };
        assert_eq!(get("interval").as_deref(), Some("\"10s\""));
        assert_eq!(get("flush_interval").as_deref(), Some("\"60s\""));
    }

    /// Telegraf has two verbosity knobs, not five, and they must never both be
    /// set.
    #[test]
    fn the_log_level_maps_onto_debug_and_quiet() {
        for (level, want_debug, want_quiet) in [
            (LogLevel::Trace, true, false),
            (LogLevel::Debug, true, false),
            (LogLevel::Info, false, false),
            (LogLevel::Warn, false, true),
            (LogLevel::Error, false, true),
        ] {
            let cfg = config_with(|c| c.logging.level = level);
            let rendered = render(&RenderContext::new(&cfg));
            let get = |key: &str| {
                rendered
                    .agent
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.render())
                    .unwrap()
            };
            assert_eq!(get("debug"), want_debug.to_string(), "{level:?}");
            assert_eq!(get("quiet"), want_quiet.to_string(), "{level:?}");
        }
    }

    /// Order is part of the byte-for-byte guarantee, so it is pinned rather than
    /// left to whichever order the builder happens to use.
    #[test]
    fn the_agent_options_render_in_a_fixed_order() {
        let cfg = config_with(|_| {});
        let rendered = render(&RenderContext::new(&cfg));
        let keys: Vec<&str> = rendered.agent.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "interval",
                "round_interval",
                "metric_batch_size",
                "metric_buffer_limit",
                "collection_jitter",
                "flush_interval",
                "flush_jitter",
                "precision",
                "hostname",
                "omit_hostname",
                "debug",
                "quiet",
                "logfile",
                "skip_processors_after_aggregators",
            ]
        );
    }
}
