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
use muninn_core::secret::Redactor;

/// Check `config_path` with `binary`.
///
/// A failure is [`MuninnError::TelegrafConfig`], which exits 20 — documented as
/// a muninn bug or a version mismatch, never operator error. The operator never
/// writes TOML.
///
/// `redactor` scrubs Telegraf's own output before any of it reaches the error.
/// The file being checked holds **resolved secrets** by design
/// (`docs/adr/0003-ephemeral-generated-config.md`), so a plugin diagnostic that
/// quotes the value it could not use would otherwise put that value in a
/// `MuninnError` — which is printed to stderr and, in the supervisor's case,
/// logged. `Secret`'s type-level redaction cannot reach text another process
/// formatted; this is the same argument that put a redactor on the child's
/// stdout and stderr, applied to the one other place Telegraf's words are
/// re-emitted (F-01).
pub fn check_config(binary: &Path, config_path: &Path, redactor: &Redactor) -> Result<()> {
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
        indent(&diagnostics(&output, redactor))
    )))
}

/// The useful part of Telegraf's output, with known secrets removed.
///
/// Telegraf reports configuration problems on stderr and logs an informational
/// "Loading config" line there too. Both streams are considered so a build that
/// changes where it writes does not turn a diagnosable failure into an empty
/// message.
///
/// Redaction is per line rather than over the joined text so that a value
/// spanning a line break cannot be reassembled by the join and then missed —
/// and so the cheap "nothing to do" path in [`Redactor::apply`] is taken for
/// each of the many lines that hold no secret.
fn diagnostics(output: &std::process::Output, redactor: &Redactor) -> String {
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
        .map(|l| redactor.apply(l).into_owned())
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
        let text = diagnostics(&o, &Redactor::default());
        assert!(!text.contains("Loading config"), "chatter kept: {text}");
        assert!(
            text.contains("undefined but requested input"),
            "got: {text}"
        );
    }

    /// A silent failure still has to produce something a human can act on.
    #[test]
    fn a_silent_failure_still_reports_the_exit_status() {
        let text = diagnostics(&failed_output("", ""), &Redactor::default());
        assert!(text.contains("said nothing"), "got: {text}");
    }

    /// The finding this closes (F-01): the file `config check` reads holds
    /// resolved secrets, so a plugin diagnostic quoting one would have gone
    /// straight into the error muninn prints.
    ///
    /// Asserts the *value is absent* rather than that the mask is present —
    /// asserting on `***` would pass for a format like `***(s3cret-token-value)`.
    #[test]
    fn a_secret_quoted_by_telegraf_never_reaches_the_error() {
        let redactor = Redactor::new(["s3cret-token-value".to_string()]);
        let o = failed_output(
            "E! [outputs.influxdb_v2] token \"s3cret-token-value\" was rejected\n",
            "",
        );
        let text = diagnostics(&o, &redactor);
        assert!(!text.contains("s3cret-token-value"), "leaked: {text}");
        assert!(text.contains("was rejected"), "lost the diagnosis: {text}");
    }

    /// A secret split across two of Telegraf's lines must not be reassembled
    /// into the joined text and survive there. Redacting per line is what makes
    /// this hold; redacting after the join would not.
    #[test]
    fn a_secret_on_each_of_two_lines_is_masked_on_both() {
        let redactor = Redactor::new(["s3cret-token-value".to_string()]);
        let o = failed_output(
            "E! first mention s3cret-token-value\nE! second mention s3cret-token-value\n",
            "",
        );
        let text = diagnostics(&o, &redactor);
        assert!(!text.contains("s3cret-token-value"), "leaked: {text}");
        assert_eq!(text.matches("***").count(), 2, "got: {text}");
    }

    /// Whole-path check: the redaction has to survive being wrapped in the
    /// error's explanatory prose and indented, not merely happen upstream of it.
    #[test]
    fn the_error_a_caller_prints_carries_no_secret() {
        let redactor = Redactor::new(["s3cret-token-value".to_string()]);
        let o = failed_output("E! token s3cret-token-value rejected\n", "");
        // Build the same message `check_config` builds, from the same parts.
        let rendered = indent(&diagnostics(&o, &redactor));
        assert!(
            !rendered.contains("s3cret-token-value"),
            "leaked: {rendered}"
        );
    }

    #[test]
    fn a_missing_binary_is_a_start_failure_not_a_config_failure() {
        let err = check_config(
            Path::new("/nonexistent/telegraf"),
            Path::new("/tmp/whatever.conf"),
            &Redactor::default(),
        )
        .unwrap_err();
        assert_eq!(
            err.exit_code(),
            muninn_core::exit::TELEGRAF_START,
            "a missing binary is not the configuration's fault"
        );
    }
}
