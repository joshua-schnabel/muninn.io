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

/// The shortest credential muninn will accept, and the shortest it can redact.
///
/// One number for both, because they are the same fact seen from two sides. A
/// short secret would match constantly — `abc` inside `abcdefg`, a two-letter
/// value inside almost any word — and a log line shot through with `***` is
/// less readable *and* less safe, because nobody reads it. So [`Redactor`]
/// cannot defend a value this short.
///
/// That used to be the end of it, and it left a hole: the redactor silently
/// skipped short values while configuration loading accepted them without a
/// word, so a valid four-character Basic Auth password reached Telegraf's
/// output unprotected. The comment here already said such a value "is a
/// configuration mistake the operator should hear about" — now
/// [`validate_file`] is what makes them hear it.
///
/// The filter in [`Redactor::new`] stays as a backstop for values that never
/// went through validation, such as [`Secret::from_value`].
pub const MIN_SECRET_LEN: usize = 8;

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
            .filter(|s| s.len() >= MIN_SECRET_LEN)
            .collect();
        values.sort_by_key(|b| std::cmp::Reverse(b.len()));
        values.dedup();
        Redactor { values }
    }

    /// The same redactor plus values that were not in the configuration model.
    ///
    /// [`crate::config::Config::redactor`] can only carry secrets the config
    /// *holds*, and not every credential is one. `modules.image_updates
    /// .registry_auth` names password **files**; the passwords are read later,
    /// by the code that sends them, and never enter the normalised model. So
    /// they were outside the redactor entirely — found by the 2026-08-12 audit
    /// as M-02, and not closable by adding a line to `redactor()`, because
    /// there is no value there to add.
    ///
    /// Rebuilt through [`Redactor::new`] rather than pushed onto `values`, so
    /// the minimum length, the longest-first order and the deduplication stay
    /// defined in exactly one place.
    #[must_use]
    pub fn extended_with(self, more: impl IntoIterator<Item = String>) -> Self {
        Redactor::new(self.values.into_iter().chain(more))
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

/// Check a configured secret file, returning warnings rather than logging them.
///
/// `field` is the configuration key, so a message can say which of several
/// credentials it means without the operator having to match paths by eye.
///
/// Three outcomes, in the order an operator can act on them:
///
/// 1. **Unreadable, missing or empty** — an error from [`Secret::from_file`],
///    naming the path.
/// 2. **Shorter than [`MIN_SECRET_LEN`]** — an error. muninn cannot redact a
///    value that short from Telegraf's output without turning every log line
///    into `***`, so accepting one would mean carrying a credential it has
///    quietly promised not to protect. Erroring here is the difference between
///    a startup failure that names the key and a token appearing in a log
///    weeks later.
/// 3. **Readable beyond its owner** — a warning, pushed onto `warnings`.
///
/// The value itself is dropped immediately; this is a check, not a resolution.
/// [`crate::config::normalised`] is still the one place secrets are read for
/// use.
///
/// # Why a warning goes here and not through `tracing`
///
/// Validation runs *before* the tracing subscriber exists — the log level to
/// initialise it with comes from the configuration being validated. Anything
/// logged at this point goes nowhere at all, and the commands that read a
/// configuration without running (`validate`, `render-config`, `check-runtime`)
/// never initialise a subscriber in the first place. The M-01 permission check
/// shipped as a `tracing::warn!` for exactly that reason and was therefore
/// discarded on every path where an operator was meant to see it (F-02). The
/// caller emits what this returns, on stderr, once it can.
pub fn validate_file(path: &str, field: &str, warnings: &mut Vec<String>) -> Result<()> {
    let secret = Secret::from_file(path)?;

    // `len()` is bytes, and deliberately: the redactor matches bytes, so bytes
    // are what decides whether it can. A short multi-byte passphrase counting
    // as long enough is the safe direction of that approximation.
    if secret.expose().len() < MIN_SECRET_LEN {
        return Err(MuninnError::Secret {
            path: path.to_string(),
            message: format!(
                "{field} holds a credential shorter than {MIN_SECRET_LEN} bytes. muninn masks \
                 known secrets in Telegraf's output, and a value this short cannot be masked \
                 without matching ordinary words in every log line — so it would travel \
                 unprotected. Use a longer credential"
            ),
        });
    }

    if let Some(warning) = permission_warning(path, field) {
        warnings.push(warning);
    }

    Ok(())
}

/// The warning for a secret file readable by anyone but its owner, if any.
///
/// A warning, not a refusal. A read-only bind mount can carry permissions the
/// operator does not control, and refusing to start over a mode bit would take
/// down a deployment that is merely untidy — the token still works. What is not
/// acceptable is saying nothing: the documentation prescribes `0600`, and until
/// M-01 nothing checked it or reported otherwise (docs/security-audit.md).
///
/// It matters more here than it would in a distroless image. muninn's runtime
/// carries a shell and a package manager because the updates module needs real
/// apt and dpkg, so "readable by anything else in the container" is a larger
/// set than it sounds.
///
/// Unix only: mode bits are the check, and there is nothing equivalent to look
/// at elsewhere. The path is named, never the contents.
#[cfg(unix)]
fn permission_warning(path: &str, field: &str) -> Option<String> {
    use std::os::unix::fs::PermissionsExt as _;

    // Best-effort: the file was just read successfully, so a stat that fails
    // says something odd about the filesystem rather than about the secret, and
    // it is not a reason to hold up the start.
    let meta = std::fs::metadata(path).ok()?;
    let mode = meta.permissions().mode() & 0o777;
    (mode & 0o077 != 0).then(|| {
        format!(
            "{field} '{path}' is mode {mode:04o} — readable beyond its owner. 0600 is expected; \
             muninn's runtime carries a shell and a package manager, so anything that achieves \
             execution in this container can read it"
        )
    })
}

/// No mode bits to look at, so nothing to report.
#[cfg(not(unix))]
fn permission_warning(_path: &str, _field: &str) -> Option<String> {
    None
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

    // ── validate_file ───────────────────────────────────────────────────────

    fn validated(content: &str) -> (Result<()>, Vec<String>) {
        let f = file_with(content);
        let mut warnings = Vec::new();
        let r = validate_file(
            &f.path().display().to_string(),
            "outputs.influxdb.token_file",
            &mut warnings,
        );
        (r, warnings)
    }

    #[test]
    fn a_credential_of_usable_length_validates_quietly() {
        let (result, warnings) = validated("s3cret-token-value");
        assert!(result.is_ok(), "got: {:?}", result.err());
        // On Windows there are no mode bits; on Unix `NamedTempFile` creates
        // 0600. Either way there is nothing to say.
        assert!(warnings.is_empty(), "unexpected: {warnings:?}");
    }

    /// The hole this closes (F-01): the redactor silently skipped values this
    /// short while loading accepted them, so a short-but-valid credential
    /// reached Telegraf's output with nothing defending it.
    #[test]
    fn a_credential_too_short_to_redact_is_refused() {
        let (result, _) = validated("tok");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("outputs.influxdb.token_file"), "got: {msg}");
        assert!(msg.contains("shorter than 8 bytes"), "got: {msg}");
        assert_eq!(err.exit_code(), crate::exit::SECRET);
    }

    /// Refusing must never quote the value it is refusing. The value here is
    /// deliberately one that appears nowhere in the message's own wording —
    /// `tok` would be found inside `token_file` and prove nothing.
    #[test]
    fn refusing_a_short_credential_does_not_print_it() {
        let (result, _) = validated("zq7");
        let msg = result.unwrap_err().to_string();
        assert!(!msg.contains("zq7"), "leaked: {msg}");
    }

    /// Exactly at the boundary is accepted: the rule is "shorter than", and an
    /// off-by-one here would reject a credential the redactor can defend.
    #[test]
    fn a_credential_of_exactly_the_minimum_length_is_accepted() {
        assert_eq!(MIN_SECRET_LEN, 8);
        let (result, _) = validated("12345678");
        assert!(result.is_ok(), "got: {:?}", result.err());
    }

    #[test]
    fn the_underlying_read_errors_still_surface() {
        let mut warnings = Vec::new();
        let err = validate_file(
            "/nonexistent/muninn-token-xyz",
            "outputs.influxdb.token_file",
            &mut warnings,
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "got: {err}");
    }

    /// A secret file readable beyond its owner is warned about, not refused —
    /// a read-only bind mount can carry permissions the operator does not
    /// control, and a token that works should not stop a deployment.
    ///
    /// This asserts the *diagnostic*, which is what M-01 claimed and F-02
    /// found was never true: the warning went through `tracing::warn!` and
    /// validation runs before any subscriber exists, so it was discarded on
    /// every path where an operator was meant to see it.
    #[cfg(unix)]
    #[test]
    fn a_secret_readable_beyond_its_owner_produces_a_warning_and_still_loads() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "s3cret-token-value").unwrap();
        std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o644)).unwrap();

        let path = f.path().display().to_string();
        let mut warnings = Vec::new();
        validate_file(&path, "outputs.influxdb.token_file", &mut warnings)
            .expect("a loose mode must not be fatal");

        assert_eq!(warnings.len(), 1, "got: {warnings:?}");
        let w = &warnings[0];
        assert!(w.contains("outputs.influxdb.token_file"), "got: {w}");
        assert!(w.contains("0644"), "the mode is the actionable part: {w}");
        assert!(w.contains(&path), "got: {w}");
        assert!(!w.contains("s3cret-token-value"), "leaked: {w}");
    }

    /// And the tight case says nothing at all.
    #[cfg(unix)]
    #[test]
    fn a_correctly_moded_secret_produces_no_warning() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "s3cret-token-value").unwrap();
        std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o600)).unwrap();

        let mut warnings = Vec::new();
        validate_file(
            &f.path().display().to_string(),
            "outputs.influxdb.token_file",
            &mut warnings,
        )
        .unwrap();
        assert!(warnings.is_empty(), "unexpected: {warnings:?}");
    }

    /// Group-readable is as much a finding as world-readable. `0640` in a mount
    /// is the shape this most often takes, and checking only `o` would miss it.
    #[cfg(unix)]
    #[test]
    fn group_readable_counts_too() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "s3cret-token-value").unwrap();
        std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o640)).unwrap();

        let mut warnings = Vec::new();
        validate_file(
            &f.path().display().to_string(),
            "outputs.influxdb.token_file",
            &mut warnings,
        )
        .unwrap();
        assert_eq!(warnings.len(), 1, "got: {warnings:?}");
        assert!(warnings[0].contains("0640"), "got: {}", warnings[0]);
    }
}
