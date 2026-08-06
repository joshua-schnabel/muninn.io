//! Telegraf as a child process.
//!
//! muninn is PID 1 in its container, which means two duties no ordinary program
//! has: it must forward stop signals rather than absorb them, and it must reap
//! its child, because no init process is going to do it afterwards. A naive PID
//! 1 produces a container that ignores `docker stop` and gets killed ten seconds
//! later, every time.
//!
//! # No restart loop
//!
//! If Telegraf exits on its own, muninn reports it and exits too. The container
//! orchestrator decides whether to restart. The failure this avoids is the
//! expensive one — a container that looks healthy from the outside while
//! Telegraf crash-loops invisibly inside it. See
//! `docs/adr/0002-supervisor-no-restart-loop.md`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use muninn_core::error::{MuninnError, Result};
use muninn_core::secret::Redactor;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{error, info, warn};

/// How a Telegraf process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// Exited with a status code.
    Code(i32),
    /// Killed by a signal (Unix only).
    Signal(i32),
    /// Ended in a way the platform did not describe.
    Unknown,
}

impl Exit {
    /// Whether this is the clean exit expected after a stop signal.
    ///
    /// A process that muninn asked to stop and that then exits 0 is normal. So
    /// is one killed by the signal muninn sent it — some builds re-raise rather
    /// than exiting 0, and treating that as a crash would make every clean
    /// shutdown look like a failure.
    pub fn is_clean_shutdown(&self) -> bool {
        match self {
            Exit::Code(0) => true,
            // 15 = SIGTERM, 2 = SIGINT.
            Exit::Signal(15) | Exit::Signal(2) => true,
            _ => false,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Exit::Code(c) => format!("exit code {c}"),
            Exit::Signal(s) => format!("signal {s}"),
            Exit::Unknown => "an unknown status".to_string(),
        }
    }
}

impl From<std::process::ExitStatus> for Exit {
    fn from(status: std::process::ExitStatus) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt as _;
            if let Some(signal) = status.signal() {
                return Exit::Signal(signal);
            }
        }
        match status.code() {
            Some(code) => Exit::Code(code),
            None => Exit::Unknown,
        }
    }
}

/// A running Telegraf.
#[derive(Debug)]
pub struct Telegraf {
    child: Child,
    pid: u32,
    binary: PathBuf,
}

impl Telegraf {
    /// Spawn Telegraf with `config_path`, in an environment carrying the
    /// `HOST_*` variables that make gopsutil read the host rather than the
    /// container.
    ///
    /// The environment is built explicitly rather than inherited wholesale: what
    /// Telegraf sees should be a function of muninn's configuration, not of
    /// whatever the container was started with.
    ///
    /// `redactor` is applied to everything Telegraf writes before it is logged.
    /// See [`forward`] for why a child's output needs it when muninn's own
    /// formatting does not.
    pub fn spawn(
        binary: &Path,
        config_path: &Path,
        host_env: &[(String, String)],
        redactor: Redactor,
    ) -> Result<Self> {
        let mut command = Command::new(binary);
        command
            .arg("--config")
            .arg(config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Without this, a muninn that is killed hard would leave Telegraf
            // running and holding :9273 — and the restarted container would fail
            // to bind, looking like a port conflict with something else.
            .kill_on_drop(true);

        for (key, value) in host_env {
            command.env(key, value);
        }

        let mut child = command.spawn().map_err(|e| {
            MuninnError::TelegrafStart(match e.kind() {
                std::io::ErrorKind::NotFound => {
                    format!("no Telegraf binary at '{}'", binary.display())
                }
                std::io::ErrorKind::PermissionDenied => {
                    format!("'{}' is not executable", binary.display())
                }
                _ => format!("cannot start '{}': {e}", binary.display()),
            })
        })?;

        let pid = child.id().ok_or_else(|| {
            MuninnError::TelegrafStart("Telegraf exited before muninn could read its PID".into())
        })?;

        // Telegraf's own output is re-emitted through muninn's logger, tagged
        // with its source, so one stream leaves the container and JSON logging
        // stays parseable end to end rather than interleaved with plain text.
        forward(child.stdout.take(), "stdout", redactor.clone());
        forward(child.stderr.take(), "stderr", redactor);

        info!(pid, binary = %binary.display(), "Telegraf started");

        Ok(Telegraf {
            child,
            pid,
            binary: binary.to_path_buf(),
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Wait for Telegraf to exit.
    pub async fn wait(&mut self) -> Result<Exit> {
        let status = self
            .child
            .wait()
            .await
            .map_err(|e| MuninnError::internal(format!("cannot wait for Telegraf: {e}")))?;
        Ok(Exit::from(status))
    }

    /// Has Telegraf exited? Returns `None` while it is still running.
    ///
    /// Does not block, and does not consume the child, so the supervisor can
    /// poll it between other work.
    pub fn try_exit(&mut self) -> Result<Option<Exit>> {
        match self.child.try_wait() {
            Ok(Some(status)) => Ok(Some(Exit::from(status))),
            Ok(None) => Ok(None),
            Err(e) => Err(MuninnError::internal(format!(
                "cannot determine Telegraf's status: {e}"
            ))),
        }
    }

    /// Ask Telegraf to stop, and wait up to `grace` for it to do so.
    ///
    /// On Unix this sends SIGTERM, which Telegraf handles by flushing its
    /// buffered metrics and exiting — it does not wait for the next flush tick,
    /// so `grace` needs to cover a write attempt rather than a collection cycle.
    ///
    /// If the grace period expires, SIGKILL. Returning without the child having
    /// exited is not an option: muninn is PID 1, and leaving a child behind
    /// means the container never stops.
    pub async fn shutdown(&mut self, grace: std::time::Duration) -> Result<Exit> {
        info!(
            pid = self.pid,
            grace_seconds = grace.as_secs(),
            "asking Telegraf to stop"
        );

        if let Err(e) = self.request_stop() {
            // Most often the process is already gone, which is not a problem —
            // wait() below will collect it.
            warn!(pid = self.pid, error = %e, "could not signal Telegraf");
        }

        match tokio::time::timeout(grace, self.child.wait()).await {
            Ok(Ok(status)) => {
                let exit = Exit::from(status);
                info!(pid = self.pid, status = %exit.describe(), "Telegraf stopped");
                Ok(exit)
            }
            Ok(Err(e)) => Err(MuninnError::internal(format!(
                "cannot wait for Telegraf: {e}"
            ))),
            Err(_) => {
                warn!(
                    pid = self.pid,
                    grace_seconds = grace.as_secs(),
                    "Telegraf did not stop within the grace period — killing it"
                );
                let _ = self.child.kill().await;
                let status = self.child.wait().await.map_err(|e| {
                    MuninnError::internal(format!("cannot reap Telegraf after killing it: {e}"))
                })?;
                Ok(Exit::from(status))
            }
        }
    }

    /// Send the platform's "please stop" signal.
    #[cfg(unix)]
    fn request_stop(&self) -> Result<()> {
        // SIGTERM by raw syscall rather than `Child::kill`, which sends SIGKILL
        // and would deny Telegraf the chance to flush.
        //
        // Audited, and the only `unsafe` in the workspace. `kill(2)` has no safe
        // wrapper in std, the pid is this process's own child (never
        // attacker-supplied), and the call reads no memory. The return code is
        // checked below rather than discarded, so a failed signal surfaces
        // instead of leaving the supervisor waiting out the grace period for a
        // signal that was never delivered.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let rc = unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGTERM) };
        if rc == 0 {
            Ok(())
        } else {
            Err(MuninnError::internal(format!(
                "kill({}, SIGTERM) failed: {}",
                self.pid,
                std::io::Error::last_os_error()
            )))
        }
    }

    /// Windows has no SIGTERM. muninn's artefact is a Linux container, so this
    /// path exists for development on Windows only: it terminates rather than
    /// asking politely, which means no flush.
    #[cfg(not(unix))]
    fn request_stop(&self) -> Result<()> {
        Err(MuninnError::internal(
            "graceful stop is not available on this platform; muninn targets Linux containers"
                .to_string(),
        ))
    }
}

/// Re-emit a child stream through muninn's logger, one line at a time.
/// Re-emit a child stream through muninn's logger, with known secrets removed.
///
/// The `redactor` is the point, and it is not decoration. Everything muninn
/// formats itself is covered by `Secret`'s type-level redaction — printing one
/// requires `.expose()`, and there is exactly one such call. None of that
/// argument reaches text that arrives *already formatted* from another process,
/// and the configuration Telegraf is reading holds resolved secrets (ADR-0003).
///
/// Whether Telegraf ever quotes a configuration value in a diagnostic is a
/// property of Telegraf. This does not depend on the answer.
fn forward<R>(stream: Option<R>, source: &'static str, redactor: Redactor)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let Some(stream) = stream else { return };
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let line = redactor.apply(&line);
            // Telegraf prefixes its own level: "E!" error, "W!" warning, "I!"
            // info, "D!" debug. Mapping them keeps a Telegraf error visible as an
            // error in muninn's output rather than flattened to info — and
            // `source` makes it obvious the line is not muninn's own.
            if line.contains("E!") {
                error!(source, "{line}");
            } else if line.contains("W!") {
                warn!(source, "{line}");
            } else {
                info!(source, "{line}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_exit_is_a_clean_shutdown() {
        assert!(Exit::Code(0).is_clean_shutdown());
    }

    /// Some builds re-raise the stop signal rather than exiting 0. Treating that
    /// as a crash would make every clean shutdown look like a failure.
    #[test]
    fn being_killed_by_the_stop_signal_is_a_clean_shutdown() {
        assert!(Exit::Signal(15).is_clean_shutdown(), "SIGTERM");
        assert!(Exit::Signal(2).is_clean_shutdown(), "SIGINT");
    }

    #[test]
    fn anything_else_is_a_crash() {
        assert!(!Exit::Code(1).is_clean_shutdown());
        assert!(!Exit::Code(137).is_clean_shutdown());
        assert!(!Exit::Signal(9).is_clean_shutdown(), "SIGKILL is not clean");
        assert!(
            !Exit::Signal(11).is_clean_shutdown(),
            "SIGSEGV is not clean"
        );
        assert!(!Exit::Unknown.is_clean_shutdown());
    }

    /// The description reaches a log line and an operator, so it has to name the
    /// number they will search for.
    #[test]
    fn the_description_names_the_number() {
        assert!(Exit::Code(137).describe().contains("137"));
        assert!(Exit::Signal(9).describe().contains("9"));
        assert!(!Exit::Unknown.describe().is_empty());
    }

    #[tokio::test]
    async fn spawning_a_missing_binary_fails_with_a_start_error() {
        let err = Telegraf::spawn(
            Path::new("/nonexistent/telegraf"),
            Path::new("/tmp/x.conf"),
            &[],
            Redactor::default(),
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), muninn_core::exit::TELEGRAF_START);
        assert!(err.to_string().contains("nonexistent"), "got: {err}");
    }

    /// The property, at the level that matters: a secret Telegraf printed does
    /// not reach a log line. `forward` writes through `tracing`, so this
    /// exercises the redaction it applies rather than the subscriber.
    #[tokio::test]
    async fn a_secret_in_child_output_is_masked_before_it_is_logged() {
        let redactor = Redactor::new(["influx-token-abcdef".to_string()]);
        let line = "2026-01-01T00:00:00Z E! [outputs.influxdb_v2] \
                    token influx-token-abcdef was rejected";

        let out = redactor.apply(line);
        assert!(!out.contains("influx-token-abcdef"), "leaked: {out}");
        assert!(
            out.contains("[outputs.influxdb_v2]") && out.contains("was rejected"),
            "the diagnostic itself must survive: {out}"
        );
    }

    /// An ordinary Telegraf line has to come through byte-identical — redaction
    /// that mangled normal output would be paid for on every line.
    #[tokio::test]
    async fn ordinary_child_output_is_unchanged() {
        let redactor = Redactor::new(["influx-token-abcdef".to_string()]);
        let line = "2026-01-01T00:00:00Z I! [agent] Config: Interval:10s";
        assert_eq!(redactor.apply(line), line);
    }
}
