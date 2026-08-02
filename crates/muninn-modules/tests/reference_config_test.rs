//! The whole pipeline, checked against a Telegraf-verified target.
//!
//! `docs/reference/telegraf.reference.conf` is what muninn renders for
//! `config/muninn.example.yaml`, and it is checked into the repository *because*
//! it is verified independently: `scripts/verify-design-package.sh` runs
//! `telegraf config check` against it with the pinned Telegraf version.
//!
//! So this test is not circular. The reference is proof that the renderer's
//! output is a configuration real Telegraf accepts; this test is proof that the
//! renderer still produces it.
//!
//! When the renderer legitimately changes, regenerate with
//!
//! ```text
//! muninn --config <example> render-config > docs/reference/telegraf.reference.conf
//! ```
//!
//! and re-run the verification script. Updating the reference without
//! re-verifying it would turn this into a test that agrees with whatever the
//! code happens to do.

use std::io::Write;
use std::path::{Path, PathBuf};

use muninn_core::Config;
use muninn_core::config::{Overrides, loader};
use muninn_modules::{RenderContext, build};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root should resolve")
}

/// Load the shipped example, pointing the token at a real temporary file.
///
/// The shipped path is `/run/secrets/influxdb_token`, which exists in the
/// container and not on a developer's machine. Substituting keeps the test about
/// the rendering rather than about the filesystem.
fn example_config() -> (Config, tempfile::NamedTempFile) {
    let raw = std::fs::read_to_string(repo_root().join("config/muninn.example.yaml"))
        .expect("the shipped example must exist");

    let mut token = tempfile::NamedTempFile::new().unwrap();
    write!(token, "example-token").unwrap();
    token.flush().unwrap();
    let token_path = token.path().display().to_string().replace('\\', "/");

    let patched = raw.replace("/run/secrets/influxdb_token", &token_path);
    let (v1, _) = loader::from_str(&patched, &Overrides::default())
        .expect("the shipped example must validate");
    (
        Config::from_v1(v1).expect("the shipped example must resolve"),
        token,
    )
}

/// The milestone this work package exists for.
#[test]
fn rendering_the_shipped_example_matches_the_verified_reference() {
    let (cfg, _token) = example_config();
    let rendered = muninn_telegraf::render(&build(&RenderContext::redacted(&cfg)), "0.1.0");

    let reference =
        std::fs::read_to_string(repo_root().join("docs/reference/telegraf.reference.conf"))
            .expect("the reference must exist")
            // The repository normalises to LF; a Windows checkout may hand back CRLF.
            .replace("\r\n", "\n");

    if rendered != reference {
        // A diff rather than a wall of two files: the useful information is
        // which lines moved.
        let mut report = String::from("rendered output differs from the verified reference:\n");
        for (i, (a, b)) in reference.lines().zip(rendered.lines()).enumerate() {
            if a != b {
                report.push_str(&format!(
                    "  line {}:\n    reference: {a}\n    rendered:  {b}\n",
                    i + 1
                ));
            }
        }
        let (rl, ol) = (reference.lines().count(), rendered.lines().count());
        if rl != ol {
            report.push_str(&format!("  line count: reference {rl}, rendered {ol}\n"));
        }
        panic!("{report}");
    }
}

/// The reference is committed with secrets redacted, so it is safe to read, and
/// the runtime path must still produce the real value.
#[test]
fn the_reference_is_redacted_but_the_runtime_rendering_is_not() {
    let (cfg, _token) = example_config();

    let redacted = muninn_telegraf::render(&build(&RenderContext::redacted(&cfg)), "0.1.0");
    assert!(redacted.contains("token = \"***\""));
    assert!(!redacted.contains("example-token"));

    let real = muninn_telegraf::render(&build(&RenderContext::new(&cfg)), "0.1.0");
    assert!(
        real.contains("token = \"example-token\""),
        "Telegraf could not authenticate with a redacted token"
    );
}

/// Every module the example enables must appear in the output. A module that
/// silently rendered nothing would be the kind of failure muninn exists to
/// prevent — a configuration that says it collects something and does not.
#[test]
fn every_enabled_module_reaches_the_output() {
    let (cfg, _token) = example_config();
    let rendered = muninn_telegraf::render(&build(&RenderContext::redacted(&cfg)), "0.1.0");

    let expected_plugins = [
        ("cpu", "[[inputs.cpu]]"),
        ("memory", "[[inputs.mem]]"),
        ("load", "[[inputs.system]]"),
        ("system", "[[inputs.system]]"),
        ("swap", "[[inputs.swap]]"),
        ("processes", "[[inputs.processes]]"),
        ("disks", "[[inputs.disk]]"),
        ("disk_io", "[[inputs.diskio]]"),
        ("network", "[[inputs.net]]"),
    ];
    for (module, header) in expected_plugins {
        assert!(
            cfg.modules.enabled_names().contains(&module),
            "the example should enable {module}"
        );
        assert!(
            rendered.contains(header),
            "{module} is enabled but {header} is missing from the output"
        );
    }

    // ...and the two that the example deliberately leaves off.
    assert!(
        !rendered.contains("[[inputs.docker]]"),
        "docker is off by default"
    );
    assert!(
        !rendered.contains("[[inputs.exec]]"),
        "updates is off by default"
    );
}

/// The generated file is what an operator reads while debugging, so every block
/// should say which module put it there.
#[test]
fn every_block_carries_its_provenance() {
    let (cfg, _token) = example_config();
    let rendered = muninn_telegraf::render(&build(&RenderContext::redacted(&cfg)), "0.1.0");

    let mut previous = "";
    for line in rendered.lines() {
        if line.starts_with("[[") {
            assert!(
                previous.starts_with("# module") || previous.starts_with("# output"),
                "{line} has no provenance comment above it (found {previous:?})"
            );
        }
        previous = line;
    }
}
