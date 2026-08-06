//! Failure paths for the two things muninn handles that can hurt someone:
//! credentials, and the host filesystem.
//!
//! The happy paths are covered elsewhere — `lifecycle_test.rs` runs the agent
//! end to end, and the unit tests in `muninn-core` cover secret loading as a
//! function. What is here is what happens when it goes **wrong**, observed on
//! the compiled binary, because that is where an operator meets it.
//!
//! Two properties are asserted rather than described:
//!
//! - a secret's *value* never reaches an operator's terminal, whatever the
//!   command and whatever went wrong. Every command is run against a token with
//!   a distinctive value and every byte of output is searched for it.
//! - a missing or wrong host mount is refused with the path named. The failure
//!   this prevents is Telegraf reporting the container's own CPU and disks as
//!   the host's: plausible numbers about the wrong machine, with no error
//!   anywhere.
//!
//! None of these need Telegraf. Secrets are read at startup step 4 and the
//! runtime preconditions at step 5, both before anything is spawned.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use muninn_core::exit;

/// Distinctive enough that finding it in any output is unambiguous, and not a
/// substring of anything muninn prints by itself.
const TOKEN: &str = "zzz-influx-token-must-never-be-printed-zzz";

struct Fixture {
    dir: tempfile::TempDir,
    config: PathBuf,
}

impl Fixture {
    fn path(&self) -> &Path {
        &self.config
    }
    fn dir(&self) -> &Path {
        self.dir.path()
    }
}

/// A configuration with an InfluxDB output, so a token file is required.
///
/// `token_file` and `host_mount_prefix` are what each test varies; everything
/// else is the smallest configuration that validates.
fn fixture(token_file: &str, host_mount_prefix: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("muninn.yaml");
    let generated = dir.path().join("telegraf.conf");

    let mut f = std::fs::File::create(&config).unwrap();
    write!(
        f,
        r#"
version: 1
agent:
  interval: 10s
  flush_interval: 10s
  hostname: "secrets-test"
runtime:
  shutdown_grace_period: 10s
  telegraf_start_timeout: 15s
  generated_config_path: "{generated}"
  host_mount_prefix: "{host_mount_prefix}"
logging:
  format: json
  level: info
health:
  listen: "127.0.0.1:{health_port}"
modules:
  cpu:
    enabled: true
outputs:
  influxdb:
    enabled: true
    url: "https://influxdb.example.internal:8086"
    organization: "testorg"
    bucket: "testbucket"
    token_file: "{token_file}"
    timeout: 5s
"#,
        generated = slash(&generated),
        health_port = free_port(),
    )
    .unwrap();

    Fixture { dir, config }
}

/// Windows paths go into YAML with forward slashes: a backslash is an escape in
/// a double-quoted YAML scalar, and `C:\Users` would fail to parse.
fn slash(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn muninn(config: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_muninn"));
    // A stale RUST_LOG in the developer's environment would change what the
    // process prints and make these assertions depend on the shell.
    c.env_remove("RUST_LOG");
    c.arg("--config").arg(config);
    c.args(args);
    c.output().expect("muninn should be runnable")
}

fn code(out: &Output) -> u8 {
    u8::try_from(out.status.code().expect("terminated by a signal")).expect("exit code fits in u8")
}

fn all_output(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A token file with a value no other test writes.
fn token_file(dir: &Path, contents: &str) -> PathBuf {
    let path = dir.join("influxdb-token");
    std::fs::write(&path, contents).unwrap();
    path
}

// ── Secrets: what happens when the file is wrong ─────────────────────────────

/// The commonest deployment mistake: the secret is mounted at a different path
/// than the configuration names, or not mounted at all. It must name the path —
/// and only the path, since the contents are the thing being protected.
#[test]
fn a_missing_token_file_exits_11_and_names_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("not-mounted").join("influxdb-token");
    let f = fixture(&slash(&missing), "");

    let out = muninn(f.path(), &["validate"]);

    assert_eq!(code(&out), exit::SECRET, "output: {}", all_output(&out));
    assert!(
        all_output(&out).contains("influxdb-token"),
        "the error must name the path it could not read: {}",
        all_output(&out)
    );
}

/// A bind mount whose source does not exist gets created by Docker as a
/// directory, so "the secret is a directory" is not a hypothetical — it is what
/// a typo in a compose file produces.
#[test]
fn a_token_file_that_is_a_directory_exits_11() {
    let f = fixture("", "");
    let as_dir = f.dir().join("influxdb-token");
    std::fs::create_dir(&as_dir).unwrap();
    let f = fixture(&slash(&as_dir), "");

    let out = muninn(f.path(), &["validate"]);

    assert_eq!(code(&out), exit::SECRET, "output: {}", all_output(&out));
}

/// An empty file is the shape a secret takes when the process that should have
/// written it failed. Reading it as an empty token would authenticate as nobody
/// and fail later, somewhere less obvious.
#[test]
fn an_empty_token_file_exits_11() {
    let dir = tempfile::tempdir().unwrap();
    let empty = token_file(dir.path(), "");
    let f = fixture(&slash(&empty), "");

    let out = muninn(f.path(), &["validate"]);

    assert_eq!(code(&out), exit::SECRET, "output: {}", all_output(&out));
}

/// A whitespace-only file is empty for this purpose. It is what an editor that
/// added a trailing newline to a file someone forgot to fill in looks like.
#[test]
fn a_whitespace_only_token_file_exits_11() {
    let dir = tempfile::tempdir().unwrap();
    let blank = token_file(dir.path(), "   \n\t\n");
    let f = fixture(&slash(&blank), "");

    let out = muninn(f.path(), &["validate"]);

    assert_eq!(code(&out), exit::SECRET, "output: {}", all_output(&out));
}

// ── Secrets: what happens when the file is right ─────────────────────────────

/// The property the `Secret` type exists for, checked against the artefact
/// rather than the type: no command prints the value, in success or failure.
///
/// `render-config` is the interesting one — it is the command whose whole job is
/// to print the configuration the token goes into.
#[test]
fn no_command_prints_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let token = token_file(dir.path(), TOKEN);
    let f = fixture(&slash(&token), "");

    for args in [
        vec!["validate"],
        vec!["render-config"],
        vec!["check-runtime"],
    ] {
        let out = muninn(f.path(), &args);
        let text = all_output(&out);
        assert!(
            !text.contains(TOKEN),
            "`muninn {}` printed the token:\n{text}",
            args.join(" ")
        );
    }
}

/// Redaction is the default and the flag is the exception, not the other way
/// round. Both halves matter: a `***` that hides a token nobody asked to see,
/// and a `--unsafe-show-secrets` that actually produces a usable configuration —
/// otherwise the flag is a trap that emits something Telegraf will reject.
#[test]
fn render_config_redacts_unless_the_flag_says_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let token = token_file(dir.path(), TOKEN);
    let f = fixture(&slash(&token), "");

    let redacted = muninn(f.path(), &["render-config"]);
    let text = String::from_utf8_lossy(&redacted.stdout).to_string();
    assert_eq!(code(&redacted), exit::OK, "{}", all_output(&redacted));
    assert!(!text.contains(TOKEN), "the default must not show the token");
    assert!(
        text.contains("***"),
        "the redacted output should show the marker: {text}"
    );

    let shown = muninn(f.path(), &["render-config", "--unsafe-show-secrets"]);
    assert_eq!(code(&shown), exit::OK, "{}", all_output(&shown));
    assert!(
        String::from_utf8_lossy(&shown.stdout).contains(TOKEN),
        "the flag must produce a configuration that actually works"
    );
    assert!(
        String::from_utf8_lossy(&shown.stderr).contains("unsafe"),
        "and must warn on stderr that the output holds credentials"
    );
}

/// `render-config --output` writes wherever the operator points it, and with
/// `--unsafe-show-secrets` that file holds a real credential. It gets the same
/// permissions as the configuration the supervisor writes, because it is the
/// same file with the same contents — the only difference is who asked for it.
#[cfg(unix)]
#[test]
fn a_rendered_file_is_not_readable_by_others() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let token = token_file(dir.path(), TOKEN);
    let f = fixture(&slash(&token), "");
    let out_path = dir.path().join("telegraf.conf");

    let out = muninn(
        f.path(),
        &[
            "render-config",
            "--unsafe-show-secrets",
            "--output",
            out_path.to_str().unwrap(),
        ],
    );
    assert_eq!(code(&out), exit::OK, "{}", all_output(&out));

    let mode = std::fs::metadata(&out_path).unwrap().permissions().mode();
    assert_eq!(
        mode & 0o077,
        0,
        "mode {mode:o} lets someone else read a file containing a token"
    );
    assert!(std::fs::read_to_string(&out_path).unwrap().contains(TOKEN));
}

// ── Mounts ───────────────────────────────────────────────────────────────────

/// The host mount is what makes the numbers describe the host rather than the
/// container. A prefix that is not there has to stop the start, and has to say
/// which path and what to mount there — the operator reading this is looking at
/// a compose file, not at the source.
#[test]
fn check_runtime_refuses_a_host_mount_that_is_absent() {
    let dir = tempfile::tempdir().unwrap();
    let token = token_file(dir.path(), TOKEN);
    let absent = dir.path().join("hostfs-not-mounted");
    let f = fixture(&slash(&token), &slash(&absent));

    let out = muninn(f.path(), &["check-runtime"]);
    let text = all_output(&out);

    assert_eq!(code(&out), exit::RUNTIME, "output: {text}");
    assert!(
        text.contains("hostfs-not-mounted"),
        "the finding must name the path: {text}"
    );
    assert!(
        text.contains(":ro"),
        "and should show the mount that fixes it: {text}"
    );
}

/// A file where a directory belongs is what a bind mount produces when the
/// source is a file and the destination was meant to be the host root. Telegraf
/// would then read nothing and report nothing, without an error.
#[test]
fn check_runtime_refuses_a_host_mount_that_is_not_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let token = token_file(dir.path(), TOKEN);
    let not_a_dir = dir.path().join("hostfs-is-a-file");
    std::fs::write(&not_a_dir, "").unwrap();
    let f = fixture(&slash(&token), &slash(&not_a_dir));

    let out = muninn(f.path(), &["check-runtime"]);
    let text = all_output(&out);

    assert_eq!(code(&out), exit::RUNTIME, "output: {text}");
    assert!(
        text.contains("not a directory"),
        "the finding should say what is wrong with it: {text}"
    );
}

/// A directory at the mount point is not the same as the host being mounted
/// there. The image creates `/hostfs` itself, so a forgotten `-v /:/hostfs:ro`
/// leaves an empty directory that exists — and every path check above it passes.
/// What catches it is the module saying which path it needs.
#[test]
fn check_runtime_refuses_a_host_mount_that_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let token = token_file(dir.path(), TOKEN);
    let empty = dir.path().join("hostfs");
    std::fs::create_dir(&empty).unwrap();
    let f = fixture(&slash(&token), &slash(&empty));

    let out = muninn(f.path(), &["check-runtime"]);
    let text = all_output(&out);

    assert_eq!(code(&out), exit::RUNTIME, "output: {text}");
    assert!(
        text.contains("proc"),
        "the finding must name the host path the module reads: {text}"
    );
    assert!(
        text.contains("cpu"),
        "and the module that needs it, so the operator can disable it instead: {text}"
    );
}

/// The other side of the same check: with the host really mounted it passes, so
/// a correct deployment is not held up by the check that protects a wrong one.
#[test]
fn check_runtime_passes_once_the_host_mount_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let token = token_file(dir.path(), TOKEN);
    let mounted = dir.path().join("hostfs");
    // What a real `-v /:/hostfs:ro` puts there, reduced to the directories the
    // enabled module declares. Creating the mount point alone is the case above.
    for sub in ["proc", "sys", "etc", "var", "run"] {
        std::fs::create_dir_all(mounted.join(sub)).unwrap();
    }
    let f = fixture(&slash(&token), &slash(&mounted));

    let out = muninn(f.path(), &["check-runtime"]);

    assert_eq!(
        code(&out),
        exit::OK,
        "a complete deployment must pass: {}",
        all_output(&out)
    );
}

/// `check-runtime` reports everything it finds rather than stopping at the
/// first. An operator fixing a deployment one restart per problem is the
/// behaviour this avoids.
#[test]
fn check_runtime_reports_every_problem_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let token = token_file(dir.path(), TOKEN);
    let absent = dir.path().join("hostfs-not-mounted");
    let f = fixture(&slash(&token), &slash(&absent));

    // A second, independent problem: a runtime directory that cannot be created
    // because its parent is a file.
    let blocked = f.dir().join("blocked");
    std::fs::write(&blocked, "").unwrap();
    let config = std::fs::read_to_string(f.path()).unwrap().replace(
        &slash(&f.dir().join("telegraf.conf")),
        &slash(&blocked.join("telegraf.conf")),
    );
    std::fs::write(f.path(), config).unwrap();

    let out = muninn(f.path(), &["check-runtime"]);
    let text = all_output(&out);

    assert_eq!(code(&out), exit::RUNTIME, "output: {text}");
    assert!(
        text.contains("hostfs-not-mounted") && text.contains("blocked"),
        "both problems must be reported in one run: {text}"
    );
}
