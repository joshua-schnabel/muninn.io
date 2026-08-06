//! `muninn check-runtime` — does the host provide what the enabled modules need?
//!
//! This is the step `telegraf config check` cannot do. That subcommand
//! initialises plugins without starting them, so a missing mount, an occupied
//! port or an unreachable Docker socket is invisible to it. Everything here is
//! about the deployment around the configuration rather than the configuration
//! itself, which is why its failures exit `12` (RUNTIME) and not `10` (CONFIG):
//! the YAML is right, the machine is not.
//!
//! # Every problem, not the first
//!
//! A check that stopped at the first missing mount would make fixing a
//! deployment an exercise in repeated guessing. All findings are collected and
//! reported together.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use muninn_core::Config;
use muninn_modules::{Endpoint, Requirements};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The deployment cannot do what the configuration says.
    Error,
    /// It can, but probably does not mean to.
    Warning,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    /// Which module or setting this is about, for grouping the output.
    pub subject: String,
    pub message: String,
}

impl Finding {
    fn error(subject: impl Into<String>, message: impl Into<String>) -> Self {
        Finding {
            severity: Severity::Error,
            subject: subject.into(),
            message: message.into(),
        }
    }

    fn warning(subject: impl Into<String>, message: impl Into<String>) -> Self {
        Finding {
            severity: Severity::Warning,
            subject: subject.into(),
            message: message.into(),
        }
    }
}

/// Run every check against `config` — what `muninn check-runtime` reports.
pub fn check(config: &Config) -> Vec<Finding> {
    let mut findings = preconditions(config);
    check_listeners(config, &mut findings);
    findings
}

/// The subset `muninn run` can check of itself, at startup.
///
/// Everything except the listener binds. By the time this runs, muninn's health
/// server already holds `health.listen`, so a bind check would report muninn's
/// own listener as an occupied port and refuse to start over it. Telegraf's
/// Prometheus port is not skipped for lack of value but for consistency: it is
/// bound moments later by Telegraf itself, and a failure there is reported with
/// the real error rather than a rehearsal of it.
pub fn preconditions(config: &Config) -> Vec<Finding> {
    let mut findings = Vec::new();

    check_host_mount(config, &mut findings);
    check_modules(config, &mut findings);
    check_runtime_directory(config, &mut findings);

    findings
}

/// Whether any finding is fatal.
pub fn has_errors(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Error)
}

// ---------------------------------------------------------------------------
// The host mount
// ---------------------------------------------------------------------------

fn check_host_mount(config: &Config, findings: &mut Vec<Finding>) {
    let needs_host = config.modules.any_needs_host_mount();

    match &config.runtime.host_mount_prefix {
        Some(prefix) => {
            let path = Path::new(prefix);
            if !path.exists() {
                findings.push(Finding::error(
                    "runtime.host_mount_prefix",
                    format!(
                        "'{prefix}' does not exist. Mount the host filesystem there: \
                         `-v /:{prefix}:ro`"
                    ),
                ));
            } else if !path.is_dir() {
                findings.push(Finding::error(
                    "runtime.host_mount_prefix",
                    format!("'{prefix}' exists but is not a directory"),
                ));
            }
        }
        None if needs_host && in_container() => {
            // The failure this prevents is the one muninn exists to prevent:
            // Telegraf reporting the container's own CPU, memory and disks as
            // the host's — plausible numbers about the wrong machine, with no
            // error anywhere.
            findings.push(Finding::error(
                "runtime.host_mount_prefix",
                "empty, but muninn is running in a container and host modules are enabled. \
                 Telegraf would report the CONTAINER's CPU, memory and disks as if they were \
                 the host's. Set it to /hostfs and mount `-v /:/hostfs:ro`"
                    .to_string(),
            ));
        }
        None => {}
    }

    // The other half of the same promise: an OS hostname inside a container is
    // the container ID, which changes on every recreate and starts a new time
    // series each time. Documented in configuration.md; checked here.
    if config.agent.hostname.is_empty() && !config.agent.omit_hostname && in_container() {
        findings.push(Finding::warning(
            "agent.hostname",
            "empty inside a container, so Telegraf will use the container ID. That changes on \
             every recreate, so every deploy starts a new time series and dashboards lose their \
             history. Set agent.hostname, or give the container the host's name"
                .to_string(),
        ));
    }
}

/// Whether this process is running in a container.
///
/// Two signals, because neither is universal: Docker writes `/.dockerenv`, and
/// most runtimes leave their name in PID 1's cgroup path. A false negative here
/// only costs a missed warning, so both are best-effort rather than exhaustive.
fn in_container() -> bool {
    if Path::new("/.dockerenv").exists() {
        return true;
    }
    std::fs::read_to_string("/proc/1/cgroup")
        .map(|s| s.contains("docker") || s.contains("containerd") || s.contains("kubepods"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Modules
// ---------------------------------------------------------------------------

fn check_modules(config: &Config, findings: &mut Vec<Finding>) {
    let prefix = config.runtime.host_mount_prefix.as_deref().unwrap_or("");

    for (module, req) in muninn_modules::requirements_of_enabled(config) {
        check_host_paths(module, &req, prefix, findings);

        for path in &req.absolute_paths {
            let p = Path::new(path);
            if !p.exists() {
                findings.push(Finding::error(
                    module,
                    format!(
                        "'{path}' does not exist. The {module} module needs it; mount it into \
                         the container, or disable the module"
                    ),
                ));
            }
        }

        for endpoint in &req.endpoints {
            check_endpoint(module, endpoint, findings);
        }

        if req.debian_family_only {
            check_debian_family(module, prefix, findings);
        }
    }
}

/// A service a module has to talk to must actually answer.
///
/// This is the check that makes enabling a module a decision with a verdict.
/// Without it, an unreachable Docker endpoint produces a Telegraf that starts,
/// stays up, and reports no containers — which is also what a host with no
/// containers looks like. An operator watching a dashboard cannot tell the two
/// apart, so the difference has to be settled before start, not after.
fn check_endpoint(module: &'static str, endpoint: &Endpoint, findings: &mut Vec<Finding>) {
    if let Err(reason) = crate::probe::docker(endpoint) {
        findings.push(Finding::error(
            module,
            format!(
                "{reason}. The {module} module is enabled, so muninn will not start with an \
                 endpoint it cannot reach — an unreachable one looks exactly like a host with \
                 nothing to report"
            ),
        ));
    }
}

fn check_host_paths(
    module: &'static str,
    req: &Requirements,
    prefix: &str,
    findings: &mut Vec<Finding>,
) {
    for name in &req.host_paths {
        let path = PathBuf::from(format!("{prefix}/{name}"));
        if !path.exists() {
            findings.push(Finding::error(
                module,
                format!(
                    "'{}' is missing. The {module} module reads it; without it Telegraf would \
                     report the container's own state instead of the host's",
                    path.display()
                ),
            ));
        } else if std::fs::read_dir(&path).is_err() && std::fs::metadata(&path).is_err() {
            findings.push(Finding::error(
                module,
                format!("'{}' exists but is not readable", path.display()),
            ));
        }
    }
}

/// Confirm the host is Debian-family.
///
/// The reading itself lives in `muninn_modules::updates::debian` so that this
/// check and the module's own preconditions cannot drift apart. They ask the same
/// question about the same files, and a startup check that disagreed with the
/// running module would be worse than no check at all.
fn check_debian_family(module: &'static str, prefix: &str, findings: &mut Vec<Finding>) {
    let root = PathBuf::from(if prefix.is_empty() { "/" } else { prefix });

    let Some(ids) = muninn_modules::updates::debian::os_release_ids(&root) else {
        let locations = muninn_modules::updates::debian::OS_RELEASE_LOCATIONS;
        findings.push(Finding::error(
            module,
            format!(
                "cannot read os-release at '{}/{}' or '{}/{}'. Note /etc/os-release is normally a \
                 symlink into /usr/lib, so a mount carrying /etc but not /usr leaves it dangling",
                root.display(),
                locations[0],
                root.display(),
                locations[1]
            ),
        ));
        return;
    };

    if !ids.is_debian_family() {
        findings.push(Finding::error(
            module,
            format!(
                "the host reports ID={:?}, which is not Debian-family. The {module} module \
                 only supports Debian and Ubuntu; disable it",
                ids.id
            ),
        ));
    }
}

// ---------------------------------------------------------------------------
// Listeners
// ---------------------------------------------------------------------------

fn check_listeners(config: &Config, findings: &mut Vec<Finding>) {
    check_bindable(config.health.listen, "health.listen", findings);

    if let Some(prom) = &config.outputs.prometheus {
        check_bindable(prom.listen, "outputs.prometheus.listen", findings);
    }
}

/// Can this address actually be bound?
///
/// The configuration layer already rejects a collision *between* muninn's own
/// two listeners. What it cannot know is whether something else on the host has
/// the port — which is only answerable by trying, and is exactly the class of
/// problem `config check` cannot see.
fn check_bindable(addr: SocketAddr, key: &str, findings: &mut Vec<Finding>) {
    match std::net::TcpListener::bind(addr) {
        Ok(listener) => drop(listener),
        Err(e) => findings.push(Finding::error(
            key,
            format!(
                "cannot bind {addr}: {e}. Something else is using the port, or the address does \
                 not exist on this machine"
            ),
        )),
    }
}

// ---------------------------------------------------------------------------
// The runtime directory
// ---------------------------------------------------------------------------

fn check_runtime_directory(config: &Config, findings: &mut Vec<Finding>) {
    let path = Path::new(&config.runtime.generated_config_path);
    let Some(dir) = path.parent() else {
        findings.push(Finding::error(
            "runtime.generated_config_path",
            format!("'{}' has no parent directory", path.display()),
        ));
        return;
    };

    // Creating it is what muninn does at startup anyway, so trying here reports
    // the same failure earlier and without leaving anything behind.
    if let Err(e) = std::fs::create_dir_all(dir) {
        findings.push(Finding::error(
            "runtime.generated_config_path",
            format!(
                "cannot create '{}': {e}. In a container this needs a writable tmpfs: \
                 `--tmpfs {}:mode=0700`",
                dir.display(),
                dir.display()
            ),
        ));
        return;
    }

    let probe = dir.join(".muninn-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
        }
        Err(e) => findings.push(Finding::error(
            "runtime.generated_config_path",
            format!("'{}' is not writable: {e}", dir.display()),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muninn_core::config::{Overrides, loader};

    /// Build a resolved configuration from YAML, so a test cannot construct one
    /// the validator would have rejected.
    fn config(yaml: &str) -> Config {
        let (v1, _) = loader::from_str(yaml, &Overrides::default()).expect("fixture must load");
        Config::from_v1(v1).expect("fixture must resolve")
    }

    /// A directory that looks enough like a mounted host root for the checks to
    /// be satisfiable.
    ///
    /// Built rather than assumed: `/proc` does not exist on Windows, and a test
    /// that took it for granted would assert about the development machine
    /// instead of about the code. Building it also exercises the prefix
    /// resolution, which asserting against a real `/proc` would not.
    fn fake_host() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["proc", "sys", "var", "etc", "usr/lib", "run"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        std::fs::write(
            dir.path().join("usr/lib/os-release"),
            "ID=debian\nVERSION_ID=\"12\"\n",
        )
        .unwrap();
        dir
    }

    fn slashed(p: &Path) -> String {
        p.display().to_string().replace('\\', "/")
    }

    /// A path whose parent cannot be created, on any platform.
    ///
    /// Not `/nonexistent-root/...`: on Windows that resolves against the system
    /// drive and gets created quite happily — the first version of this test
    /// silently made a directory on the developer's C:. Nesting under a regular
    /// *file* fails everywhere, because a path component is not a directory.
    fn unwritable_path(anchor: &tempfile::TempDir) -> String {
        let blocker = anchor.path().join("not-a-directory");
        std::fs::write(&blocker, b"").unwrap();
        slashed(&blocker.join("muninn/telegraf.conf"))
    }

    fn base(extra: &str) -> String {
        format!(
            r#"version: 1
runtime:
  generated_config_path: "{}/telegraf.conf"
  host_mount_prefix: ""
health:
  listen: "127.0.0.1:0"
modules:
  cpu:
    enabled: true
outputs:
  prometheus:
    enabled: true
    listen: "127.0.0.1:0"
{extra}"#,
            std::env::temp_dir()
                .display()
                .to_string()
                .replace('\\', "/")
        )
    }

    fn subjects(findings: &[Finding], severity: Severity) -> Vec<&str> {
        findings
            .iter()
            .filter(|f| f.severity == severity)
            .map(|f| f.subject.as_str())
            .collect()
    }

    /// Port 0 always binds, the temp directory is writable, and the host paths
    /// are present under a prefix the test builds.
    #[test]
    fn a_workable_deployment_produces_no_errors() {
        let host = fake_host();
        let mut cfg = config(&base(""));
        cfg.runtime.host_mount_prefix = Some(slashed(host.path()));

        let findings = check(&cfg);
        assert!(
            !has_errors(&findings),
            "expected no errors, got: {findings:#?}"
        );
    }

    /// The complement: a prefix that exists but is missing what a module reads.
    /// This is the realistic half-configured mount, and its symptom without the
    /// check is metrics about the container rather than the host.
    #[test]
    fn a_prefix_without_the_paths_a_module_reads_is_an_error() {
        let host = tempfile::tempdir().unwrap();
        let mut cfg = config(&base(""));
        cfg.runtime.host_mount_prefix = Some(slashed(host.path()));

        let findings = check(&cfg);
        assert!(has_errors(&findings), "{findings:#?}");
        assert!(
            subjects(&findings, Severity::Error).contains(&"cpu"),
            "the finding should name the module: {findings:#?}"
        );
        assert!(
            findings.iter().any(|f| f.message.contains("proc")),
            "and the path: {findings:#?}"
        );
    }

    /// Found the hard way in the WP1 spike: /etc/os-release is a symlink into
    /// /usr/lib, so a mount carrying /etc but not /usr leaves it dangling.
    #[test]
    fn the_updates_module_reads_os_release_from_either_location() {
        let host = fake_host();
        let mut cfg = config(&base(""));
        cfg.runtime.host_mount_prefix = Some(slashed(host.path()));
        cfg.modules.updates.enabled = true;

        // Only /usr/lib/os-release exists in the fixture, which is the case a
        // naive check that read /etc/os-release alone would fail.
        let findings = check(&cfg);
        assert!(
            !subjects(&findings, Severity::Error).contains(&"updates"),
            "should accept os-release from /usr/lib: {findings:#?}"
        );

        // ...and a host that is not Debian-family must be refused.
        std::fs::write(
            host.path().join("usr/lib/os-release"),
            "ID=alpine
",
        )
        .unwrap();
        let findings = check(&cfg);
        assert!(
            subjects(&findings, Severity::Error).contains(&"updates"),
            "a non-Debian host must be refused: {findings:#?}"
        );
    }

    /// A prefix that is not mounted is the most common deployment mistake, and
    /// the one with the most misleading symptom.
    #[test]
    fn a_missing_host_mount_prefix_is_an_error() {
        let cfg = config(&base("\n# override\n"));
        let mut cfg = cfg;
        cfg.runtime.host_mount_prefix = Some("/nonexistent-hostfs".into());
        let findings = check(&cfg);
        assert!(has_errors(&findings));
        let prefix_finding = findings
            .iter()
            .find(|f| f.subject == "runtime.host_mount_prefix")
            .unwrap_or_else(|| panic!("expected a prefix finding: {findings:#?}"));
        assert!(
            prefix_finding.message.contains("-v /:"),
            "should say how to fix it: {}",
            prefix_finding.message
        );
    }

    /// Every problem, not the first — otherwise fixing a deployment is repeated
    /// guessing.
    #[test]
    fn several_problems_are_reported_together() {
        let anchor = tempfile::tempdir().unwrap();
        let mut cfg = config(&base(""));
        cfg.runtime.host_mount_prefix = Some("/nonexistent-hostfs".into());
        cfg.runtime.generated_config_path = unwritable_path(&anchor);

        let findings = check(&cfg);
        let errors = subjects(&findings, Severity::Error);
        assert!(
            errors.len() >= 2,
            "expected several findings: {findings:#?}"
        );
        assert!(errors.contains(&"runtime.host_mount_prefix"), "{errors:?}");
        assert!(
            errors.contains(&"runtime.generated_config_path"),
            "{errors:?}"
        );
    }

    /// The check the configuration layer cannot do: something *else* holds the
    /// port. `telegraf config check` cannot see this either — it initialises
    /// plugins without starting them.
    #[test]
    fn an_occupied_port_is_detected() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = held.local_addr().unwrap();

        let host = fake_host();
        let mut cfg = config(&base(""));
        cfg.runtime.host_mount_prefix = Some(slashed(host.path()));
        cfg.health.listen = addr;

        let findings = check(&cfg);
        assert!(has_errors(&findings), "{findings:#?}");
        assert!(
            subjects(&findings, Severity::Error).contains(&"health.listen"),
            "{findings:#?}"
        );
    }

    #[test]
    fn an_unwritable_runtime_directory_is_detected() {
        let host = fake_host();
        let anchor = tempfile::tempdir().unwrap();
        let mut cfg = config(&base(""));
        cfg.runtime.host_mount_prefix = Some(slashed(host.path()));
        cfg.runtime.generated_config_path = unwritable_path(&anchor);
        let findings = check(&cfg);
        assert!(
            subjects(&findings, Severity::Error).contains(&"runtime.generated_config_path"),
            "{findings:#?}"
        );
        assert!(
            findings.iter().any(|f| f.message.contains("tmpfs")),
            "should say what a container needs: {findings:#?}"
        );
    }

    /// A module that needs something absent must name itself, so the operator
    /// knows which one to disable.
    #[test]
    fn a_module_requirement_names_the_module() {
        let mut cfg = config(&base(""));
        cfg.modules.docker.enabled = true;
        cfg.modules.docker.endpoint = "unix:///nonexistent/docker.sock".into();

        let findings = check(&cfg);
        // The Docker module declares the conventional socket path, which is
        // absent on a machine without Docker — and present on one with it, so
        // this asserts on the shape rather than the outcome.
        let docker: Vec<_> = findings.iter().filter(|f| f.subject == "docker").collect();
        for f in &docker {
            assert!(
                f.message.contains("docker"),
                "a finding should name what is missing: {f:?}"
            );
        }
    }

    /// The check WP9 exists for. An unreachable endpoint has to be a refusal,
    /// because the alternative — a Telegraf that starts and reports no
    /// containers — is indistinguishable from a host that runs none.
    ///
    /// Over TCP rather than a unix socket so the test asserts the same thing on
    /// every platform; the unix path is covered by the container test against a
    /// real Docker socket.
    #[test]
    fn an_unreachable_docker_endpoint_is_an_error() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);

        let host = fake_host();
        let mut cfg = config(&base(""));
        cfg.runtime.host_mount_prefix = Some(slashed(host.path()));
        cfg.modules.docker.enabled = true;
        cfg.modules.docker.endpoint = format!("tcp://127.0.0.1:{port}");

        let findings = check(&cfg);
        assert!(
            subjects(&findings, Severity::Error).contains(&"docker"),
            "an unreachable endpoint must refuse the start: {findings:#?}"
        );
        let f = findings.iter().find(|f| f.subject == "docker").unwrap();
        assert!(
            f.message.contains("nothing to report"),
            "should say why silence is not an acceptable answer: {}",
            f.message
        );
    }

    /// ...and the same endpoint is nobody's business while the module is off.
    #[test]
    fn a_disabled_docker_module_is_not_probed() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);

        let host = fake_host();
        let mut cfg = config(&base(""));
        cfg.runtime.host_mount_prefix = Some(slashed(host.path()));
        cfg.modules.docker.enabled = false;
        cfg.modules.docker.endpoint = format!("tcp://127.0.0.1:{port}");

        assert!(
            !subjects(&check(&cfg), Severity::Error).contains(&"docker"),
            "a module nobody enabled must not be able to fail a startup"
        );
    }

    /// `run` cannot use the full check: by the time it gets there muninn's own
    /// health server holds `health.listen`, and a bind test would report
    /// muninn's own listener as an occupied port and refuse to start over it.
    #[test]
    fn preconditions_do_not_include_the_listener_binds() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = held.local_addr().unwrap();

        let host = fake_host();
        let mut cfg = config(&base(""));
        cfg.runtime.host_mount_prefix = Some(slashed(host.path()));
        cfg.health.listen = addr;

        assert!(
            !has_errors(&preconditions(&cfg)),
            "startup must not fail on the port it is about to serve on"
        );
        assert!(
            has_errors(&check(&cfg)),
            "but check-runtime still reports it: that is its job"
        );
    }

    /// Container detection is best-effort, so it must not panic or block
    /// wherever it runs — including on Windows, where neither signal exists.
    #[test]
    fn container_detection_is_answerable_anywhere() {
        let _ = in_container();
    }

    #[test]
    fn findings_carry_a_subject_so_output_can_be_grouped() {
        let mut cfg = config(&base(""));
        cfg.runtime.host_mount_prefix = Some("/nonexistent-hostfs".into());
        for f in check(&cfg) {
            assert!(!f.subject.is_empty(), "{f:?}");
            assert!(!f.message.is_empty(), "{f:?}");
        }
    }
}
