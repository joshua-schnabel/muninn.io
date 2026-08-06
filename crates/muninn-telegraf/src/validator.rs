//! Having Telegraf check the configuration muninn just generated.
//!
//! ```text
//! telegraf config check --strict-env-handling --config <file>
//! ```
//!
//! `config check` loads the configuration and **initialises the plugins without
//! starting them**. That is why it is used rather than `--test`, which runs a
//! collection cycle — meaning `outputs.prometheus_client` binds `:9273`, the
//! port the real process is about to need. A validation step that races the
//! thing it validates is not a validation step. See
//! `docs/adr/0006-validate-with-config-check.md`.
//!
//! What this does *not* catch is as important as what it does. Initialising is
//! not running: a Docker endpoint that does not exist, a port already taken on
//! the host, a mount that is missing — none are visible here. That is why
//! `muninn check-runtime` is a separate startup step and why readiness waits for
//! Telegraf to actually be running.

use std::path::Path;
use std::process::Command;

use muninn_core::error::{MuninnError, Result};

/// Check `config_path` with `binary`.
///
/// A failure is [`MuninnError::TelegrafConfig`], which exits 20 — documented as
/// a muninn bug or a version mismatch, never operator error. The operator never
/// writes TOML.
pub fn check_config(binary: &Path, config_path: &Path) -> Result<()> {
    let output = Command::new(binary)
        .arg("config")
        .arg("check")
        // Strict handling became the default in Telegraf 1.38, and running
        // without an explicit choice prints a warning on every start. muninn
        // generates no ${...} references at all — secrets are resolved into the
        // file — so strict costs nothing and silences the noise.
        .arg("--strict-env-handling")
        .arg("--config")
        .arg(config_path)
        .output()
        .map_err(|e| {
            MuninnError::TelegrafStart(format!("cannot run '{}': {e}", binary.display()))
        })?;

    if output.status.success() {
        return Ok(());
    }

    Err(MuninnError::TelegrafConfig(format!(
        "`telegraf config check` rejected the generated configuration.\n{}\n\
         This is a muninn bug or a Telegraf version mismatch — the configuration is generated, \
         not written by hand. Please report it, attaching the output of `muninn render-config` \
         (which redacts secrets).",
        indent(&diagnostics(&output))
    )))
}

/// The useful part of Telegraf's output.
///
/// Telegraf reports configuration problems on stderr and logs an informational
/// "Loading config" line there too. Both streams are considered so a build that
/// changes where it writes does not turn a diagnosable failure into an empty
/// message.
fn diagnostics(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let text: String = stderr
        .lines()
        .chain(stdout.lines())
        // Drop Telegraf's own progress chatter; keep anything that looks like a
        // complaint.
        .filter(|l| !l.contains("I! Loading config"))
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    if text.is_empty() {
        format!("telegraf exited with {} and said nothing", output.status)
    } else {
        text
    }
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an `Output` with a failing status.
    ///
    /// `ExitStatus` cannot be constructed portably, so this goes through each
    /// platform's extension trait. Development happens on Windows and the
    /// artefact runs on Linux, so both have to compile — a unix-only test helper
    /// would mean these tests never run where they are written.
    fn failed_output(stderr: &str, stdout: &str) -> std::process::Output {
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt as _;
            std::process::ExitStatus::from_raw(256) // exit code 1
        };
        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt as _;
            std::process::ExitStatus::from_raw(1)
        };

        std::process::Output {
            status,
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn diagnostics_drop_telegrafs_progress_chatter() {
        let o = failed_output(
            "2026-08-02T10:00:00Z I! Loading config: /run/muninn/telegraf.conf\n\
             2026-08-02T10:00:00Z E! error loading config: undefined but requested input: nope\n",
            "",
        );
        let text = diagnostics(&o);
        assert!(!text.contains("Loading config"), "chatter kept: {text}");
        assert!(
            text.contains("undefined but requested input"),
            "got: {text}"
        );
    }

    /// A silent failure still has to produce something a human can act on.
    #[test]
    fn a_silent_failure_still_reports_the_exit_status() {
        let text = diagnostics(&failed_output("", ""));
        assert!(text.contains("said nothing"), "got: {text}");
    }

    #[test]
    fn a_missing_binary_is_a_start_failure_not_a_config_failure() {
        let err = check_config(
            Path::new("/nonexistent/telegraf"),
            Path::new("/tmp/whatever.conf"),
        )
        .unwrap_err();
        assert_eq!(
            err.exit_code(),
            muninn_core::exit::TELEGRAF_START,
            "a missing binary is not the configuration's fault"
        );
    }
}
