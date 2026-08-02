//! Secret values, and the type that stops them being printed.
//!
//! Every credential muninn handles is read from a file and wrapped in
//! [`Secret`]. The wrapper's `Debug` and `Display` render `***`; the real value
//! is reachable only through [`Secret::expose`].
//!
//! That is the whole point. muninn logs structured events, and
//! `tracing::debug!(?config)` somewhere down the line must not be able to print
//! an InfluxDB token. Making redaction a property of the type rather than a rule
//! people remember means the compiler is on the reviewer's side: to leak a
//! secret you have to write `.expose()`, which is one grep away.

use std::fmt;
use std::path::Path;

use crate::error::{MuninnError, Result};

/// A credential read from a file.
///
/// ```
/// use muninn_core::secret::Secret;
/// let s = Secret::from_value("hunter2");
/// assert_eq!(format!("{s}"), "***");
/// assert_eq!(format!("{s:?}"), "Secret(***)");
/// assert_eq!(s.expose(), "hunter2");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Read a secret from `path`.
    ///
    /// Missing, unreadable and empty are three distinct errors, because the fix
    /// differs: a missing file is usually a wrong mount, an unreadable one is
    /// usually permissions, and an empty one is usually a secret that failed to
    /// be written by whatever produced it.
    ///
    /// Trailing whitespace is stripped. `echo "token" > file` appends a newline,
    /// and a token with a trailing `\n` fails authentication in a way that looks
    /// like a wrong token — an hour of debugging for one invisible byte.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let display = path.display().to_string();

        let raw = std::fs::read_to_string(path).map_err(|e| MuninnError::Secret {
            path: display.clone(),
            message: match e.kind() {
                std::io::ErrorKind::NotFound => "file does not exist".to_string(),
                std::io::ErrorKind::PermissionDenied => "file is not readable".to_string(),
                // Not `{e}` verbatim for the common cases above: the OS message
                // ("No such file or directory (os error 2)") is noisier than the
                // one thing the operator needs to know.
                _ => e.to_string(),
            },
        })?;

        let trimmed = raw.trim();
        if trimmed.is_empty() {
            // Fail closed. An empty secret file is never intent — and treating
            // it as "no credential configured" would silently downgrade an
            // authenticated connection to an unauthenticated one.
            return Err(MuninnError::Secret {
                path: display,
                message: "file is empty".to_string(),
            });
        }

        Ok(Secret(trimmed.to_string()))
    }

    /// Wrap a value directly. For tests and for values that never came from a
    /// file; production credentials use [`Secret::from_file`].
    pub fn from_value(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// The real value.
    ///
    /// Named to be conspicuous in review and greppable in audit. It is called in
    /// exactly one place in production code: the Telegraf renderer, writing the
    /// ephemeral configuration Telegraf reads.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn file_with(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{content}").unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn reads_a_secret_from_a_file() {
        let f = file_with("s3cret-token");
        assert_eq!(
            Secret::from_file(f.path()).unwrap().expose(),
            "s3cret-token"
        );
    }

    /// `echo "token" > file` is how most people write one, and the newline it
    /// appends fails authentication in a way that looks like a wrong token.
    #[test]
    fn strips_the_trailing_newline_echo_leaves_behind() {
        let f = file_with("s3cret-token\n");
        assert_eq!(
            Secret::from_file(f.path()).unwrap().expose(),
            "s3cret-token"
        );
    }

    #[test]
    fn strips_surrounding_whitespace() {
        let f = file_with("  \t s3cret-token \r\n ");
        assert_eq!(
            Secret::from_file(f.path()).unwrap().expose(),
            "s3cret-token"
        );
    }

    /// Internal whitespace is part of the value — a passphrase may contain
    /// spaces, and trimming those would corrupt it.
    #[test]
    fn keeps_whitespace_inside_the_value() {
        let f = file_with("  correct horse battery staple  ");
        assert_eq!(
            Secret::from_file(f.path()).unwrap().expose(),
            "correct horse battery staple"
        );
    }

    #[test]
    fn missing_file_is_an_error_naming_the_path() {
        let err = Secret::from_file("/nonexistent/muninn-token-xyz").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("muninn-token-xyz"), "got: {msg}");
        assert!(msg.contains("does not exist"), "got: {msg}");
        assert_eq!(err.exit_code(), crate::exit::SECRET);
    }

    /// An empty file must fail rather than yield an empty credential: the
    /// operator asked for authentication, so proceeding without it is the worst
    /// available fallback.
    #[test]
    fn empty_file_is_an_error_not_an_empty_secret() {
        let f = file_with("");
        let err = Secret::from_file(f.path()).unwrap_err();
        assert!(err.to_string().contains("empty"), "got: {err}");
    }

    /// Whitespace-only is empty for this purpose — a file containing "\n" is
    /// what a failed write leaves behind.
    #[test]
    fn whitespace_only_file_counts_as_empty() {
        let f = file_with("   \n\t  \n");
        assert!(Secret::from_file(f.path()).is_err());
    }

    // ── Redaction ───────────────────────────────────────────────────────────
    // These assert the value is ABSENT, not merely that "***" is present.
    // Asserting on the mask would pass for a format like "***(hunter2)".

    #[test]
    fn display_hides_the_value() {
        let s = Secret::from_value("hunter2");
        let out = format!("{s}");
        assert!(!out.contains("hunter2"), "leaked: {out}");
        assert_eq!(out, "***");
    }

    #[test]
    fn debug_hides_the_value() {
        let s = Secret::from_value("hunter2");
        let out = format!("{s:?}");
        assert!(!out.contains("hunter2"), "leaked: {out}");
        assert_eq!(out, "Secret(***)");
    }

    /// The realistic leak: a secret nested in a larger struct that someone
    /// derives `Debug` on and logs with `?config`.
    #[test]
    fn debug_hides_the_value_when_nested_in_another_struct() {
        #[derive(Debug)]
        #[allow(dead_code)] // constructed only to be formatted
        struct Output {
            url: String,
            token: Secret,
        }
        let o = Output {
            url: "https://influx.example".into(),
            token: Secret::from_value("hunter2"),
        };
        let out = format!("{o:?}");
        assert!(
            !out.contains("hunter2"),
            "leaked through a parent struct: {out}"
        );
        assert!(
            out.contains("https://influx.example"),
            "non-secrets should still show"
        );
    }

    #[test]
    fn debug_hides_the_value_in_a_collection() {
        let v = vec![
            Secret::from_value("hunter2"),
            Secret::from_value("swordfish"),
        ];
        let out = format!("{v:?}");
        assert!(
            !out.contains("hunter2") && !out.contains("swordfish"),
            "leaked: {out}"
        );
    }

    /// A secret error must name the path and never the contents — the whole
    /// reason `MuninnError::Secret` has no field that could hold a value.
    #[test]
    fn errors_never_carry_the_secret_value() {
        let f = file_with("");
        let err = Secret::from_file(f.path()).unwrap_err();
        assert!(!err.to_string().contains("hunter2"));
        // And the path, which is safe and necessary, is present.
        assert!(err.to_string().contains(&f.path().display().to_string()));
    }
}
