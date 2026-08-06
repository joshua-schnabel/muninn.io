//! The outputs: InfluxDB v2 and Prometheus.
//!
//! A disabled output is `None` in the normalised configuration, so there is no
//! boolean to forget here — an output that is off simply is not present.

use muninn_telegraf::PluginInstance;

use crate::RenderContext;

const RANK_INFLUXDB: u16 = 10;
const RANK_PROMETHEUS: u16 = 20;

pub fn render(ctx: &RenderContext<'_>) -> Vec<PluginInstance> {
    let mut out = Vec::new();
    if let Some(influx) = &ctx.config.outputs.influxdb {
        out.push(render_influxdb(ctx, influx));
    }
    if let Some(prom) = &ctx.config.outputs.prometheus {
        out.push(render_prometheus(ctx, prom));
    }
    out
}

fn render_influxdb(
    ctx: &RenderContext<'_>,
    o: &muninn_core::config::normalised::Influxdb,
) -> PluginInstance {
    let tls = &o.tls;

    PluginInstance::output("influxdb_v2", RANK_INFLUXDB)
        .from_output("influxdb")
        // An array even for one URL: the plugin takes a list, and writing a bare
        // string here is rejected.
        .scalar("urls", vec![o.url.clone()])
        // The only place a real credential reaches the output, and only when
        // redaction is off.
        .scalar("token", ctx.secret(&o.token))
        .scalar("organization", o.organization.clone())
        .scalar("bucket", o.bucket.clone())
        .scalar("timeout", o.timeout.as_telegraf())
        .scalar_opt("tls_ca", tls.ca_file.clone())
        .scalar_opt("tls_cert", tls.cert_file.clone())
        .scalar_opt("tls_key", tls.key_file.clone())
        // Stated explicitly even when false. This is the one option in the file
        // whose value an auditor will want to confirm at a glance, and an
        // omitted key would make them go and look up the default.
        .scalar("insecure_skip_verify", tls.insecure_skip_verify)
}

fn render_prometheus(
    ctx: &RenderContext<'_>,
    o: &muninn_core::config::normalised::Prometheus,
) -> PluginInstance {
    let mut instance = PluginInstance::output("prometheus_client", RANK_PROMETHEUS)
        .from_output("prometheus")
        .scalar("listen", o.listen.to_string())
        .scalar("path", o.path.clone())
        .scalar("expiration_interval", o.expiration_interval.as_telegraf())
        // gocollector and process describe the Telegraf process itself. They are
        // excluded so this endpoint carries host metrics only — Telegraf's own
        // health is muninn's business to report, on the health port, where it
        // survives Telegraf not running. See ADR-0012.
        .scalar("collectors_exclude", vec!["gocollector", "process"]);

    if let Some(auth) = &o.basic_auth {
        // Through the same redaction path as the InfluxDB token. There is
        // exactly one way to emit a secret, and it goes through RenderContext.
        instance = instance
            .scalar("basic_username", auth.username.clone())
            .scalar("basic_password", ctx.secret(&auth.password));
    }

    instance
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{config_with, token_file};

    fn find(instance: &PluginInstance, key: &str) -> Option<String> {
        instance
            .scalars()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.render())
    }

    #[test]
    fn influxdb_url_renders_as_an_array() {
        let t = token_file("tok");
        let cfg = config_with(|c| crate::tests::enable_influx(c, &t));
        let out = render(&RenderContext::new(&cfg));
        let influx = out.iter().find(|i| i.plugin == "influxdb_v2").unwrap();
        assert_eq!(
            find(influx, "urls").as_deref(),
            Some("[\"https://influx.example:8086\"]"),
            "the plugin takes a list and rejects a bare string"
        );
    }

    /// The whole point of `render-config`: its output must be safe to paste into
    /// an issue.
    #[test]
    fn redaction_replaces_the_token() {
        let t = token_file("super-secret-token");
        let cfg = config_with(|c| crate::tests::enable_influx(c, &t));

        let plain = render(&RenderContext::new(&cfg));
        let influx = plain.iter().find(|i| i.plugin == "influxdb_v2").unwrap();
        assert_eq!(
            find(influx, "token").as_deref(),
            Some("\"super-secret-token\"")
        );

        let redacted = render(&RenderContext::redacted(&cfg));
        let influx = redacted.iter().find(|i| i.plugin == "influxdb_v2").unwrap();
        assert_eq!(find(influx, "token").as_deref(), Some("\"***\""));
    }

    #[test]
    fn unset_tls_options_are_omitted_rather_than_emitted_empty() {
        let t = token_file("tok");
        let cfg = config_with(|c| crate::tests::enable_influx(c, &t));
        let out = render(&RenderContext::new(&cfg));
        let influx = out.iter().find(|i| i.plugin == "influxdb_v2").unwrap();
        assert!(find(influx, "tls_ca").is_none());
        assert!(find(influx, "tls_cert").is_none());
        assert!(find(influx, "tls_key").is_none());
    }

    /// Always stated, even when false: an auditor should be able to confirm it
    /// at a glance rather than look up a default.
    #[test]
    fn insecure_skip_verify_is_always_stated() {
        let t = token_file("tok");
        let cfg = config_with(|c| crate::tests::enable_influx(c, &t));
        let out = render(&RenderContext::new(&cfg));
        let influx = out.iter().find(|i| i.plugin == "influxdb_v2").unwrap();
        assert_eq!(
            find(influx, "insecure_skip_verify").as_deref(),
            Some("false")
        );
    }

    #[test]
    fn prometheus_renders_its_listener_and_excludes_agent_collectors() {
        let cfg = config_with(|c| c.modules.cpu.enabled = true);
        let out = render(&RenderContext::new(&cfg));
        let prom = out
            .iter()
            .find(|i| i.plugin == "prometheus_client")
            .unwrap();
        assert_eq!(find(prom, "listen").as_deref(), Some("\"0.0.0.0:9273\""));
        assert_eq!(find(prom, "path").as_deref(), Some("\"/metrics\""));
        assert_eq!(
            find(prom, "collectors_exclude").as_deref(),
            Some("[\"gocollector\", \"process\"]"),
            "host metrics only — Telegraf's own health belongs on the health port"
        );
    }

    #[test]
    fn a_disabled_output_produces_no_instance() {
        let cfg = config_with(|c| {
            c.modules.cpu.enabled = true;
            c.outputs.prometheus = None;
        });
        assert!(render(&RenderContext::new(&cfg)).is_empty());
    }

    #[test]
    fn both_outputs_render_together_in_a_fixed_order() {
        let t = token_file("tok");
        let cfg = config_with(|c| crate::tests::enable_influx(c, &t));
        let rendered = render(&RenderContext::new(&cfg));
        let names: Vec<&str> = rendered.iter().map(|i| i.plugin.as_str()).collect();
        assert_eq!(names, vec!["influxdb_v2", "prometheus_client"]);
    }
}
