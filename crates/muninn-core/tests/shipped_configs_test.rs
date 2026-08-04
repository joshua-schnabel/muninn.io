//! The configurations muninn ships must pass muninn's own validation.
//!
//! Without this, `config/muninn.example.yaml` drifts away from the schema the
//! moment a key is renamed — and the file people copy is the one that no longer
//! loads. It has happened to every project that documents its configuration by
//! hand.
//!
//! These live in `tests/` rather than inline because they are about the repo's
//! shipped artefacts, not about the crate's own code.

use std::io::Write;
use std::path::{Path, PathBuf};

use muninn_core::config::{self, Overrides};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/muninn-core.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root should resolve")
}

/// Load a shipped config, redirecting any secret paths at real temporary files.
///
/// The shipped files reference `/run/secrets/...`, which exists in the container
/// and not on a developer's machine. Substituting a real file keeps the test
/// about the *schema* rather than about the filesystem — every other rule still
/// runs unchanged.
fn load_shipped(name: &str) -> (String, Vec<String>) {
    let path = repo_root().join("config").join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

    let mut token = tempfile::NamedTempFile::new().unwrap();
    write!(token, "test-token").unwrap();
    token.flush().unwrap();
    let token_path = token.path().display().to_string().replace('\\', "/");

    let patched = raw.replace("/run/secrets/influxdb_token", &token_path);

    match config::loader::from_str(&patched, &Overrides::default()) {
        Ok((_, warnings)) => (name.to_string(), warnings),
        Err(e) => panic!(
            "{name} does not pass muninn's own validation: {e}\n\
             The shipped configuration and the schema have drifted apart."
        ),
    }
}

#[test]
fn the_annotated_example_validates() {
    let (_, warnings) = load_shipped("muninn.example.yaml");
    // The example enables the Docker socket warning? It must not — docker is off
    // there. Any warning at all in the file we tell people to copy is a defect:
    // it would train them to ignore startup warnings.
    assert!(
        warnings.is_empty(),
        "the file people copy should produce no warnings, got: {warnings:#?}"
    );
}

#[test]
fn the_minimal_example_validates() {
    let (_, warnings) = load_shipped("muninn.minimal.yaml");
    assert!(warnings.is_empty(), "got: {warnings:#?}");
}

/// The integration config is allowed exactly one warning, and it is a correct
/// one: the CI stack talks to InfluxDB over plaintext HTTP on a private compose
/// network, which muninn rightly flags and which is fine there.
///
/// Listing the exception rather than relaxing the assertion is the point. If a
/// *second* warning appears, this fails and someone has to decide whether the
/// config or the rule is wrong.
#[test]
fn the_integration_config_validates_with_one_expected_warning() {
    let (_, warnings) = load_shipped("muninn.integration.yaml");
    let unexpected: Vec<_> = warnings
        .iter()
        .filter(|w| !w.contains("plaintext HTTP"))
        .collect();
    assert!(
        unexpected.is_empty(),
        "only the plaintext-HTTP warning is expected here, also got: {unexpected:#?}"
    );
    assert_eq!(
        warnings.len(),
        1,
        "the plaintext-HTTP warning should still be produced: {warnings:#?}"
    );
}

/// The annotated example is the one people copy, so it should exercise the
/// schema rather than a corner of it. If a module is added and never appears
/// here, nobody discovers it.
#[test]
fn the_annotated_example_covers_every_module_and_output() {
    let path = repo_root().join("config/muninn.example.yaml");
    let raw = std::fs::read_to_string(path).unwrap();
    for key in [
        "cpu:",
        "memory:",
        "load:",
        "system:",
        "swap:",
        "processes:",
        "disks:",
        "disk_io:",
        "network:",
        "docker:",
        "updates:",
        "image_updates:",
        "influxdb:",
        "prometheus:",
    ] {
        assert!(
            raw.contains(key),
            "muninn.example.yaml should document `{key}` — it is the file people copy"
        );
    }
}

/// The minimal example is the counterweight: it must stay minimal, or it stops
/// answering the question it exists for ("what is the least I have to write?").
#[test]
fn the_minimal_example_stays_minimal() {
    let path = repo_root().join("config/muninn.minimal.yaml");
    let raw = std::fs::read_to_string(path).unwrap();
    let significant = raw
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .count();
    assert!(
        significant <= 12,
        "muninn.minimal.yaml has grown to {significant} significant lines; \
         it exists to show the smallest configuration that starts"
    );
}

/// Every YAML in `config/`, not just the two named above.
///
/// Enumerated from the directory rather than listed here, so a file added later
/// is covered the day it is added. A shipped configuration that does not load is
/// worse than no example at all: it teaches the wrong schema and fails in the
/// reader's deployment rather than in CI.
#[test]
fn every_shipped_configuration_validates() {
    let dir = repo_root().join("config");
    let mut checked = Vec::new();

    for entry in std::fs::read_dir(&dir).expect("config/ should exist") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "yaml") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // Panics with a useful message if the file has drifted.
        let (_, _) = load_shipped(&name);
        checked.push(name);
    }

    assert!(
        checked.len() >= 3,
        "expected the shipped examples to be found, got: {checked:?}"
    );
}

/// The proxy deployment ships as a pair — a compose file and the configuration
/// it mounts. If they disagree on the endpoint, following the documentation
/// produces a container that exits 12, and the reader has no way to know which
/// of the two files is wrong.
#[test]
fn the_docker_module_example_matches_the_compose_file_it_belongs_to() {
    let yaml = std::fs::read_to_string(repo_root().join("config/muninn.docker-module.yaml"))
        .expect("the docker-module example should ship");
    let compose = std::fs::read_to_string(repo_root().join("docker-compose.docker-module.yml"))
        .expect("the docker-module compose file should ship");

    assert!(
        yaml.contains("tcp://docker-socket-proxy:2375"),
        "the example should point at the proxy, not at the socket"
    );
    assert!(
        compose.contains("docker-socket-proxy:"),
        "and the compose file should define a service by that name"
    );
    assert!(
        !yaml.contains("unix:///var/run/docker.sock"),
        "the proxy example must not also mount the socket — that would grant \
         exactly what the proxy exists to avoid"
    );
    assert!(
        compose.contains("PING: 1"),
        "the proxy must allow /_ping, or muninn's own reachability check fails \
         against the deployment this file recommends"
    );
    assert!(
        compose.contains("POST: 0"),
        "POST: 0 is what makes the proxy a boundary rather than a suggestion"
    );
}
