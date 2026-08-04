//! Configuration tests.
//!
//! The brief asks for at least one negative test per field. That is the shape
//! here: for every rule, a case that breaks it and an assertion that the error
//! **names the key**. An error message that does not say which key is wrong
//! sends the operator back to the documentation, which is the failure this whole
//! layer exists to avoid.

use super::*;
use crate::config::model::*;
use std::io::Write;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn token_file(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    write!(f, "{content}").unwrap();
    f.flush().unwrap();
    f
}

fn path_of(f: &tempfile::NamedTempFile) -> String {
    f.path().display().to_string().replace('\\', "/")
}

/// The smallest configuration that validates: one module, one output.
const MINIMAL: &str = r#"
version: 1
modules:
  cpu:
    enabled: true
outputs:
  prometheus:
    enabled: true
"#;

fn load_str(yaml: &str) -> Result<(ConfigV1, Vec<String>)> {
    loader::from_str(yaml, &Overrides::default())
}

fn ok(yaml: &str) -> ConfigV1 {
    load_str(yaml)
        .unwrap_or_else(|e| panic!("expected this to validate, got: {e}\n---\n{yaml}"))
        .0
}

fn warnings_of(yaml: &str) -> Vec<String> {
    load_str(yaml).expect("expected this to validate").1
}

/// Assert the load fails and that the message names `key`.
fn rejects(yaml: &str, key: &str) -> String {
    let err = load_str(yaml)
        .err()
        .unwrap_or_else(|| panic!("expected rejection mentioning '{key}'\n---\n{yaml}"));
    let msg = err.to_string();
    assert!(
        msg.contains(key),
        "error should name '{key}', got: {msg}\n---\n{yaml}"
    );
    msg
}

/// Build a valid document around `block`.
///
/// The block is inserted verbatim; the `modules:` and `outputs:` sections are
/// filled in only if the block does not define them itself. Appending them
/// unconditionally would produce a duplicate mapping key, which YAML rejects —
/// so a test would fail for the wrong reason and prove nothing about the rule it
/// was written for.
fn with(block: &str) -> String {
    let defines =
        |section: &str| block.starts_with(section) || block.contains(&format!("\n{section}"));
    let mut s = String::from("version: 1\n");
    s.push_str(block);
    if !s.ends_with('\n') {
        s.push('\n');
    }
    if !defines("modules:") {
        s.push_str("modules:\n  cpu:\n    enabled: true\n");
    }
    if !defines("outputs:") {
        s.push_str("outputs:\n  prometheus:\n    enabled: true\n");
    }
    s
}

// ---------------------------------------------------------------------------
// Schema version
// ---------------------------------------------------------------------------

#[test]
fn accepts_the_minimal_configuration() {
    let cfg = ok(MINIMAL);
    assert_eq!(cfg.version, 1);
    assert!(cfg.modules.cpu.enabled);
    assert!(cfg.outputs.prometheus.enabled);
}

#[test]
fn missing_version_is_rejected_by_name() {
    let msg = rejects(
        r#"
modules:
  cpu:
    enabled: true
outputs:
  prometheus:
    enabled: true
"#,
        "version",
    );
    assert!(msg.contains("version: 1"), "should say what to add: {msg}");
}

#[test]
fn unknown_version_is_rejected_with_its_number() {
    let msg = rejects(&MINIMAL.replace("version: 1", "version: 2"), "version 2");
    assert!(msg.contains("understands version 1"), "got: {msg}");
}

/// The reason the version is probed before the full parse: a future schema has
/// keys this build does not know, and forty "unknown field" errors would bury
/// the one fact that matters.
#[test]
fn unknown_version_is_reported_before_unknown_keys() {
    let msg = rejects(
        r#"
version: 99
modules:
  cpu:
    enabled: true
some_key_from_the_future:
  nested: true
"#,
        "version 99",
    );
    assert!(
        !msg.contains("some_key_from_the_future"),
        "version should be reported alone, got: {msg}"
    );
}

#[test]
fn non_integer_version_is_rejected() {
    assert!(load_str(&MINIMAL.replace("version: 1", "version: \"one\"")).is_err());
}

#[test]
fn malformed_yaml_is_reported_as_yaml_not_as_a_missing_version() {
    let msg = rejects("version: 1\n  bad indentation: [", "YAML");
    assert!(!msg.contains("missing required key"), "got: {msg}");
}

// ---------------------------------------------------------------------------
// Unknown fields
// ---------------------------------------------------------------------------

/// The rule this whole layer stands on. A silently-ignored key leaves the
/// operator believing something is configured when it is not.
#[test]
fn unknown_key_at_top_level_is_rejected() {
    rejects(&with("nonsense_key: true\n"), "nonsense_key");
}

#[test]
fn unknown_key_in_every_section_is_rejected() {
    let cases = [
        ("agent:\n  nonsense: 1\n", "nonsense"),
        ("runtime:\n  nonsense: 1\n", "nonsense"),
        ("logging:\n  nonsense: 1\n", "nonsense"),
        ("health:\n  nonsense: 1\n", "nonsense"),
        ("modules:\n  nonsense:\n    enabled: true\n", "nonsense"),
        ("modules:\n  cpu:\n    nonsense: true\n", "nonsense"),
        ("modules:\n  disks:\n    nonsense: []\n", "nonsense"),
        ("modules:\n  docker:\n    nonsense: 1\n", "nonsense"),
        ("outputs:\n  nonsense:\n    enabled: true\n", "nonsense"),
        ("outputs:\n  influxdb:\n    nonsense: 1\n", "nonsense"),
        ("outputs:\n  prometheus:\n    nonsense: 1\n", "nonsense"),
        (
            "outputs:\n  influxdb:\n    tls:\n      nonsense: 1\n",
            "nonsense",
        ),
    ];
    for (block, key) in cases {
        rejects(&with(block), key);
    }
}

/// The realistic typo: a key that is nearly right. `exclude_mountpoint` without
/// the `s` would silently disable an exclusion the operator believes is active.
#[test]
fn a_near_miss_key_is_rejected_rather_than_ignored() {
    rejects(
        &with("modules:\n  disks:\n    enabled: true\n    exclude_mountpoint: [\"/snap*\"]\n"),
        "exclude_mountpoint",
    );
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

#[test]
fn omitted_sections_take_their_documented_defaults() {
    let cfg = ok(MINIMAL);
    assert_eq!(cfg.agent.interval.as_secs(), 30);
    assert_eq!(cfg.agent.flush_interval.as_secs(), 30);
    assert_eq!(cfg.agent.hostname, "");
    assert!(!cfg.agent.omit_hostname);
    assert_eq!(cfg.runtime.shutdown_grace_period.as_secs(), 20);
    assert_eq!(cfg.runtime.telegraf_start_timeout.as_secs(), 15);
    assert_eq!(
        cfg.runtime.generated_config_path,
        "/run/muninn/telegraf.conf"
    );
    assert_eq!(cfg.runtime.host_mount_prefix, "/hostfs");
    assert_eq!(cfg.logging.format, LogFormat::Human);
    assert_eq!(cfg.logging.level, LogLevel::Info);
    assert_eq!(cfg.health.listen, "0.0.0.0:8080");
    assert_eq!(cfg.outputs.prometheus.listen, "0.0.0.0:9273");
    assert_eq!(cfg.outputs.prometheus.path, "/metrics");
    assert_eq!(cfg.outputs.prometheus.expiration_interval.as_secs(), 60);
}

/// A module nobody named is off. This is what makes "what the file says is what
/// runs" true, and why there are no profiles.
#[test]
fn every_unnamed_module_is_off() {
    let cfg = ok(MINIMAL);
    assert_eq!(cfg.modules.enabled_names(), vec!["cpu"]);
    assert!(!cfg.modules.docker.enabled);
    assert!(!cfg.modules.updates.enabled);
    assert!(!cfg.modules.disks.enabled);
}

/// An empty block must behave exactly like an omitted one, or the two forms
/// would quietly disagree.
#[test]
fn an_empty_section_matches_an_omitted_one() {
    let omitted = ok(MINIMAL);
    let empty = ok(&with("agent: {}\nruntime: {}\nlogging: {}\nhealth: {}\n"));
    assert_eq!(omitted.agent.interval, empty.agent.interval);
    assert_eq!(
        omitted.runtime.host_mount_prefix,
        empty.runtime.host_mount_prefix
    );
    assert_eq!(omitted.logging.level, empty.logging.level);
    assert_eq!(omitted.health.listen, empty.health.listen);
}

#[test]
fn module_and_output_ordering_is_fixed_not_file_order() {
    let cfg = ok(r#"
version: 1
modules:
  network:
    enabled: true
  cpu:
    enabled: true
  disks:
    enabled: true
outputs:
  prometheus:
    enabled: true
"#);
    // Declaration order in the model, not the order the file happened to use.
    assert_eq!(cfg.modules.enabled_names(), vec!["cpu", "disks", "network"]);
}

// ---------------------------------------------------------------------------
// Durations
// ---------------------------------------------------------------------------

#[test]
fn durations_accept_the_documented_forms() {
    let cfg = ok(&with("agent:\n  interval: 5m\n  flush_interval: 1h\n"));
    assert_eq!(cfg.agent.interval.as_secs(), 300);
    assert_eq!(cfg.agent.flush_interval.as_secs(), 3600);
}

#[test]
fn a_bare_number_is_not_a_duration() {
    let msg = rejects(&with("agent:\n  interval: 30\n"), "interval");
    assert!(!msg.is_empty());
}

#[test]
fn zero_durations_are_rejected_by_name() {
    for (block, key) in [
        ("agent:\n  interval: 0s\n", "agent.interval"),
        ("agent:\n  flush_interval: 0s\n", "agent.flush_interval"),
        (
            "runtime:\n  shutdown_grace_period: 0s\n",
            "runtime.shutdown_grace_period",
        ),
        (
            "runtime:\n  telegraf_start_timeout: 0s\n",
            "runtime.telegraf_start_timeout",
        ),
    ] {
        rejects(&with(block), key);
    }
}

/// Almost always a unit mistake — `500ms` where `500s` was meant — and it costs
/// real CPU on the machine being measured.
#[test]
fn sub_second_collection_is_rejected() {
    let msg = rejects(&with("agent:\n  interval: 500ms\n"), "agent.interval");
    assert!(msg.contains("1s or more"), "should say what to use: {msg}");
}

// ---------------------------------------------------------------------------
// Modules
// ---------------------------------------------------------------------------

/// Outputs but no modules: the agent starts, connects, and reports nothing.
#[test]
fn no_module_enabled_is_fatal() {
    let msg = rejects(
        r#"
version: 1
outputs:
  prometheus:
    enabled: true
"#,
        "no module is enabled",
    );
    assert!(msg.contains("collect nothing"), "got: {msg}");
}

/// A stray `- ` left behind while editing a list matches nothing, and would
/// silently do nothing.
#[test]
fn a_blank_pattern_in_any_list_is_rejected_with_its_index() {
    let cases = [
        (
            "modules:\n  disks:\n    enabled: true\n    exclude_mountpoints: [\"/snap*\", \"\"]\n",
            "modules.disks.exclude_mountpoints[1]",
        ),
        (
            "modules:\n  disks:\n    enabled: true\n    exclude_filesystems: [\"\"]\n",
            "modules.disks.exclude_filesystems[0]",
        ),
        (
            "modules:\n  network:\n    enabled: true\n    exclude_interfaces: [\"lo\", \"  \"]\n",
            "modules.network.exclude_interfaces[1]",
        ),
        (
            "modules:\n  disk_io:\n    enabled: true\n    exclude_devices: [\"\"]\n",
            "modules.disk_io.exclude_devices[0]",
        ),
        (
            "modules:\n  image_updates:\n    enabled: true\n    container_include: [\"\"]\n",
            "modules.image_updates.container_include[0]",
        ),
    ];
    for (block, key) in cases {
        rejects(&with(block), key);
    }
}

#[test]
fn docker_endpoint_must_have_a_known_scheme() {
    rejects(
        &with("modules:\n  docker:\n    enabled: true\n    endpoint: \"/var/run/docker.sock\"\n"),
        "modules.docker.endpoint",
    );
}

#[test]
fn docker_endpoint_must_not_be_empty_when_enabled() {
    rejects(
        &with("modules:\n  docker:\n    enabled: true\n    endpoint: \"\"\n"),
        "modules.docker.endpoint",
    );
}

/// A bare scheme passes a `starts_with` test and names nothing. Left unchecked
/// it reaches the runtime layer with no address to probe, which reports no
/// problem — so the module would start and quietly collect nothing.
#[test]
fn a_docker_endpoint_that_is_only_a_scheme_is_rejected() {
    for endpoint in ["unix://", "tcp://"] {
        let msg = rejects(
            &with(&format!(
                "modules:
  docker:
    enabled: true
    endpoint: \"{endpoint}\"
"
            )),
            "modules.docker.endpoint",
        );
        assert!(
            msg.contains("docker.sock") || msg.contains("proxy"),
            "should show what a complete endpoint looks like: {msg}"
        );
    }
}

/// Telegraf does not reject an unknown container state — it simply matches no
/// container. A typo would therefore produce a module that runs and reports
/// nothing, which reads as "no containers".
#[test]
fn an_unknown_container_state_is_rejected() {
    let msg = rejects(
        &with(
            "modules:
  docker:
    enabled: true
    container_states: [runnning]
",
        ),
        "modules.docker.container_states",
    );
    assert!(msg.contains("running"), "should list the valid ones: {msg}");
}

/// The same failure by a different route: selecting no state at all.
#[test]
fn an_empty_container_state_list_is_rejected() {
    rejects(
        &with(
            "modules:
  docker:
    enabled: true
    container_states: []
",
        ),
        "modules.docker.container_states",
    );
}

/// Alerting on a container that crashed needs it to still be reported, so
/// `exited` has to be selectable.
#[test]
fn exited_containers_can_be_selected() {
    let cfg = ok(&with(
        "modules:
  docker:
    enabled: true
    container_states: [running, exited]
",
    ));
    assert_eq!(cfg.modules.docker.container_states, ["running", "exited"]);
}

/// Enabling the Docker module grants root-equivalent access to the host. The
/// operator should be told at startup, not only in the documentation they may
/// not have read.
#[test]
fn enabling_docker_warns_about_the_socket() {
    let w = warnings_of(&with("modules:\n  docker:\n    enabled: true\n"));
    assert!(
        w.iter().any(|s| s.contains("root")),
        "expected a socket warning, got: {w:?}"
    );
}

#[test]
fn updates_interval_below_a_minute_is_rejected() {
    let msg = rejects(
        &with("modules:\n  updates:\n    enabled: true\n    interval: 30s\n"),
        "modules.updates.interval",
    );
    assert!(msg.contains("1m or more"), "got: {msg}");
}

#[test]
fn image_updates_interval_below_a_minute_is_rejected() {
    let msg = rejects(
        &with("modules:\n  image_updates:\n    enabled: true\n    interval: 30s\n"),
        "modules.image_updates.interval",
    );
    assert!(msg.contains("1m or more"), "got: {msg}");
}

#[test]
fn image_updates_endpoint_must_have_a_known_scheme() {
    rejects(
        &with(
            "modules:\n  image_updates:\n    enabled: true\n    \
             endpoint: \"/var/run/docker.sock\"\n",
        ),
        "modules.image_updates.endpoint",
    );
}

#[test]
fn image_updates_endpoint_must_not_be_empty_when_enabled() {
    rejects(
        &with("modules:\n  image_updates:\n    enabled: true\n    endpoint: \"\"\n"),
        "modules.image_updates.endpoint",
    );
}

/// The same failure mode docker's endpoint check exists for: a bare scheme
/// passes a `starts_with` test and names nothing to probe.
#[test]
fn an_image_updates_endpoint_that_is_only_a_scheme_is_rejected() {
    for endpoint in ["unix://", "tcp://"] {
        let msg = rejects(
            &with(&format!(
                "modules:
  image_updates:
    enabled: true
    endpoint: \"{endpoint}\"
"
            )),
            "modules.image_updates.endpoint",
        );
        assert!(
            msg.contains("docker.sock") || msg.contains("proxy"),
            "should show what a complete endpoint looks like: {msg}"
        );
    }
}

/// Same exposure as the docker module — same socket, same warning.
#[test]
fn enabling_image_updates_warns_about_the_socket() {
    let w = warnings_of(&with("modules:\n  image_updates:\n    enabled: true\n"));
    assert!(
        w.iter().any(|s| s.contains("root")),
        "expected a socket warning, got: {w:?}"
    );
}

#[test]
fn image_updates_defaults_match_docker_and_updates() {
    let cfg = ok(&with("modules:\n  image_updates:\n    enabled: true\n"));
    assert_eq!(
        cfg.modules.image_updates.endpoint,
        "unix:///var/run/docker.sock"
    );
    assert_eq!(cfg.modules.image_updates.timeout.as_secs(), 5);
    assert_eq!(cfg.modules.image_updates.interval.as_secs(), 3600);
    assert!(cfg.modules.image_updates.container_include.is_empty());
    assert!(cfg.modules.image_updates.container_exclude.is_empty());
}

/// The registry call is the one that leaves the host — a TLS handshake, a
/// token exchange and a manifest fetch — so it does not share the local
/// socket call's five seconds.
#[test]
fn the_registry_lookup_gets_a_longer_default_than_a_local_api_call() {
    let cfg = ok(&with("modules:\n  image_updates:\n    enabled: true\n"));
    assert_eq!(cfg.modules.image_updates.registry_timeout.as_secs(), 30);
    assert!(
        cfg.modules.image_updates.registry_timeout > cfg.modules.image_updates.timeout,
        "the call that reaches a registry must get more patience than one the daemon answers itself"
    );
}

#[test]
fn a_zero_registry_timeout_is_rejected() {
    rejects(
        &with("modules:\n  image_updates:\n    enabled: true\n    registry_timeout: 0s\n"),
        "modules.image_updates.registry_timeout",
    );
}

/// Not refused — an operator may have a reason — but the symptom of getting
/// this backwards (`distribution_query_failed` against a registry that answers
/// fine by hand) points nowhere near the cause, so it is named here.
#[test]
fn a_registry_timeout_shorter_than_the_api_timeout_warns() {
    let w = warnings_of(&with(
        "modules:\n  image_updates:\n    enabled: true\n    timeout: 30s\n    \
         registry_timeout: 5s\n",
    ));
    assert!(
        w.iter().any(|s| s.contains("registry_timeout")),
        "expected a warning about the inverted timeouts, got: {w:?}"
    );
}

#[test]
fn an_empty_host_mount_prefix_warns_when_host_modules_are_on() {
    let w = warnings_of(&with("runtime:\n  host_mount_prefix: \"\"\n"));
    assert!(
        w.iter().any(|s| s.contains("host_mount_prefix")),
        "expected a warning, got: {w:?}"
    );
}

#[test]
fn a_relative_host_mount_prefix_is_rejected() {
    let msg = rejects(
        &with("runtime:\n  host_mount_prefix: \"hostfs\"\n"),
        "runtime.host_mount_prefix",
    );
    assert!(msg.contains("absolute"), "got: {msg}");
}

/// The same rule `generated_config_path` follows, and for the same reason: a
/// path the host calls absolute has to validate too, or the tests cannot point
/// the prefix at a temporary directory on the machine they run on. It was fixed
/// for one key and left `starts_with('/')` for the other.
#[test]
fn a_host_absolute_host_mount_prefix_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let prefix = dir.path().display().to_string().replace('\\', "/");
    ok(&with(&format!(
        "runtime:\n  host_mount_prefix: \"{prefix}\"\n"
    )));
}

/// The include list is already an allow-list, so exclusions on top only apply
/// within it — worth pointing at, not worth refusing.
#[test]
fn setting_both_include_and_exclude_warns_but_loads() {
    let w = warnings_of(&with(
        "modules:\n  network:\n    enabled: true\n    include_interfaces: [\"eth0\"]\n    exclude_interfaces: [\"lo\"]\n",
    ));
    assert!(
        w.iter().any(|s| s.contains("allow-list")),
        "expected a warning, got: {w:?}"
    );
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

#[test]
fn no_output_enabled_is_fatal() {
    let msg = rejects(
        r#"
version: 1
modules:
  cpu:
    enabled: true
"#,
        "no output is enabled",
    );
    assert!(msg.contains("send them nowhere"), "got: {msg}");
}

#[test]
fn both_outputs_may_be_enabled_together() {
    let t = token_file("tok");
    let cfg = ok(&with(&format!(
        r#"outputs:
  prometheus:
    enabled: true
  influxdb:
    enabled: true
    url: "https://influx.example:8086"
    organization: infra
    bucket: servers
    token_file: "{}"
"#,
        path_of(&t)
    )));
    assert_eq!(cfg.outputs.enabled_names(), vec!["influxdb", "prometheus"]);
}

/// Build an InfluxDB block, letting one field be blanked out.
fn influx_block(token_path: &str, blank: Option<&str>) -> String {
    let value = |field: &str, v: &str| {
        if blank == Some(field) {
            format!("    {field}: \"\"\n")
        } else {
            format!("    {field}: {v}\n")
        }
    };
    format!(
        "outputs:\n  influxdb:\n    enabled: true\n{}{}{}{}",
        value("url", "\"https://influx.example:8086\""),
        value("organization", "infra"),
        value("bucket", "servers"),
        value("token_file", &format!("\"{token_path}\"")),
    )
}

#[test]
fn every_required_influxdb_field_is_rejected_when_missing() {
    let t = token_file("tok");
    for field in ["url", "organization", "bucket", "token_file"] {
        rejects(
            &with(&influx_block(&path_of(&t), Some(field))),
            &format!("outputs.influxdb.{field}"),
        );
    }
}

#[test]
fn a_complete_influxdb_block_validates() {
    let t = token_file("tok");
    let cfg = ok(&with(&influx_block(&path_of(&t), None)));
    assert!(cfg.outputs.influxdb.enabled);
}

#[test]
fn influxdb_url_must_be_absolute() {
    let t = token_file("tok");
    rejects(
        &with(&format!(
            r#"outputs:
  influxdb:
    enabled: true
    url: "influx.example:8086"
    organization: infra
    bucket: servers
    token_file: "{}"
"#,
            path_of(&t)
        )),
        "outputs.influxdb.url",
    );
}

/// A missing token discovered ten minutes in looks like an InfluxDB outage.
/// Discovered here it names the path.
#[test]
fn a_missing_influxdb_token_file_is_fatal_at_load() {
    let err = load_str(&with(
        r#"outputs:
  influxdb:
    enabled: true
    url: "https://influx.example:8086"
    organization: infra
    bucket: servers
    token_file: "/nonexistent/muninn-token"
"#,
    ))
    .unwrap_err();
    assert!(err.to_string().contains("muninn-token"), "got: {err}");
    assert_eq!(err.exit_code(), crate::exit::SECRET);
}

#[test]
fn an_empty_influxdb_token_file_is_fatal() {
    let t = token_file("   \n");
    let err = load_str(&with(&format!(
        r#"outputs:
  influxdb:
    enabled: true
    url: "https://influx.example:8086"
    organization: infra
    bucket: servers
    token_file: "{}"
"#,
        path_of(&t)
    )))
    .unwrap_err();
    assert!(err.to_string().contains("empty"), "got: {err}");
}

/// A disabled output is not validated — an operator should be able to leave a
/// half-written InfluxDB block in place while running on Prometheus only.
#[test]
fn a_disabled_output_is_not_validated() {
    ok(&with(
        r#"outputs:
  prometheus:
    enabled: true
  influxdb:
    enabled: false
    url: ""
    token_file: "/nonexistent/token"
"#,
    ));
}

#[test]
fn plaintext_influxdb_warns_about_the_token() {
    let t = token_file("tok");
    let w = warnings_of(&with(&format!(
        r#"outputs:
  influxdb:
    enabled: true
    url: "http://influx.example:8086"
    organization: infra
    bucket: servers
    token_file: "{}"
"#,
        path_of(&t)
    )));
    assert!(w.iter().any(|s| s.contains("plaintext HTTP")), "got: {w:?}");
}

#[test]
fn insecure_skip_verify_warns_loudly() {
    let t = token_file("tok");
    let w = warnings_of(&with(&format!(
        r#"outputs:
  influxdb:
    enabled: true
    url: "https://influx.example:8086"
    organization: infra
    bucket: servers
    token_file: "{}"
    tls:
      insecure_skip_verify: true
"#,
        path_of(&t)
    )));
    assert!(
        w.iter().any(|s| s.contains("DISABLED")),
        "expected a prominent warning, got: {w:?}"
    );
}

/// Half a client certificate is not a client certificate — TLS would silently
/// fall back to server-only authentication.
#[test]
fn a_client_certificate_without_its_key_is_rejected() {
    let t = token_file("tok");
    let cert = token_file("cert");
    rejects(
        &with(&format!(
            r#"outputs:
  influxdb:
    enabled: true
    url: "https://influx.example:8086"
    organization: infra
    bucket: servers
    token_file: "{}"
    tls:
      cert_file: "{}"
"#,
            path_of(&t),
            path_of(&cert)
        )),
        "cert_file",
    );
}

#[test]
fn prometheus_path_must_start_with_a_slash() {
    rejects(
        &with("outputs:\n  prometheus:\n    enabled: true\n    path: metrics\n"),
        "outputs.prometheus.path",
    );
}

#[test]
fn prometheus_listen_must_include_a_port() {
    let msg = rejects(
        &with("outputs:\n  prometheus:\n    enabled: true\n    listen: \"0.0.0.0\"\n"),
        "outputs.prometheus.listen",
    );
    assert!(msg.contains("0.0.0.0"), "got: {msg}");
}

#[test]
fn health_listen_must_include_a_port() {
    let msg = rejects(&with("health:\n  listen: \"localhost\"\n"), "health.listen");
    assert!(
        msg.contains("container"),
        "should mention the 0.0.0.0 trap: {msg}"
    );
}

#[test]
fn basic_auth_needs_both_halves() {
    rejects(
        &with(
            "outputs:\n  prometheus:\n    enabled: true\n    basic_auth:\n      username: scraper\n",
        ),
        "password_file",
    );
    let p = token_file("pw");
    rejects(
        &with(&format!(
            "outputs:\n  prometheus:\n    enabled: true\n    basic_auth:\n      password_file: \"{}\"\n",
            path_of(&p)
        )),
        "username",
    );
}

#[test]
fn basic_auth_password_file_must_exist() {
    let err = load_str(&with(
        "outputs:\n  prometheus:\n    enabled: true\n    basic_auth:\n      username: scraper\n      password_file: \"/nonexistent/pw\"\n",
    ))
    .unwrap_err();
    assert_eq!(err.exit_code(), crate::exit::SECRET);
}

// ---------------------------------------------------------------------------
// Port collisions
// ---------------------------------------------------------------------------

#[test]
fn identical_listen_addresses_collide() {
    let msg = rejects(
        &with(
            "health:\n  listen: \"0.0.0.0:9273\"\noutputs:\n  prometheus:\n    enabled: true\n    listen: \"0.0.0.0:9273\"\n",
        ),
        "cannot both bind",
    );
    assert!(msg.contains("9273"), "should name the port: {msg}");
}

/// The case an equality check misses. A wildcard address already covers every
/// specific one, so the second bind fails — and the symptom is a listener that
/// silently never starts while readiness still reports true.
#[test]
fn a_wildcard_collides_with_a_specific_address_on_the_same_port() {
    rejects(
        &with(
            "health:\n  listen: \"0.0.0.0:8080\"\noutputs:\n  prometheus:\n    enabled: true\n    listen: \"127.0.0.1:8080\"\n",
        ),
        "cannot both bind",
    );
    // ...and the other way round.
    rejects(
        &with(
            "health:\n  listen: \"127.0.0.1:8080\"\noutputs:\n  prometheus:\n    enabled: true\n    listen: \"0.0.0.0:8080\"\n",
        ),
        "cannot both bind",
    );
}

#[test]
fn different_ports_never_collide() {
    ok(&with(
        "health:\n  listen: \"0.0.0.0:8080\"\noutputs:\n  prometheus:\n    enabled: true\n    listen: \"0.0.0.0:9273\"\n",
    ));
}

/// Two specific addresses on one port genuinely can both bind.
#[test]
fn distinct_specific_addresses_may_share_a_port() {
    ok(&with(
        "health:\n  listen: \"127.0.0.1:8080\"\noutputs:\n  prometheus:\n    enabled: true\n    listen: \"127.0.0.2:8080\"\n",
    ));
}

/// Port 0 means "any free port", so two of them get different ones. Rejecting
/// this would refuse a configuration that works — and it is what the tests and
/// the integration stack use to avoid fighting over ports.
#[test]
fn port_zero_never_collides() {
    ok(&with(
        "health:
  listen: \"127.0.0.1:0\"
outputs:
  prometheus:
    enabled: true
    listen: \"127.0.0.1:0\"
",
    ));
    ok(&with(
        "health:
  listen: \"0.0.0.0:0\"
outputs:
  prometheus:
    enabled: true
    listen: \"0.0.0.0:0\"
",
    ));
}

/// ...but one real port and one zero must still not be confused for a conflict.
#[test]
fn a_real_port_does_not_collide_with_port_zero() {
    ok(&with(
        "health:
  listen: \"0.0.0.0:8080\"
outputs:
  prometheus:
    enabled: true
    listen: \"0.0.0.0:0\"
",
    ));
}

/// A disabled listener cannot collide with anything.
#[test]
fn a_disabled_prometheus_output_does_not_collide() {
    let t = token_file("tok");
    ok(&with(&format!(
        r#"health:
  listen: "0.0.0.0:9273"
outputs:
  prometheus:
    enabled: false
    listen: "0.0.0.0:9273"
  influxdb:
    enabled: true
    url: "https://influx.example:8086"
    organization: infra
    bucket: servers
    token_file: "{}"
"#,
        path_of(&t)
    )));
}

// ---------------------------------------------------------------------------
// Overrides
// ---------------------------------------------------------------------------

#[test]
fn overrides_replace_the_file_value() {
    let o = Overrides {
        log_level: Some("debug".into()),
        log_format: Some("json".into()),
    };
    let (cfg, w) = loader::from_str(MINIMAL, &o).unwrap();
    assert_eq!(cfg.logging.level, LogLevel::Debug);
    assert_eq!(cfg.logging.format, LogFormat::Json);
    assert!(w.is_empty(), "valid values should not warn: {w:?}");
}

#[test]
fn overrides_are_case_insensitive() {
    let o = Overrides {
        log_level: Some("DEBUG".into()),
        log_format: Some("JSON".into()),
    };
    let (cfg, _) = loader::from_str(MINIMAL, &o).unwrap();
    assert_eq!(cfg.logging.level, LogLevel::Debug);
    assert_eq!(cfg.logging.format, LogFormat::Json);
}

/// A typo must not look like a deliberate setting. Falling back to the default
/// would make `MUNINN_LOG_FORMAT=jsn` indistinguishable from not setting it, and
/// the operator would spend an afternoon wondering why their pipeline sees no
/// JSON.
#[test]
fn an_invalid_override_warns_and_keeps_the_file_value() {
    let yaml = with("logging:\n  format: json\n  level: warn\n");
    let o = Overrides {
        log_level: Some("verbose".into()),
        log_format: Some("jsn".into()),
    };
    let (cfg, w) = loader::from_str(&yaml, &o).unwrap();
    assert_eq!(
        cfg.logging.level,
        LogLevel::Warn,
        "must keep the file value"
    );
    assert_eq!(
        cfg.logging.format,
        LogFormat::Json,
        "must keep the file value"
    );
    assert_eq!(w.len(), 2, "each bad value warns: {w:?}");
    assert!(w.iter().any(|s| s.contains("verbose")));
    assert!(w.iter().any(|s| s.contains("jsn")));
}

/// CLI beats environment. The `Option` shape is what makes this possible: a
/// defaulted string could not express "not given".
#[test]
fn cli_overrides_beat_environment_overrides() {
    let env = Overrides {
        log_level: Some("error".into()),
        log_format: Some("human".into()),
    };
    let merged = env.merge_cli(Some("trace".into()), None);
    assert_eq!(merged.log_level.as_deref(), Some("trace"));
    assert_eq!(
        merged.log_format.as_deref(),
        Some("human"),
        "an absent CLI value must not clear the environment one"
    );
}

// ---------------------------------------------------------------------------
// Normalisation
// ---------------------------------------------------------------------------

#[test]
fn a_disabled_output_normalises_to_none() {
    let cfg = Config::from_v1(ok(MINIMAL)).unwrap();
    assert!(
        cfg.outputs.influxdb.is_none(),
        "disabled must not be constructible"
    );
    assert!(cfg.outputs.prometheus.is_some());
}

#[test]
fn normalisation_reads_the_token_once() {
    let t = token_file("s3cret\n");
    let cfg = Config::from_v1(ok(&with(&format!(
        r#"outputs:
  influxdb:
    enabled: true
    url: "https://influx.example:8086"
    organization: infra
    bucket: servers
    token_file: "{}"
"#,
        path_of(&t)
    ))))
    .unwrap();
    let influx = cfg.outputs.influxdb.unwrap();
    assert_eq!(influx.token.expose(), "s3cret");
}

/// The realistic leak path: someone logs the whole resolved config.
#[test]
fn debug_formatting_the_resolved_config_leaks_no_secret() {
    let t = token_file("s3cret-token-value");
    let cfg = Config::from_v1(ok(&with(&format!(
        r#"outputs:
  influxdb:
    enabled: true
    url: "https://influx.example:8086"
    organization: infra
    bucket: servers
    token_file: "{}"
"#,
        path_of(&t)
    ))))
    .unwrap();
    let dumped = format!("{cfg:?}");
    assert!(
        !dumped.contains("s3cret-token-value"),
        "the whole-config Debug leaked a token"
    );
    assert!(
        dumped.contains("influx.example"),
        "non-secrets should still appear"
    );
}

#[test]
fn addresses_are_parsed_once_during_normalisation() {
    let cfg = Config::from_v1(ok(MINIMAL)).unwrap();
    assert_eq!(cfg.health.listen.port(), 8080);
    assert!(cfg.health.listen.ip().is_unspecified());
    assert_eq!(cfg.outputs.prometheus.unwrap().listen.port(), 9273);
}

#[test]
fn an_empty_mount_prefix_normalises_to_none() {
    let cfg = Config::from_v1(ok(&with("runtime:\n  host_mount_prefix: \"\"\n"))).unwrap();
    assert!(cfg.runtime.host_mount_prefix.is_none());
    assert!(
        cfg.runtime.host_env().is_empty(),
        "no prefix means gopsutil's defaults are already right"
    );
}

/// `/` and `""` mean the same thing, and collapsing them here leaves downstream
/// code with one case instead of three.
#[test]
fn a_root_mount_prefix_is_the_same_as_none() {
    let cfg = Config::from_v1(ok(&with("runtime:\n  host_mount_prefix: \"/\"\n"))).unwrap();
    assert!(cfg.runtime.host_mount_prefix.is_none());
}

#[test]
fn the_mount_prefix_produces_every_host_variable_telegraf_needs() {
    let cfg = Config::from_v1(ok(MINIMAL)).unwrap();
    let env: std::collections::HashMap<_, _> = cfg.runtime.host_env().into_iter().collect();
    assert_eq!(env.get("HOST_MOUNT_PREFIX").unwrap(), "/hostfs");
    assert_eq!(env.get("HOST_PROC").unwrap(), "/hostfs/proc");
    assert_eq!(env.get("HOST_SYS").unwrap(), "/hostfs/sys");
    assert_eq!(env.get("HOST_ETC").unwrap(), "/hostfs/etc");
    assert_eq!(env.get("HOST_VAR").unwrap(), "/hostfs/var");
    assert_eq!(env.get("HOST_RUN").unwrap(), "/hostfs/run");
    assert_eq!(env.len(), 6);
}

/// Without HOST_MOUNT_PREFIX the path tags carry `/hostfs`, and every dashboard
/// filter has to know about the container's internals.
#[test]
fn host_mount_prefix_is_always_among_the_variables() {
    let cfg =
        Config::from_v1(ok(&with("runtime:\n  host_mount_prefix: \"/mnt/host/\"\n"))).unwrap();
    let env: std::collections::HashMap<_, _> = cfg.runtime.host_env().into_iter().collect();
    assert_eq!(
        env.get("HOST_MOUNT_PREFIX").unwrap(),
        "/mnt/host",
        "a trailing slash must be normalised away"
    );
    assert_eq!(env.get("HOST_PROC").unwrap(), "/mnt/host/proc");
}

#[test]
fn a_relative_generated_config_path_is_rejected() {
    rejects(
        &with("runtime:\n  generated_config_path: \"telegraf.conf\"\n"),
        "generated_config_path",
    );
}

/// The generated file holds resolved secrets, so a path outside the tmpfs
/// conventions is worth questioning.
#[test]
fn a_persistent_generated_config_path_warns_about_secrets() {
    let w = warnings_of(&with(
        "runtime:\n  generated_config_path: \"/var/lib/muninn/telegraf.conf\"\n",
    ));
    assert!(
        w.iter().any(|s| s.contains("tmpfs")),
        "expected a warning about persisting secrets, got: {w:?}"
    );
}

// ---------------------------------------------------------------------------
// Log level mapping
// ---------------------------------------------------------------------------

/// Telegraf has two verbosity knobs, not five. The mapping lives in one place so
/// call sites cannot invent their own.
#[test]
fn log_levels_map_onto_telegrafs_two_flags() {
    assert_eq!(LogLevel::Trace.telegraf_flags(), (true, false));
    assert_eq!(LogLevel::Debug.telegraf_flags(), (true, false));
    assert_eq!(LogLevel::Info.telegraf_flags(), (false, false));
    assert_eq!(LogLevel::Warn.telegraf_flags(), (false, true));
    assert_eq!(LogLevel::Error.telegraf_flags(), (false, true));
}

#[test]
fn debug_and_quiet_are_never_both_set() {
    for level in [
        LogLevel::Trace,
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
    ] {
        let (debug, quiet) = level.telegraf_flags();
        assert!(!(debug && quiet), "{level:?} sets both flags");
    }
}
