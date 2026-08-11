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

    // Server-side TLS. The option names are Telegraf's own and were checked
    // against the pinned release's `sample.conf` rather than assumed from the
    // InfluxDB output's client-side ones: the CA key here is
    // `tls_allowed_cacerts`, it is an array, and it means "client certificates
    // I will accept" rather than "who I trust".
    if let (Some(cert), Some(key)) = (&o.tls.cert_file, &o.tls.key_file) {
        instance = instance
            .scalar("tls_cert", cert.clone())
            .scalar("tls_key", key.clone());

        if let Some(ca) = &o.tls.client_ca_file {
            instance = instance.scalar("tls_allowed_cacerts", vec![ca.clone()]);
        }
    }

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
    use muninn_core::Config;
    use muninn_core::config::normalised;

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

    // ── The Prometheus credential and its TLS ───────────────────────────────
    //
    // The `basic_auth` branch shipped with **no rendering test at all**: the
    // tests above cover urls, redaction, TLS omission, the listener and
    // ordering, and none of them ever set it, while the shipped example leaves
    // both keys null so it is absent from the reference config too. The first
    // execution of that code was an operator's (N-01).

    fn with_basic_auth(cfg: &mut Config, password: &tempfile::NamedTempFile) {
        cfg.modules.cpu.enabled = true;
        let prom = cfg.outputs.prometheus.as_mut().unwrap();
        prom.basic_auth = Some(normalised::BasicAuth {
            username: "scraper".to_string(),
            password: muninn_core::secret::Secret::from_file(password.path()).unwrap(),
        });
    }

    fn prometheus_of(cfg: &Config) -> PluginInstance {
        render(&RenderContext::new(cfg))
            .into_iter()
            .find(|i| i.plugin == "prometheus_client")
            .expect("the prometheus output should render")
    }

    #[test]
    fn basic_auth_renders_both_halves() {
        let p = token_file("scrape-password-value");
        let cfg = config_with(|c| with_basic_auth(c, &p));
        let prom = prometheus_of(&cfg);
        assert_eq!(
            find(&prom, "basic_username").as_deref(),
            Some("\"scraper\"")
        );
        assert_eq!(
            find(&prom, "basic_password").as_deref(),
            Some("\"scrape-password-value\"")
        );
    }

    /// The credential goes through the same redaction path as the InfluxDB
    /// token, so `render-config` output stays safe to paste into an issue.
    #[test]
    fn the_basic_auth_password_is_redacted_like_every_other_secret() {
        let p = token_file("scrape-password-value");
        let cfg = config_with(|c| with_basic_auth(c, &p));
        let redacted = render(&RenderContext::redacted(&cfg))
            .into_iter()
            .find(|i| i.plugin == "prometheus_client")
            .unwrap();
        assert_eq!(
            find(&redacted, "basic_password").as_deref(),
            Some("\"***\"")
        );
    }

    #[test]
    fn no_tls_keys_are_rendered_when_none_are_configured() {
        let cfg = config_with(|c| c.modules.cpu.enabled = true);
        let prom = prometheus_of(&cfg);
        for key in ["tls_cert", "tls_key", "tls_allowed_cacerts"] {
            assert_eq!(find(&prom, key), None, "{key} should be absent");
        }
    }

    /// The option names are Telegraf's, taken from the pinned release's
    /// `sample.conf` rather than mirrored from the InfluxDB output — which is
    /// a *client* and spells its CA option differently because it means
    /// something else.
    #[test]
    fn server_tls_renders_telegrafs_own_option_names() {
        let cfg = config_with(|c| {
            c.modules.cpu.enabled = true;
            let prom = c.outputs.prometheus.as_mut().unwrap();
            prom.tls.cert_file = Some("/etc/ssl/muninn.crt".to_string());
            prom.tls.key_file = Some("/etc/ssl/muninn.key".to_string());
        });
        let prom = prometheus_of(&cfg);
        assert_eq!(
            find(&prom, "tls_cert").as_deref(),
            Some("\"/etc/ssl/muninn.crt\"")
        );
        assert_eq!(
            find(&prom, "tls_key").as_deref(),
            Some("\"/etc/ssl/muninn.key\"")
        );
        assert_eq!(
            find(&prom, "tls_allowed_cacerts"),
            None,
            "mutual TLS was not asked for"
        );
    }

    /// `tls_allowed_cacerts` is an array in the plugin, and a bare string is
    /// rejected at `config check` — the same shape mistake `urls` guards
    /// against on the InfluxDB side.
    #[test]
    fn a_client_ca_renders_as_an_array() {
        let cfg = config_with(|c| {
            c.modules.cpu.enabled = true;
            let prom = c.outputs.prometheus.as_mut().unwrap();
            prom.tls.cert_file = Some("/etc/ssl/muninn.crt".to_string());
            prom.tls.key_file = Some("/etc/ssl/muninn.key".to_string());
            prom.tls.client_ca_file = Some("/etc/ssl/clientca.pem".to_string());
        });
        assert_eq!(
            find(&prometheus_of(&cfg), "tls_allowed_cacerts").as_deref(),
            Some("[\"/etc/ssl/clientca.pem\"]")
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
