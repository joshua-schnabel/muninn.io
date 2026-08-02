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
/// Prometheus is the only output, on a port the caller chooses, so two tests
/// running concurrently do not fight over a listener.
fn fixture(port: u16, generated: &Path) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
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
  host_mount_prefix: ""
logging:
  format: json
  level: info
health:
  listen: "127.0.0.1:0"
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
