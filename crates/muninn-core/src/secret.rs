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

/// The mask [`Redactor`] substitutes. The same one [`Secret`] renders, so a
/// redacted log line reads like every other place a secret is suppressed.
pub const MASK: &str = "***";

/// Values shorter than this are not redacted.
///
/// A short secret would match constantly — `abc` inside `abcdefg`, a two-letter
/// value inside almost any word — and a log line shot through with `***` is
/// less readable *and* less safe, because nobody reads it. Any credential worth
/// protecting is longer than this; a shorter one is a configuration mistake the
/// operator should hear about rather than a value to defend.
const MIN_REDACTABLE_LEN: usize = 8;

/// Removes known secret values from text muninn did not write.
///
/// # Why this exists at all
///
/// [`Secret`]'s redaction is a property of the *type*: to print one you have to
/// call [`Secret::expose`], and there is exactly one such call. That argument
/// covers everything muninn formats itself — and covers nothing at all in text
/// that arrives already formatted from somewhere else.
///
/// Telegraf's stdout and stderr are exactly that. muninn re-emits them through
/// its own logger, and the configuration Telegraf is reading holds **resolved
/// secrets** ([ADR-0003](../../../docs/adr/0003-ephemeral-generated-config.md)).
/// Whether a Telegraf diagnostic ever quotes a configuration value is a
/// property of Telegraf, not of muninn — an assumption about software this
/// project does not control, at the point where logs leave the container. This
/// closes it instead of resting on it, the same way the `image_updates` Docker
/// client refuses a control character it has been told cannot arrive.
///
/// # What it does not do
///
/// It matches literal values. A secret that Telegraf reformats — URL-encoded,
/// truncated, base64'd — passes through. That is a real limit and the reason
/// this is defence in depth rather than a guarantee: the load-bearing control
/// is still that the generated configuration lives on a tmpfs and is never
/// mounted out.
#[derive(Clone, Default)]
pub struct Redactor {
    /// Longest first, so an overlapping pair cannot leave a fragment of the
    /// longer value behind after the shorter one has been replaced.
    values: Vec<String>,
}

impl Redactor {
    /// Build from every secret that reached the generated configuration.
    pub fn new(secrets: impl IntoIterator<Item = String>) -> Self {
        let mut values: Vec<String> = secrets
            .into_iter()
            .filter(|s| s.len() >= MIN_REDACTABLE_LEN)
            .collect();
        values.sort_by_key(|b| std::cmp::Reverse(b.len()));
        values.dedup();
        Redactor { values }
    }

    /// Whether this redactor would change anything. Lets a caller skip the
    /// work — and the allocation — when nothing is configured.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// `line` with every known secret replaced by [`MASK`].
    ///
    /// Returns the input unchanged, without allocating, when there is nothing
    /// to do — which is the overwhelmingly common case for a log line.
    pub fn apply<'a>(&self, line: &'a str) -> std::borrow::Cow<'a, str> {
        if self.values.is_empty() || !self.values.iter().any(|v| line.contains(v.as_str())) {
            return std::borrow::Cow::Borrowed(line);
        }
        let mut out = line.to_string();
        for value in &self.values {
            if out.contains(value.as_str()) {
                out = out.replace(value.as_str(), MASK);
            }
        }
        std::borrow::Cow::Owned(out)
    }
}

/// Never render the values it holds — the whole point of the type.
impl fmt::Debug for Redactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Redactor({} values)", self.values.len())
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

    // ── Redactor ────────────────────────────────────────────────────────────

    fn redactor(values: &[&str]) -> Redactor {
        Redactor::new(values.iter().map(|s| s.to_string()))
    }

    /// The case this exists for: a line muninn did not write, quoting a value
    /// muninn resolved.
    #[test]
    fn a_secret_quoted_by_a_child_process_is_masked() {
        let r = redactor(&["s3cret-token-value"]);
        let out = r.apply("E! [outputs.influxdb_v2] token s3cret-token-value rejected");
        assert!(!out.contains("s3cret-token-value"), "leaked: {out}");
        assert_eq!(out, "E! [outputs.influxdb_v2] token *** rejected");
    }

    #[test]
    fn a_line_without_a_secret_is_returned_untouched() {
        let r = redactor(&["s3cret-token-value"]);
        let line = "I! [agent] Config: Interval:10s, Quiet:false";
        assert_eq!(r.apply(line), line);
        // Borrowed, not rebuilt: the common case must not allocate.
        assert!(matches!(r.apply(line), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn every_occurrence_on_a_line_is_masked() {
        let r = redactor(&["longenoughsecret"]);
        let out = r.apply("longenoughsecret and again longenoughsecret");
        assert_eq!(out, "*** and again ***");
    }

    #[test]
    fn several_secrets_are_all_masked() {
        let r = redactor(&["influx-token-aaa", "prometheus-pass-bbb"]);
        let out = r.apply("influx-token-aaa / prometheus-pass-bbb");
        assert_eq!(out, "*** / ***");
    }

    /// Longest first: replacing the short one first would leave the tail of the
    /// long one — `...-extended` — sitting in the log.
    #[test]
    fn an_overlapping_pair_cannot_leave_a_fragment_behind() {
        let r = redactor(&["secret-value-x", "secret-value-x-extended"]);
        let out = r.apply("token secret-value-x-extended here");
        assert!(!out.contains("extended"), "left a fragment: {out}");
        assert_eq!(out, "token *** here");
    }

    /// A short value would match inside ordinary words and turn every log line
    /// into noise — which is less safe, because an unreadable log is unread.
    #[test]
    fn values_too_short_to_match_safely_are_ignored() {
        let r = redactor(&["abc"]);
        assert!(r.is_empty());
        assert_eq!(r.apply("abcdef"), "abcdef");
    }

    #[test]
    fn an_empty_redactor_changes_nothing() {
        let r = Redactor::default();
        assert!(r.is_empty());
        assert_eq!(r.apply("anything at all"), "anything at all");
    }

    /// The redactor holds the values it is meant to hide; formatting it must
    /// not undo that.
    #[test]
    fn debug_does_not_render_the_values_it_holds() {
        let r = redactor(&["s3cret-token-value"]);
        let out = format!("{r:?}");
        assert!(!out.contains("s3cret-token-value"), "leaked: {out}");
        assert_eq!(out, "Redactor(1 values)");
    }
}
