//! muninn's own operational metrics, in Prometheus text format.
//!
//! These are **not** your host metrics. Those come from Telegraf on its own
//! port. These describe the agent: whether Telegraf is running, how long
//! generation took, whether each module's last self-check succeeded.
//!
//! # Why they are here and not in Telegraf
//!
//! `muninn_telegraf_running 0` is worth reading precisely when Telegraf is
//! **not** running — which is exactly when Telegraf's own endpoint is gone.
//! Serving these through `outputs.prometheus_client` would mean they vanish in
//! the failure they exist to report. See
//! `docs/adr/0012-self-metrics-on-health-server.md`.
//!
//! # Cardinality
//!
//! Labels are version, module name and result status. Never an error string,
//! never a path, never a PID — a PID changes on every restart and would make a
//! new time series each time. The PID belongs in `/status`, where it is read by
//! a human, not in a metric that is stored forever.

use std::fmt::Write as _;

use crate::state::HealthState;

/// Render the `muninn_*` families.
pub fn render(state: &HealthState, muninn_version: &str) -> String {
    let details = state.details();
    let current = state.get();
    let mut out = String::with_capacity(1024);

    // muninn_info — the conventional "constant 1 with the interesting bits as
    // labels" pattern, so a dashboard can join on version without parsing it out
    // of anything.
    help(&mut out, "muninn_info", "gauge", "Build information.");
    let _ = writeln!(
        out,
        "muninn_info{{version=\"{}\",telegraf_version=\"{}\"}} 1",
        escape(muninn_version),
        escape(details.telegraf_version.as_deref().unwrap_or("unknown")),
    );

    help(
        &mut out,
        "muninn_state",
        "gauge",
        "Current supervisor state, as a label; the value is always 1.",
    );
    let _ = writeln!(
        out,
        "muninn_state{{state=\"{}\"}} 1",
        escape(current.as_str())
    );

    help(
        &mut out,
        "muninn_uptime_seconds",
        "gauge",
        "Process uptime.",
    );
    let _ = writeln!(
        out,
        "muninn_uptime_seconds {}",
        state.uptime().as_secs_f64()
    );

    help(
        &mut out,
        "muninn_ready",
        "gauge",
        "1 when /health/ready would succeed.",
    );
    let _ = writeln!(out, "muninn_ready {}", bool_value(current.is_ready()));

    help(
        &mut out,
        "muninn_telegraf_running",
        "gauge",
        "1 when Telegraf is running as a supervised child.",
    );
    let _ = writeln!(
        out,
        "muninn_telegraf_running {}",
        bool_value(details.telegraf_pid.is_some())
    );

    help(
        &mut out,
        "muninn_telegraf_restarts_total",
        "counter",
        "Times Telegraf has been restarted by muninn. Zero unless a bounded restart policy is enabled.",
    );
    let _ = writeln!(
        out,
        "muninn_telegraf_restarts_total {}",
        state.telegraf_restarts()
    );

    // Durations are omitted rather than reported as zero when the step has not
    // run: 0 would read as "instantaneous" on a graph, which is a different
    // claim from "has not happened".
    if let Some(d) = details.config_generation {
        help(
            &mut out,
            "muninn_config_generation_duration_seconds",
            "gauge",
            "Time taken to render the Telegraf configuration.",
        );
        let _ = writeln!(
            out,
            "muninn_config_generation_duration_seconds {}",
            d.as_secs_f64()
        );
    }

    if let Some(d) = details.telegraf_validation {
        help(
            &mut out,
            "muninn_telegraf_validation_duration_seconds",
            "gauge",
            "Time taken by `telegraf config check`.",
        );
        let _ = writeln!(
            out,
            "muninn_telegraf_validation_duration_seconds {}",
            d.as_secs_f64()
        );
    }

    if !details.module_checks.is_empty() {
        help(
            &mut out,
            "muninn_module_check_success",
            "gauge",
            "1 when a module's last self-check succeeded.",
        );
        for (module, check) in &details.module_checks {
            let _ = writeln!(
                out,
                "muninn_module_check_success{{module=\"{}\"}} {}",
                escape(module),
                bool_value(check.success)
            );
        }

        help(
            &mut out,
            "muninn_module_check_timestamp_seconds",
            "gauge",
            "When a module last completed a self-check.",
        );
        for (module, check) in &details.module_checks {
            let _ = writeln!(
                out,
                "muninn_module_check_timestamp_seconds{{module=\"{}\"}} {}",
                escape(module),
                check.at
            );
        }
    }

    out
}

fn help(out: &mut String, name: &str, kind: &str, text: &str) {
    let _ = writeln!(out, "# HELP {name} {text}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

fn bool_value(b: bool) -> u8 {
    if b { 1 } else { 0 }
}

/// Escape a label value.
///
/// Every label muninn emits is a version string, a state name or a module id —
/// none of which can contain these characters today. Escaping anyway costs
/// nothing and means a future label taken from configuration cannot break the
/// exposition format, which is the same reasoning as centralising TOML escaping
/// in the renderer.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str(r"\n"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    fn ready_state() -> HealthState {
        let s = HealthState::new();
        s.set(State::Ready);
        s.update(|d| {
            d.telegraf_version = Some("1.39.2".into());
            d.telegraf_pid = Some(17);
        });
        s
    }

    #[test]
    fn every_family_declares_its_type() {
        let out = render(&ready_state(), "0.1.0");
        for line in out.lines().filter(|l| !l.starts_with('#')) {
            let name = line.split(['{', ' ']).next().unwrap();
            assert!(
                out.contains(&format!("# TYPE {name} ")),
                "{name} has no TYPE declaration"
            );
        }
    }

    #[test]
    fn a_ready_agent_reports_ready_and_running() {
        let out = render(&ready_state(), "0.1.0");
        assert!(out.contains("muninn_ready 1"), "{out}");
        assert!(out.contains("muninn_telegraf_running 1"), "{out}");
        assert!(out.contains("muninn_state{state=\"ready\"} 1"), "{out}");
        assert!(
            out.contains("muninn_info{version=\"0.1.0\",telegraf_version=\"1.39.2\"} 1"),
            "{out}"
        );
    }

    /// The reason these metrics exist at all: they must still be servable, and
    /// still truthful, when Telegraf is gone.
    #[test]
    fn a_failed_agent_reports_not_ready_and_not_running() {
        let s = HealthState::new();
        s.set(State::Failed);
        s.update(|d| d.last_telegraf_exit = Some("exit code 137".into()));
        let out = render(&s, "0.1.0");
        assert!(out.contains("muninn_ready 0"), "{out}");
        assert!(out.contains("muninn_telegraf_running 0"), "{out}");
        assert!(out.contains("muninn_state{state=\"failed\"} 1"), "{out}");
    }

    /// `Degraded` is ready. A metric that disagreed with `/health/ready` would
    /// be worse than no metric.
    #[test]
    fn degraded_reports_ready_matching_the_endpoint() {
        let s = HealthState::new();
        s.set(State::Degraded);
        s.update(|d| d.telegraf_pid = Some(17));
        let out = render(&s, "0.1.0");
        assert!(out.contains("muninn_ready 1"), "{out}");
        assert_eq!(s.is_ready(), out.contains("muninn_ready 1"));
    }

    /// Zero would read as "instantaneous" on a graph, which is a different claim
    /// from "this step has not run".
    #[test]
    fn durations_are_omitted_rather_than_reported_as_zero() {
        let out = render(&HealthState::new(), "0.1.0");
        assert!(!out.contains("muninn_config_generation_duration_seconds"));
        assert!(!out.contains("muninn_telegraf_validation_duration_seconds"));

        let s = HealthState::new();
        s.update(|d| d.config_generation = Some(std::time::Duration::from_millis(12)));
        let out = render(&s, "0.1.0");
        assert!(
            out.contains("muninn_config_generation_duration_seconds 0.012"),
            "{out}"
        );
    }

    #[test]
    fn module_checks_render_one_series_per_module() {
        let s = HealthState::new();
        s.record_module_check("updates", false);
        s.record_module_check("docker", true);
        let out = render(&s, "0.1.0");
        assert!(
            out.contains("muninn_module_check_success{module=\"updates\"} 0"),
            "{out}"
        );
        assert!(
            out.contains("muninn_module_check_success{module=\"docker\"} 1"),
            "{out}"
        );
        assert!(
            out.contains("muninn_module_check_timestamp_seconds{module=\"updates\"}"),
            "{out}"
        );
    }

    /// The cardinality rule, asserted rather than trusted. A PID label would
    /// start a new time series on every restart.
    #[test]
    fn no_metric_carries_a_pid_a_path_or_an_error_string() {
        let s = HealthState::new();
        s.set(State::Failed);
        s.update(|d| {
            d.telegraf_pid = Some(31337);
            d.last_telegraf_exit =
                Some("exit code 137: /run/muninn/telegraf.conf unreadable".into());
            d.telegraf_version = Some("1.39.2".into());
        });
        let out = render(&s, "0.1.0");
        assert!(!out.contains("31337"), "a PID reached the metrics:\n{out}");
        assert!(
            !out.contains("/run/muninn"),
            "a path reached the metrics:\n{out}"
        );
        assert!(
            !out.contains("unreadable"),
            "an error string reached the metrics:\n{out}"
        );
    }

    /// Every label muninn emits is safe today; escaping means a future one taken
    /// from configuration cannot break the exposition format.
    #[test]
    fn label_values_are_escaped() {
        let s = HealthState::new();
        s.update(|d| d.telegraf_version = Some("1.0\"; evil=\"".into()));
        let out = render(&s, "0.1.0");
        assert!(out.contains(r#"telegraf_version="1.0\"; evil=\""#), "{out}");
        // One label pair per line, still parseable.
        let info = out.lines().find(|l| l.starts_with("muninn_info")).unwrap();
        assert_eq!(info.matches("} 1").count(), 1, "{info}");
    }

    /// Two scrapes of an unchanged state must produce the same series in the
    /// same order, or every scrape looks like a change.
    #[test]
    fn rendering_is_stable_between_scrapes() {
        let s = ready_state();
        s.record_module_check("updates", true);
        s.record_module_check("docker", true);
        let strip = |t: String| {
            t.lines()
                .filter(|l| !l.starts_with("muninn_uptime_seconds"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(strip(render(&s, "0.1.0")), strip(render(&s, "0.1.0")));
    }

    #[test]
    fn an_unknown_telegraf_version_is_stated_rather_than_left_blank() {
        let out = render(&HealthState::new(), "0.1.0");
        assert!(out.contains("telegraf_version=\"unknown\""), "{out}");
    }
}
