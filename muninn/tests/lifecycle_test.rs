//! The compiled binary, run as a subprocess, against a real Telegraf.
//!
//! Every other test in this workspace exercises a function. These observe the
//! shipped artefact, because muninn's whole job is what happens across a process
//! lifecycle: spawning a child, forwarding a stop signal, waiting out a grace
//! period, reaping, and exiting with a code an orchestrator reads.
//!
//! **No in-process test can see any of that.** They all run inside the test
//! harness's own runtime, which outlives the thing under test; production has no
//! such runtime. huginn.io shipped a daemon that exited at startup and monitored
//! nothing for months, with a green test suite, for exactly this reason.
//!
//! # Requires a real Telegraf
//!
//! Set `MUNINN_TELEGRAF_BIN` to a Telegraf 1.39.2 binary. Without it the tests
//! that need one skip loudly rather than passing vacuously — a skipped test that
//! reports success is worse than no test.
//!
//! ```text
//! MUNINN_TELEGRAF_BIN=/usr/local/bin/telegraf cargo test -p muninn
//! ```

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The Telegraf binary to test against, if one is available.
fn telegraf_binary() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("MUNINN_TELEGRAF_BIN")?);
    path.exists().then_some(path)
}

/// Skip with a visible reason. A test that silently passes when its
/// precondition is absent is indistinguishable from one that verified something.
macro_rules! require_telegraf {
    () => {
        match telegraf_binary() {
            Some(p) => p,
            None => {
                eprintln!(
                    "SKIP: no Telegraf binary. Set MUNINN_TELEGRAF_BIN to a 1.39.2 binary to run this test."
                );
                return;
            }
        }
    };
}

struct Fixture {
    dir: tempfile::TempDir,
    config: PathBuf,
}

/// A configuration that collects locally and writes nowhere over the network.
///
/// Both listeners take a port the caller chooses, so tests running concurrently
/// do not fight over them.
fn fixture(port: u16, generated: &Path) -> Fixture {
    fixture_with_health(port, free_port(), generated)
}

fn fixture_with_health(port: u16, health_port: u16, generated: &Path) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    // muninn refuses to start with host modules enabled, no host mount prefix,
    // and a container around it — that combination makes Telegraf report the
    // container's CPU and disks as the host's. The Linux suite runs in exactly
    // such a container and mounts the host at /hostfs (see scripts/test-linux.sh),
    // so the fixture describes the deployment it is actually running in rather
    // than one muninn would reject.
    let host_mount_prefix = if Path::new("/hostfs").is_dir() {
        "/hostfs"
    } else {
        ""
    };
    let config = dir.path().join("muninn.yaml");
    let mut f = std::fs::File::create(&config).unwrap();
    write!(
        f,
        r#"
version: 1
agent:
  interval: 1s
  flush_interval: 1s
  hostname: "lifecycle-test"
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
  memory:
    enabled: true
outputs:
  prometheus:
    enabled: true
    listen: "127.0.0.1:{port}"
"#,
        generated = generated.display().to_string().replace('\\', "/"),
        port = port,
    )
    .unwrap();
    Fixture { dir, config }
}

impl Fixture {
    fn path(&self) -> &Path {
        &self.config
    }
    fn dir(&self) -> &Path {
        self.dir.path()
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn muninn(telegraf: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_muninn"));
    c.env("MUNINN_TELEGRAF_BIN", telegraf);
    // A stale RUST_LOG in the developer's environment would change what the
    // process prints and make these assertions depend on the shell.
    c.env_remove("RUST_LOG");
    c
}

/// Poll until `f` holds or the deadline passes.
///
/// Polling rather than sleeping: a fixed sleep is a flake waiting for a loaded
/// CI runner, and this suite starts real processes.
fn wait_until(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// A child's stdout, collected on a thread so the test can wait for a marker.
///
/// Reading it also matters for its own sake: a child whose piped stdout is never
/// drained blocks once the pipe buffer fills.
struct LogTail(std::sync::Arc<std::sync::Mutex<String>>);

impl LogTail {
    fn attach(stdout: std::process::ChildStdout) -> Self {
        use std::io::BufRead as _;
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&buffer);
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                sink.lock().unwrap().push_str(&line);
                sink.lock().unwrap().push('\n');
            }
        });
        LogTail(buffer)
    }

    fn contains(&self, needle: &str) -> bool {
        self.0.lock().unwrap().contains(needle)
    }

    fn text(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

/// Wait until muninn reports itself ready.
///
/// Waiting for Telegraf's listener instead is not the same thing, and the
/// difference is observable: the port opens while muninn is still in its
/// post-spawn settle window, before it has confirmed the process is running. A
/// test that killed Telegraf at that moment would get exit 21 (never started)
/// rather than 22 (died while supervised) — correct behaviour, wrong test. This
/// waits for the state muninn actually logs.
fn wait_for_ready(log: &LogTail, limit: Duration) -> bool {
    wait_until(limit, || log.contains("muninn is ready"))
}

// ---------------------------------------------------------------------------
// Version and validation
// ---------------------------------------------------------------------------

#[test]
fn version_reports_both_the_pin_and_what_is_actually_present() {
    let telegraf = require_telegraf!();
    let out = muninn(&telegraf).arg("version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(out.status.success(), "version failed: {text}");
    assert!(text.contains("built for telegraf 1.39.2"), "got: {text}");
    assert!(
        text.contains("telegraf 1.39.2 at"),
        "should report the binary actually present: {text}"
    );
}

/// The mismatch check is the reason the pin exists. Pointing muninn at something
/// that is not Telegraf must refuse rather than proceed.
#[test]
fn a_binary_that_is_not_telegraf_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let generated = dir.path().join("telegraf.conf");
    let fx = fixture(free_port(), &generated);

    // Any program that exists but does not report a Telegraf version.
    let impostor = if cfg!(windows) {
        PathBuf::from("C:\\Windows\\System32\\cmd.exe")
    } else {
        PathBuf::from("/bin/echo")
    };
    if !impostor.exists() {
        eprintln!("SKIP: no impostor binary available on this platform");
        return;
    }

    let out = muninn(&impostor)
        .arg("--config")
        .arg(fx.path())
        .arg("run")
        .output()
        .unwrap();

    assert!(!out.status.success(), "an impostor binary must be refused");
    assert_eq!(
        out.status.code(),
        Some(21),
        "a version problem is TELEGRAF_START"
    );
}

#[test]
fn validate_with_telegraf_accepts_the_generated_configuration() {
    let telegraf = require_telegraf!();
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(free_port(), &dir.path().join("telegraf.conf"));

    let out = muninn(&telegraf)
        .arg("--config")
        .arg(fx.path())
        .args(["validate", "--with-telegraf"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("Telegraf accepted"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// The lifecycle
// ---------------------------------------------------------------------------

/// The bug class this file exists for: a daemon that exits at startup, having
/// collected nothing, while every in-process test passes.
#[test]
fn the_binary_stays_alive_and_serves_metrics() {
    let telegraf = require_telegraf!();
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let generated = dir.path().join("telegraf.conf");
    let fx = fixture(port, &generated);

    let mut child = muninn(&telegraf)
        .arg("--config")
        .arg(fx.path())
        .arg("run")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let log = LogTail::attach(child.stdout.take().unwrap());

    // Telegraf's own listener answering is proof the whole chain worked:
    // generated, verified, spawned, and collecting.
    let url = format!("http://127.0.0.1:{port}/metrics");
    let serving = wait_until(Duration::from_secs(30), || {
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    let ready = wait_for_ready(&log, Duration::from_secs(30));
    let still_running = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        still_running,
        "muninn exited on its own — it must run until signalled.\n{}",
        log.text()
    );
    assert!(
        serving,
        "nothing was listening on {url} within 30s.\n{}",
        log.text()
    );
    assert!(ready, "muninn never reported itself ready.\n{}", log.text());
    assert!(
        generated.exists(),
        "the generated configuration was never written"
    );
}

/// The generated file holds resolved secrets, so it must not be persisted with
/// permissions that let anyone else read it.
#[cfg(unix)]
#[test]
fn the_generated_configuration_is_not_world_readable() {
    use std::os::unix::fs::PermissionsExt as _;
    let telegraf = require_telegraf!();
    let dir = tempfile::tempdir().unwrap();
    let generated = dir.path().join("telegraf.conf");
    let fx = fixture(free_port(), &generated);

    let mut child = muninn(&telegraf)
        .arg("--config")
        .arg(fx.path())
        .arg("run")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let written = wait_until(Duration::from_secs(30), || generated.exists());
    let mode = written.then(|| std::fs::metadata(&generated).unwrap().permissions().mode());

    let _ = child.kill();
    let _ = child.wait();

    assert!(written, "the configuration was never written");
    assert_eq!(
        mode.unwrap() & 0o077,
        0,
        "mode {:o} is readable by others",
        mode.unwrap()
    );
}

/// SIGTERM is what `docker stop` sends. Catching only SIGINT means the shutdown
/// path never runs under Docker or systemd, and the container is killed ten
/// seconds later instead — every time.
#[cfg(unix)]
#[test]
fn sigterm_produces_a_clean_shutdown_within_the_grace_period() {
    let telegraf = require_telegraf!();
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(port, &dir.path().join("telegraf.conf"));

    let mut child = muninn(&telegraf)
        .arg("--config")
        .arg(fx.path())
        .arg("run")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let log = LogTail::attach(child.stdout.take().unwrap());

    assert!(
        wait_for_ready(&log, Duration::from_secs(30)),
        "muninn never became ready:
{}",
        log.text()
    );

    let started = Instant::now();
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let status = child.wait().unwrap();
    let took = started.elapsed();

    assert_eq!(
        status.code(),
        Some(0),
        "a requested shutdown must exit 0, got {status:?}"
    );
    // The grace period is 10s in the fixture; a clean stop should be far under
    // it. Exceeding it would mean the SIGKILL path ran, which is not "clean".
    assert!(
        took < Duration::from_secs(10),
        "shutdown took {took:?}, which means the grace period expired"
    );

    // And nothing is left holding the port — muninn is PID 1 in its container,
    // so a surviving child would mean the next start fails to bind.
    assert!(
        wait_until(Duration::from_secs(5), || {
            std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
        }),
        "the Telegraf listener outlived muninn"
    );
}

/// A dead Telegraf must be reported, not papered over. Exit 22 is what tells an
/// orchestrator to restart the container — and what keeps a crash from being
/// invisible inside a container that still looks healthy.
#[cfg(unix)]
#[test]
fn a_telegraf_that_dies_takes_muninn_down_with_exit_22() {
    let telegraf = require_telegraf!();
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(port, &dir.path().join("telegraf.conf"));

    let mut child = muninn(&telegraf)
        .arg("--config")
        .arg(fx.path())
        .arg("run")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let log = LogTail::attach(child.stdout.take().unwrap());

    // Readiness, not the open port. Telegraf's listener comes up while muninn is
    // still in its settle window, and killing it there yields exit 21 (never
    // started) rather than 22 (died while supervised) — which is correct
    // behaviour and the wrong thing to be testing.
    assert!(
        wait_for_ready(&log, Duration::from_secs(30)),
        "muninn never became ready:
{}",
        log.text()
    );

    // Kill the child Telegraf, leaving muninn alive: exactly the situation where
    // a restart loop would hide the failure.
    let killed = Command::new("pkill")
        .args(["-KILL", "-P", &child.id().to_string()])
        .status();
    if killed.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        eprintln!("SKIP: pkill is not available to kill the child process");
        return;
    }

    let exited = wait_until(Duration::from_secs(20), || {
        child.try_wait().unwrap().is_some()
    });
    let status = child.try_wait().unwrap();
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
        panic!("muninn kept running after Telegraf died — a crash must never be invisible");
    }

    assert_eq!(
        status.unwrap().code(),
        Some(22),
        "a Telegraf that dies while supervised is TELEGRAF_EXITED:
{}",
        log.text()
    );
}

// ---------------------------------------------------------------------------
// Failure paths that need no Telegraf
// ---------------------------------------------------------------------------

#[test]
fn a_missing_telegraf_binary_exits_21_and_says_where_to_look() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(free_port(), &dir.path().join("telegraf.conf"));

    let out = muninn(Path::new("/nonexistent/telegraf"))
        .arg("--config")
        .arg(fx.path())
        .arg("run")
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(21));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("MUNINN_TELEGRAF_BIN"), "got: {stderr}");
}

#[test]
fn a_missing_configuration_file_exits_10() {
    let out = muninn(Path::new("/nonexistent/telegraf"))
        .arg("--config")
        .arg("/nonexistent/muninn.yaml")
        .arg("run")
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(10), "a config problem is CONFIG");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("does not exist"), "got: {stderr}");
}

/// `render-config` must not need Telegraf, and must not start anything: it is
/// what an operator runs to inspect the configuration before deploying.
#[test]
fn render_config_works_without_telegraf_and_redacts() {
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture(free_port(), &dir.path().join("telegraf.conf"));

    let out = muninn(Path::new("/nonexistent/telegraf"))
        .arg("--config")
        .arg(fx.path())
        .arg("render-config")
        .output()
        .unwrap();

    assert!(out.status.success(), "render-config needed Telegraf");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[[inputs.cpu]]"), "got: {stdout}");
    assert!(
        !fx.dir().join("telegraf.conf").exists(),
        "render-config wrote the runtime file; it should start and touch nothing"
    );
}

// ---------------------------------------------------------------------------
// Health endpoints, on the running binary
// ---------------------------------------------------------------------------

/// A bare HTTP GET. One endpoint on loopback does not justify an HTTP client
/// with a TLS stack and a connection pool — the same reasoning as `muninn
/// healthcheck` itself.
fn http_get(port: u16, path: &str) -> Option<(u16, String)> {
    use std::io::{Read as _, Write as _};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let code = response.split_whitespace().nth(1)?.parse().ok()?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())?;
    Some((code, body))
}

/// The endpoints must reflect the real lifecycle, not a state a unit test set by
/// hand. This starts the actual binary and watches readiness turn true.
#[test]
fn the_health_endpoints_follow_the_real_lifecycle() {
    let telegraf = require_telegraf!();
    let health_port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture_with_health(free_port(), health_port, &dir.path().join("telegraf.conf"));

    let mut child = muninn(&telegraf)
        .arg("--config")
        .arg(fx.path())
        .arg("run")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let log = LogTail::attach(child.stdout.take().unwrap());

    // Liveness comes up with the server, before readiness does — that ordering
    // is the whole reason they are two endpoints.
    let live_first = wait_until(Duration::from_secs(30), || {
        matches!(http_get(health_port, "/health/live"), Some((200, _)))
    });

    let ready = wait_until(Duration::from_secs(30), || {
        matches!(http_get(health_port, "/health/ready"), Some((200, _)))
    });

    let ready_body = http_get(health_port, "/health/ready")
        .map(|(_, b)| b)
        .unwrap_or_default();
    let status_body = http_get(health_port, "/status")
        .map(|(_, b)| b)
        .unwrap_or_default();
    let metrics_body = http_get(health_port, "/metrics")
        .map(|(_, b)| b)
        .unwrap_or_default();

    let _ = child.kill();
    let _ = child.wait();

    assert!(live_first, "liveness never came up:\n{}", log.text());
    assert!(ready, "readiness never came up:\n{}", log.text());

    assert!(ready_body.contains("\"status\":\"ready\""), "{ready_body}");
    assert!(ready_body.contains("\"running\":true"), "{ready_body}");
    assert!(
        ready_body.contains("\"pid\""),
        "should report Telegraf's PID: {ready_body}"
    );

    assert!(
        status_body.contains("\"telegraf_version\":\"1.39.2\""),
        "{status_body}"
    );
    assert!(
        status_body.contains("\"cpu\""),
        "enabled modules: {status_body}"
    );
    assert!(
        status_body.contains("\"prometheus\""),
        "enabled outputs: {status_body}"
    );

    assert!(metrics_body.contains("muninn_ready 1"), "{metrics_body}");
    assert!(
        metrics_body.contains("muninn_telegraf_running 1"),
        "{metrics_body}"
    );
    // Recorded by the real startup path, not by a test.
    assert!(
        metrics_body.contains("muninn_config_generation_duration_seconds"),
        "generation should have been timed: {metrics_body}"
    );
    assert!(
        metrics_body.contains("muninn_telegraf_validation_duration_seconds"),
        "validation should have been timed: {metrics_body}"
    );
}

/// `muninn healthcheck` is what Docker's HEALTHCHECK runs. It must succeed
/// against a ready agent and fail when there is nothing to reach.
#[test]
fn the_healthcheck_command_reflects_readiness() {
    let telegraf = require_telegraf!();
    let health_port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let fx = fixture_with_health(free_port(), health_port, &dir.path().join("telegraf.conf"));

    // Nothing running yet: it must fail rather than hang or report healthy.
    let out = muninn(&telegraf)
        .arg("--config")
        .arg(fx.path())
        .arg("healthcheck")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "healthcheck succeeded with no agent running"
    );

    let mut child = muninn(&telegraf)
        .arg("--config")
        .arg(fx.path())
        .arg("run")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let log = LogTail::attach(child.stdout.take().unwrap());

    let became_ready = wait_until(Duration::from_secs(30), || {
        muninn(&telegraf)
            .arg("--config")
            .arg(fx.path())
            .arg("healthcheck")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    });

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        became_ready,
        "healthcheck never reported ready:\n{}",
        log.text()
    );
}

// ---------------------------------------------------------------------------
// The updates module
// ---------------------------------------------------------------------------

/// Run `muninn update-check` exactly as Telegraf's `inputs.exec` would.
fn update_check(hostfs: &Path) -> (std::process::Output, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_muninn"))
        .arg("update-check")
        .arg("--hostfs")
        .arg(hostfs)
        // Otherwise a HOSTFS in the developer's environment would decide what
        // this test looks at.
        .env_remove("HOSTFS")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    (out, stdout)
}

/// The invariant, observed on the shipped binary rather than on a function: a
/// check that cannot look reports that it could not look. It never reports zero,
/// and it never fails in a way that would make Telegraf emit nothing at all.
#[test]
fn an_update_check_without_a_host_mount_reports_failure_not_zero() {
    let dir = tempfile::tempdir().unwrap();
    let (out, line) = update_check(&dir.path().join("never-mounted"));

    assert!(
        out.status.success(),
        "must exit 0 — a non-zero exit makes Telegraf log an error and emit nothing, \
         which is indistinguishable from the module being off. stdout: {line}"
    );
    assert!(line.contains("check_success=0i"), "{line}");
    assert!(line.contains("reason=hostfs_not_mounted"), "{line}");
    assert!(
        !line.contains("pending"),
        "a failed check must not carry a count: {line}"
    );
    // The reason is a tag, so the detail behind it belongs on stderr, where
    // Telegraf logs it.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("update check"),
        "no detail for the log: {stderr}"
    );
}

/// A host that is not Debian-family gets a refusal, not a number derived from a
/// package manager it does not use.
#[test]
fn an_update_check_refuses_a_host_it_does_not_understand() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("var/lib/dpkg")).unwrap();
    std::fs::create_dir_all(root.join("var/lib/apt/lists")).unwrap();
    std::fs::create_dir_all(root.join("etc/apt")).unwrap();
    std::fs::write(root.join("var/lib/dpkg/status"), "Package: x\n").unwrap();
    std::fs::write(root.join("var/lib/apt/lists/x_Packages"), "Package: x\n").unwrap();
    std::fs::write(root.join("etc/os-release"), "ID=alpine\n").unwrap();

    let (out, line) = update_check(root);
    assert!(out.status.success(), "{line}");
    assert!(line.contains("reason=host_not_debian_family"), "{line}");
    assert!(!line.contains("pending"), "{line}");
}

/// `HOSTFS` is what the rendered configuration passes, so it has to work as well
/// as the flag. If it did not, the module would quietly check the container.
#[test]
fn the_update_check_reads_the_host_prefix_from_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("also-never-mounted");
    let out = Command::new(env!("CARGO_BIN_EXE_muninn"))
        .arg("update-check")
        .env("HOSTFS", &missing)
        .output()
        .unwrap();
    let line = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{line}");
    assert!(line.contains("reason=hostfs_not_mounted"), "{line}");
}

/// Whether the mounted host tree is one the updates module supports.
///
/// This decides which of two correct behaviours the test below asserts, and the
/// distinction is real rather than a testing convenience: a host that is not
/// Debian-family fails a *precondition*, which muninn refuses to start on, while
/// a check that fails with its preconditions met only degrades it.
#[cfg(unix)]
fn hostfs_is_debian_family() -> bool {
    ["/hostfs/etc/os-release", "/hostfs/usr/lib/os-release"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .any(|text| {
            text.lines()
                .filter(|l| l.starts_with("ID=") || l.starts_with("ID_LIKE="))
                .any(|l| l.contains("debian") || l.contains("ubuntu"))
        })
}

/// With the module enabled, muninn checks once at startup and reports what it
/// found — and stays serving either way.
///
/// The point is the "either way". Whether this host's package state can be *read*
/// decides which branch is asserted, not whether the agent survives: a module
/// that cannot answer must degrade muninn, never stop it, because CPU, memory
/// and disk collection have nothing to do with counting packages.
///
/// A host that is not Debian-family is the other case, and it is asserted here
/// too: that is a *precondition*, checked before anything starts, and refusing
/// with exit 12 is right — the operator enabled a module their host cannot
/// support, and no amount of running would make it work.
///
/// Which branch runs is decided by reading the host, not by assuming one. That
/// distinction found a real bug: on Docker Desktop this test took the refusal
/// branch against a host that is plainly Debian, because its `/etc/os-release`
/// carries only `PRETTY_NAME` and muninn stopped at the first file it could
/// open.
#[cfg(unix)]
#[test]
fn an_enabled_updates_module_reports_itself_and_never_stops_the_agent() {
    let telegraf = require_telegraf!();
    if !Path::new("/hostfs").is_dir() {
        eprintln!(
            "SKIP: no /hostfs mount. Run under scripts/test-linux.sh, which mounts the host root \
             the way the documented deployment does."
        );
        return;
    }
    let debian_host = hostfs_is_debian_family();

    let health_port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let generated = dir.path().join("telegraf.conf");
    let config = dir.path().join("muninn.yaml");
    std::fs::write(
        &config,
        format!(
            r#"
version: 1
agent:
  interval: 1s
  flush_interval: 1s
  hostname: "lifecycle-test"
runtime:
  shutdown_grace_period: 10s
  telegraf_start_timeout: 15s
  generated_config_path: "{generated}"
  host_mount_prefix: "/hostfs"
logging:
  format: json
  level: info
health:
  listen: "127.0.0.1:{health_port}"
modules:
  cpu:
    enabled: true
  updates:
    enabled: true
    interval: 1h
outputs:
  prometheus:
    enabled: true
    listen: "127.0.0.1:{prom_port}"
"#,
            generated = generated.display(),
            prom_port = free_port(),
        ),
    )
    .unwrap();

    if !debian_host {
        // The precondition case: refuse, name the module, and say what to do.
        let out = muninn(&telegraf)
            .arg("--config")
            .arg(&config)
            .arg("run")
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(12),
            "a host the module cannot support is a runtime precondition failure: {text}"
        );
        assert!(
            text.contains("updates") && text.contains("Debian"),
            "the refusal must name the module and the reason: {text}"
        );
        return;
    }

    let mut child = muninn(&telegraf)
        .arg("--config")
        .arg(&config)
        .arg("run")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let log = LogTail::attach(child.stdout.take().unwrap());

    let ready = wait_for_ready(&log, Duration::from_secs(30));
    // The check runs after readiness, so give it its own window.
    let checked = wait_until(Duration::from_secs(60), || {
        http_get(health_port, "/metrics")
            .map(|(_, b)| b.contains("muninn_module_check_success{module=\"updates\"}"))
            .unwrap_or(false)
    });

    let metrics = http_get(health_port, "/metrics")
        .map(|(_, b)| b)
        .unwrap_or_default();
    let status = http_get(health_port, "/status")
        .map(|(_, b)| b)
        .unwrap_or_default();
    let still_serving = matches!(http_get(health_port, "/health/ready"), Some((200, _)));

    let _ = child.kill();
    let _ = child.wait();

    assert!(ready, "never became ready:\n{}", log.text());
    assert!(
        checked,
        "the startup check never reported a result:\n{}\n{metrics}",
        log.text()
    );
    assert!(
        status.contains("\"updates\""),
        "/status should name the module and its check: {status}"
    );
    assert!(
        still_serving,
        "a failing updates module must not take a working agent out of service: {metrics}"
    );

    // Whichever way the check went, it has to be legible in the log.
    let text = log.text();
    assert!(
        text.contains("updates check") || text.contains("could not read the host's package state"),
        "the result should be in the log:\n{text}"
    );
}
