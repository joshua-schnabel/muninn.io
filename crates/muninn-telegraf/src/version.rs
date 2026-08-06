//! Finding the Telegraf binary and confirming it is the one muninn expects.
//!
//! muninn generates configuration against a specific plugin surface. Option
//! names, defaults and semantics move between minor releases —
//! `inputs.system.include` is recent, and `skip_processors_after_aggregators`
//! changes its default in 1.40 — so running a configuration generated for one
//! version against another is not a supported combination. It would usually
//! work, which is the problem: the failure mode is a silently different meaning
//! rather than an error.
//!
//! So the version is checked at startup, and a mismatch refuses to start.
//! See `docs/adr/0011-telegraf-pinning.md`.

use std::path::{Path, PathBuf};
use std::process::Command;

use muninn_core::error::{MuninnError, Result};

/// The Telegraf version this build generates configuration for.
///
/// Set at build time from the workspace; the Dockerfile installs the matching
/// binary and verifies its checksum. Keep the two in step — that is the whole
/// point of pinning.
pub const EXPECTED_VERSION: &str = "1.39.2";

/// Where the binary lives in muninn's image.
pub const DEFAULT_BINARY: &str = "/usr/local/bin/telegraf";

/// Locate the Telegraf binary.
///
/// `MUNINN_TELEGRAF_BIN` overrides the default. That exists for tests and for
/// running muninn outside its image; in the container the default is correct and
/// the variable is unset.
pub fn binary_path() -> PathBuf {
    resolve_binary_path(std::env::var_os("MUNINN_TELEGRAF_BIN"))
}

/// The decision behind [`binary_path`], separated from reading the environment.
///
/// Taking the override as an argument is what makes this testable: the
/// environment is process-global, so a test that set or unset the variable would
/// depend on the ambient one and race every other test in the binary. That is
/// not hypothetical — the first version of this test asserted the default and
/// then failed inside the Linux container, where the variable is legitimately
/// set to point at the Telegraf under test.
fn resolve_binary_path(override_value: Option<std::ffi::OsString>) -> PathBuf {
    match override_value {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        // An empty override is a deployment mistake — usually an unset variable
        // expanded by a shell — and silently becoming "" would produce a
        // baffling "cannot run ''".
        _ => PathBuf::from(DEFAULT_BINARY),
    }
}

/// Ask the binary which version it is.
pub fn query(binary: &Path) -> Result<String> {
    let output = Command::new(binary).arg("version").output().map_err(|e| {
        MuninnError::TelegrafStart(match e.kind() {
            std::io::ErrorKind::NotFound => format!(
                "no Telegraf binary at '{}'. In muninn's image it is at {DEFAULT_BINARY}; \
                 outside it, set MUNINN_TELEGRAF_BIN",
                binary.display()
            ),
            std::io::ErrorKind::PermissionDenied => {
                format!("'{}' is not executable", binary.display())
            }
            _ => format!("cannot run '{}': {e}", binary.display()),
        })
    })?;

    // Telegraf prints its version on stdout and exits 0. Both streams are read
    // because a build that logs a warning first would otherwise look empty.
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr)
    } else {
        String::from_utf8_lossy(&output.stdout)
    };

    parse(&text).ok_or_else(|| {
        MuninnError::TelegrafStart(format!(
            "'{}' did not report a recognisable version; it printed: {}",
            binary.display(),
            text.trim().chars().take(120).collect::<String>()
        ))
    })
}

/// Extract the version from `telegraf version` output.
///
/// The format is `Telegraf 1.39.2 (git: HEAD@e8162a94)`. Only the number is
/// taken: the git suffix varies between builds of the same release, and pinning
/// on it would reject a perfectly good rebuild.
pub fn parse(output: &str) -> Option<String> {
    let token = output
        .split_whitespace()
        .skip_while(|w| !w.eq_ignore_ascii_case("telegraf"))
        .nth(1)?;

    // Guard against picking up a word from an unexpected line: a version starts
    // with a digit and contains only digits, dots and pre-release characters.
    let looks_like_a_version = token.chars().next().is_some_and(|c| c.is_ascii_digit())
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');

    looks_like_a_version.then(|| token.to_string())
}

/// Confirm the binary is the version muninn generates configuration for.
pub fn check(binary: &Path) -> Result<String> {
    let found = query(binary)?;
    if found == EXPECTED_VERSION {
        return Ok(found);
    }
    Err(MuninnError::TelegrafStart(format!(
        "Telegraf version mismatch: '{}' is {found}, but this muninn generates configuration \
         for {EXPECTED_VERSION}. Plugin options move between releases, so the two are not \
         interchangeable — use the muninn image, which ships the matching binary",
        binary.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_output_format() {
        assert_eq!(
            parse("Telegraf 1.39.2 (git: HEAD@e8162a94)").as_deref(),
            Some("1.39.2")
        );
    }

    #[test]
    fn parses_a_version_with_a_pre_release_suffix() {
        assert_eq!(
            parse("Telegraf 1.40.0-rc1 (git: master@abc123)").as_deref(),
            Some("1.40.0-rc1")
        );
    }

    #[test]
    fn tolerates_a_leading_log_line() {
        let output = "2026-08-02T10:00:00Z I! some notice\nTelegraf 1.39.2 (git: HEAD@e8162a94)";
        assert_eq!(parse(output).as_deref(), Some("1.39.2"));
    }

    /// Returning `None` rather than a wrong guess matters: the caller turns
    /// `None` into "did not report a recognisable version", which is honest,
    /// whereas a guess would be compared against the pin and produce a confident
    /// mismatch about nothing.
    #[test]
    fn refuses_to_guess_at_unrecognised_output() {
        for output in [
            "",
            "command not found",
            "Telegraf",
            "Telegraf unknown",
            "some other program 1.2.3",
        ] {
            assert_eq!(parse(output), None, "{output:?} should not parse");
        }
    }

    #[test]
    fn a_missing_binary_says_where_it_should_be() {
        let err = query(Path::new("/nonexistent/telegraf")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("MUNINN_TELEGRAF_BIN"), "got: {msg}");
        assert_eq!(err.exit_code(), muninn_core::exit::TELEGRAF_START);
    }

    /// The default is the container path; the override is what makes running
    /// outside the image possible at all.
    #[test]
    fn the_binary_path_falls_back_to_the_container_default() {
        assert_eq!(resolve_binary_path(None), PathBuf::from(DEFAULT_BINARY));
    }

    #[test]
    fn the_binary_path_honours_an_override() {
        assert_eq!(
            resolve_binary_path(Some("/opt/telegraf".into())),
            PathBuf::from("/opt/telegraf")
        );
    }

    /// Usually an unset shell variable that expanded to nothing. Taking it
    /// literally would produce a baffling "cannot run ''".
    #[test]
    fn an_empty_override_falls_back_rather_than_becoming_an_empty_path() {
        assert_eq!(
            resolve_binary_path(Some("".into())),
            PathBuf::from(DEFAULT_BINARY)
        );
    }
}
