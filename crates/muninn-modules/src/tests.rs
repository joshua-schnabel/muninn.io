//! Shared test helpers and the cross-module behaviour tests.
//!
//! Per-module tests live next to their modules; what is here is what only makes
//! sense across them — merging, ordering, and the shape of the whole file.

use std::io::Write;

use muninn_core::Config;
use muninn_core::config::{Overrides, loader, normalised};

use crate::{RenderContext, build};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn token_file(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    write!(f, "{content}").unwrap();
    f.flush().unwrap();
    f
}

/// A minimal resolved configuration — Prometheus on, everything else off —
/// which the closure then adjusts.
///
/// Built through the real loader rather than by hand, so a test cannot
/// accidentally construct a configuration the validator would have rejected.
pub(crate) fn config_with(adjust: impl FnOnce(&mut Config)) -> Config {
    let yaml = r#"
version: 1
modules:
  cpu:
    enabled: true
outputs:
  prometheus:
    enabled: true
"#;
    let (v1, _) = loader::from_str(yaml, &Overrides::default()).expect("fixture must load");
    let mut cfg = Config::from_v1(v1).expect("fixture must resolve");
    cfg.modules.cpu.enabled = false; // the closure decides what is on
    adjust(&mut cfg);
    cfg
}

/// Switch InfluxDB on in an already-resolved configuration.
pub(crate) fn enable_influx(cfg: &mut Config, token: &tempfile::NamedTempFile) {
    cfg.modules.cpu.enabled = true;
    cfg.outputs.influxdb = Some(normalised::Influxdb {
        url: "https://influx.example:8086".to_string(),
        organization: "infra".to_string(),
        bucket: "servers".to_string(),
        token: muninn_core::secret::Secret::from_file(token.path()).unwrap(),
        timeout: muninn_core::duration::ConfigDuration::from_secs(5),
        tls: Default::default(),
    });
}

fn render_of(cfg: &Config) -> String {
    muninn_telegraf::render(&build(&RenderContext::new(cfg)), "0.1.0")
}

// ---------------------------------------------------------------------------
// Module selection
// ---------------------------------------------------------------------------

#[test]
fn only_enabled_modules_appear() {
    let cfg = config_with(|c| {
        c.modules.cpu.enabled = true;
        c.modules.swap.enabled = true;
    });
    let out = render_of(&cfg);
    assert!(out.contains("[[inputs.cpu]]"));
    assert!(out.contains("[[inputs.swap]]"));
    assert!(!out.contains("[[inputs.mem]]"), "memory was not enabled");
    assert!(!out.contains("[[inputs.disk]]"), "disks was not enabled");
}

/// Every module in the registry must be reachable from the configuration, or it
/// is dead code that looks like a feature.
#[test]
fn every_module_can_be_enabled() {
    for module in crate::all_modules() {
        let id = module.id();
        let cfg = config_with(|c| match id {
            "cpu" => c.modules.cpu.enabled = true,
            "memory" => c.modules.memory.enabled = true,
            "load" => c.modules.load.enabled = true,
            "system" => c.modules.system.enabled = true,
            "swap" => c.modules.swap.enabled = true,
            "processes" => c.modules.processes.enabled = true,
            "disks" => c.modules.disks.enabled = true,
            "disk_io" => c.modules.disk_io.enabled = true,
            "network" => c.modules.network.enabled = true,
            "docker" => c.modules.docker.enabled = true,
            "updates" => c.modules.updates.enabled = true,
            "image_updates" => c.modules.image_updates.enabled = true,
            other => panic!("module '{other}' has no way to be enabled from the configuration"),
        });
        assert!(
            module.enabled(&cfg),
            "{id} did not report itself enabled after being switched on"
        );
        assert!(
            !module.render(&RenderContext::new(&cfg)).is_empty(),
            "{id} rendered nothing"
        );
    }
}

// ---------------------------------------------------------------------------
// The load/system merge
// ---------------------------------------------------------------------------

/// Four cases, because the merge is the one mapping in the codebase that is not
/// one-to-one and is therefore the one a refactor is most likely to break.
#[test]
fn load_and_system_produce_exactly_one_instance() {
    let cases = [
        (true, false, Some(vec!["load"])),
        (false, true, Some(vec!["uptime", "users"])),
        (true, true, Some(vec!["load", "uptime", "users"])),
        (false, false, None),
    ];

    for (load, system, expected) in cases {
        let cfg = config_with(|c| {
            c.modules.cpu.enabled = true; // so the config is never empty
            c.modules.load.enabled = load;
            c.modules.system.enabled = system;
        });
        let out = render_of(&cfg);
        let blocks = out.matches("[[inputs.system]]").count();

        match expected {
            None => assert_eq!(blocks, 0, "neither enabled: expected no system block"),
            Some(groups) => {
                assert_eq!(
                    blocks, 1,
                    "load={load} system={system}: expected exactly one [[inputs.system]], \
                     two would collect every metric twice\n{out}"
                );
                let want = format!(
                    "include = [{}]",
                    groups
                        .iter()
                        .map(|g| format!("\"{g}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                assert!(out.contains(&want), "expected {want}\n{out}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Exclusions
// ---------------------------------------------------------------------------

/// Each `exclude_*` must land on the right tag. Getting the tag name wrong
/// produces a filter that matches nothing — valid TOML, accepted by Telegraf,
/// and silently ineffective.
#[test]
fn exclusions_render_onto_the_correct_tag() {
    let cfg = config_with(|c| {
        c.modules.disks.enabled = true;
        c.modules.disks.exclude_mountpoints = vec!["/snap*".into()];
        c.modules.disk_io.enabled = true;
        c.modules.disk_io.exclude_devices = vec!["loop*".into()];
        c.modules.network.enabled = true;
        c.modules.network.exclude_interfaces = vec!["veth*".into()];
    });
    let out = render_of(&cfg);
    assert!(
        out.contains("[inputs.disk.tagdrop]\n    path = [\"/snap*\"]"),
        "{out}"
    );
    assert!(
        out.contains("[inputs.diskio.tagdrop]\n    name = [\"loop*\"]"),
        "{out}"
    );
    assert!(
        out.contains("[inputs.net.tagdrop]\n    interface = [\"veth*\"]"),
        "{out}"
    );
}

#[test]
fn include_lists_render_as_plugin_options_not_filters() {
    let cfg = config_with(|c| {
        c.modules.network.enabled = true;
        c.modules.network.include_interfaces = vec!["eth0".into()];
    });
    let out = render_of(&cfg);
    assert!(out.contains("interfaces = [\"eth0\"]"));
    assert!(
        !out.contains("tagpass"),
        "an include is a plugin option, not a filter"
    );
}

/// The rule ADR-0007 exists for, checked on real module output rather than on a
/// hand-built instance.
#[test]
fn a_scalar_never_follows_a_subtable_within_a_plugin_block() {
    let cfg = config_with(|c| {
        c.modules.disks.enabled = true;
        c.modules.disks.exclude_filesystems = vec!["tmpfs".into()];
        c.modules.disks.exclude_mountpoints = vec!["/snap*".into()];
    });
    let out = render_of(&cfg);
    let ignore_at = out.find("ignore_fs").expect("ignore_fs missing");
    let drop_at = out.find("[inputs.disk.tagdrop]").expect("tagdrop missing");
    assert!(
        ignore_at < drop_at,
        "ignore_fs would be swallowed by the tagdrop table:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

#[test]
fn the_whole_pipeline_is_deterministic() {
    let t = token_file("tok");
    let cfg = config_with(|c| {
        enable_influx(c, &t);
        c.modules.load.enabled = true;
        c.modules.system.enabled = true;
        c.modules.disks.enabled = true;
        c.modules.disks.exclude_mountpoints = vec!["/snap*".into(), "/var/lib/docker/*".into()];
    });
    assert_eq!(render_of(&cfg), render_of(&cfg));
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

/// `render-config` output must be safe to paste into an issue.
#[test]
fn redacted_rendering_contains_no_secret() {
    let t = token_file("super-secret-token");
    let cfg = config_with(|c| enable_influx(c, &t));

    let redacted = muninn_telegraf::render(&build(&RenderContext::redacted(&cfg)), "0.1.0");
    assert!(
        !redacted.contains("super-secret-token"),
        "the redacted rendering leaked the token:\n{redacted}"
    );
    assert!(redacted.contains("token = \"***\""));

    // ...and the runtime path still emits the real value, or Telegraf could not
    // authenticate.
    let real = render_of(&cfg);
    assert!(real.contains("token = \"super-secret-token\""));
}

// ---------------------------------------------------------------------------
// Requirements
// ---------------------------------------------------------------------------

#[test]
fn requirements_are_collected_only_for_enabled_modules() {
    let cfg = config_with(|c| {
        c.modules.cpu.enabled = true;
        c.modules.disk_io.enabled = true;
    });
    let reqs = crate::requirements_of_enabled(&cfg);
    let ids: Vec<&str> = reqs.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, vec!["cpu", "disk_io"]);
    assert!(
        reqs.iter().any(|(_, r)| r.host_paths.contains(&"sys")),
        "disk_io needs /sys"
    );
}

/// Nothing should be demanded on behalf of a module nobody enabled — that is the
/// whole reason requirements are per-module.
#[test]
fn a_configuration_with_one_module_demands_almost_nothing() {
    let cfg = config_with(|c| c.modules.cpu.enabled = true);
    let reqs = crate::requirements_of_enabled(&cfg);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].1.host_paths, vec!["proc"]);
    assert!(reqs[0].1.absolute_paths.is_empty());
}

// ---------------------------------------------------------------------------
// Docker
// ---------------------------------------------------------------------------

/// Every option the module exposes has to reach the generated file. A filter
/// that silently failed to render would be the worst kind of bug here: the
/// operator believes they excluded a noisy container and the cardinality it
/// costs shows up on the bill instead.
#[test]
fn the_docker_module_renders_every_option_it_offers() {
    let cfg = config_with(|c| {
        c.modules.docker.enabled = true;
        c.modules.docker.endpoint = "tcp://docker-socket-proxy:2375".into();
        c.modules.docker.container_include = vec!["web-*".into()];
        c.modules.docker.container_exclude = vec!["*-build".into()];
        c.modules.docker.container_states = vec!["running".into(), "exited".into()];
        c.modules.docker.timeout = muninn_core::duration::ConfigDuration::from_secs(12);
    });
    let toml = render_of(&cfg);

    assert!(toml.contains("[[inputs.docker]]"), "{toml}");
    assert!(
        toml.contains(r#"endpoint = "tcp://docker-socket-proxy:2375""#),
        "{toml}"
    );
    assert!(toml.contains(r#"timeout = "12s""#), "{toml}");
    assert!(
        toml.contains(r#"container_name_include = ["web-*"]"#),
        "{toml}"
    );
    assert!(
        toml.contains(r#"container_name_exclude = ["*-build"]"#),
        "{toml}"
    );
    assert!(
        toml.contains(r#"container_state_include = ["running", "exited"]"#),
        "state selection must reach the file: {toml}"
    );
}

/// The default is running only, and that is a decision rather than an accident:
/// a stopped container reporting zeros is indistinguishable from an idle one.
#[test]
fn the_docker_module_collects_running_containers_by_default() {
    let cfg = config_with(|c| c.modules.docker.enabled = true);
    assert!(
        render_of(&cfg).contains(r#"container_state_include = ["running"]"#),
        "{}",
        render_of(&cfg)
    );
}

/// Empty filters are omitted rather than rendered empty — an empty
/// `container_name_include` in Telegraf is not "include everything" by accident
/// but by a rule worth not depending on.
#[test]
fn unset_docker_filters_are_omitted() {
    let cfg = config_with(|c| c.modules.docker.enabled = true);
    let toml = render_of(&cfg);
    assert!(!toml.contains("container_name_include"), "{toml}");
    assert!(!toml.contains("container_name_exclude"), "{toml}");
}

/// A unix endpoint is two requirements, not one: the socket file has to be
/// mounted *and* the daemon behind it has to answer. They fail differently and
/// are fixed differently.
#[test]
fn a_unix_docker_endpoint_demands_the_socket_file_and_the_service() {
    let cfg = config_with(|c| {
        c.modules.docker.enabled = true;
        c.modules.docker.endpoint = "unix:///var/run/docker.sock".into();
    });
    let reqs = crate::requirements_of_enabled(&cfg);
    let (_, docker) = reqs.iter().find(|(id, _)| *id == "docker").unwrap();

    assert_eq!(docker.absolute_paths, vec!["/var/run/docker.sock"]);
    assert_eq!(
        docker.endpoints,
        vec![crate::Endpoint {
            kind: crate::EndpointKind::UnixSocket("/var/run/docker.sock".into()),
            timeout: std::time::Duration::from_secs(5),
        }]
    );
}

/// The recommended deployment. A proxy is reached over TCP and has no socket
/// file at all — demanding one would refuse the safest setup, which is the bug
/// this replaced.
#[test]
fn a_proxy_endpoint_demands_no_socket_file() {
    let cfg = config_with(|c| {
        c.modules.docker.enabled = true;
        c.modules.docker.endpoint = "tcp://docker-socket-proxy:2375".into();
    });
    let reqs = crate::requirements_of_enabled(&cfg);
    let (_, docker) = reqs.iter().find(|(id, _)| *id == "docker").unwrap();

    assert!(
        docker.absolute_paths.is_empty(),
        "a proxy deployment has no socket to mount: {:?}",
        docker.absolute_paths
    );
    assert_eq!(
        docker.endpoints[0].kind,
        crate::EndpointKind::Tcp("docker-socket-proxy:2375".into())
    );
}

/// The probe waits as long as the operator said the module may, not a number
/// invented here — otherwise a deployment the running agent tolerates could be
/// refused at startup.
#[test]
fn the_endpoint_carries_the_configured_timeout() {
    let cfg = config_with(|c| {
        c.modules.docker.enabled = true;
        c.modules.docker.timeout = muninn_core::duration::ConfigDuration::from_secs(30);
    });
    let reqs = crate::requirements_of_enabled(&cfg);
    let (_, docker) = reqs.iter().find(|(id, _)| *id == "docker").unwrap();
    assert_eq!(
        docker.endpoints[0].timeout,
        std::time::Duration::from_secs(30)
    );
}

/// A module nobody enabled demands nothing — including no probe, which would
/// otherwise make a disabled module able to fail a startup.
#[test]
fn a_disabled_docker_module_demands_nothing() {
    let cfg = config_with(|c| c.modules.docker.enabled = false);
    let reqs = crate::requirements_of_enabled(&cfg);
    assert!(!reqs.iter().any(|(id, _)| *id == "docker"));
}
