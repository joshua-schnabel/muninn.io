//! Human-readable durations: `30s`, `5m`, `1h`.
//!
//! Two jobs, and the second is easy to overlook.
//!
//! **Parsing.** The YAML says `30s`, not `30`. Seconds-as-integer reads badly at
//! `interval: 3600` and invites unit mistakes; a suffix is unambiguous.
//!
//! **Rendering back out, for Go.** Telegraf parses durations with Go's
//! `time.ParseDuration`, which accepts `1m30s` but *not* `1m 30s` — and
//! humantime's `Display` produces the spaced form. Emitting it would generate a
//! configuration Telegraf rejects, for a value the operator wrote correctly. So
//! [`ConfigDuration::as_telegraf`] never uses `Display`; see its documentation.

use std::fmt;
use std::time::Duration;

use serde::Deserialize;

/// A duration written as `30s`, `5m`, `1h30m`, `500ms`.
///
/// ```
/// use muninn_core::duration::ConfigDuration;
/// let d: ConfigDuration = serde_yaml_ng::from_str("30s").unwrap();
/// assert_eq!(d.as_secs(), 30);
/// assert_eq!(d.as_telegraf(), "30s");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(transparent)]
pub struct ConfigDuration(#[serde(with = "humantime_serde")] Duration);

impl ConfigDuration {
    pub const fn new(inner: Duration) -> Self {
        ConfigDuration(inner)
    }

    pub const fn from_secs(secs: u64) -> Self {
        ConfigDuration(Duration::from_secs(secs))
    }

    pub const fn inner(&self) -> Duration {
        self.0
    }

    pub const fn as_secs(&self) -> u64 {
        self.0.as_secs()
    }

    pub const fn as_millis(&self) -> u128 {
        self.0.as_millis()
    }

    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    /// Render for a Telegraf configuration file.
    ///
    /// Deliberately not `Display`. Telegraf uses Go's `time.ParseDuration`,
    /// which rejects the space-separated form humantime produces: 90 seconds is
    /// `1m 30s` to humantime and must be `1m30s` or `90s` to Go.
    ///
    /// This emits whole seconds where it can (`90s`) and milliseconds otherwise
    /// (`1500ms`). Both are unambiguous to Go, and a single unit means there is
    /// no composition to get wrong. Sub-millisecond precision is discarded — no
    /// muninn setting is meaningful below a millisecond, and a value that small
    /// is rejected during validation anyway.
    pub fn as_telegraf(&self) -> String {
        let millis = self.0.as_millis();
        if millis.is_multiple_of(1000) {
            format!("{}s", millis / 1000)
        } else {
            format!("{millis}ms")
        }
    }
}

impl From<Duration> for ConfigDuration {
    fn from(d: Duration) -> Self {
        ConfigDuration(d)
    }
}

/// Human-facing rendering, for error messages. Not for Telegraf — see
/// [`ConfigDuration::as_telegraf`].
impl fmt::Display for ConfigDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", humantime::format_duration(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> ConfigDuration {
        serde_yaml_ng::from_str(s).expect("should parse")
    }

    #[test]
    fn parses_the_units_the_schema_documents() {
        assert_eq!(parse("30s").as_secs(), 30);
        assert_eq!(parse("5m").as_secs(), 300);
        assert_eq!(parse("1h").as_secs(), 3600);
        assert_eq!(parse("500ms").as_millis(), 500);
    }

    #[test]
    fn parses_compound_durations() {
        assert_eq!(parse("1h30m").as_secs(), 5400);
        assert_eq!(parse("2m 30s").as_secs(), 150);
    }

    /// A bare number is rejected: `interval: 30` gives no clue whether the
    /// author meant seconds, minutes or milliseconds.
    #[test]
    fn rejects_a_bare_number() {
        assert!(serde_yaml_ng::from_str::<ConfigDuration>("30").is_err());
    }

    #[test]
    fn rejects_nonsense() {
        for s in ["\"soon\"", "\"30x\"", "\"-30s\"", "\"\""] {
            assert!(
                serde_yaml_ng::from_str::<ConfigDuration>(s).is_err(),
                "{s} should not parse"
            );
        }
    }

    /// Zero parses — rejecting it belongs to validation, where the field name is
    /// known and the message can say which key is wrong.
    #[test]
    fn zero_parses_and_is_recognisable() {
        assert!(parse("0s").is_zero());
    }

    // ── Telegraf rendering ──────────────────────────────────────────────────

    /// The trap this method exists for. Go's time.ParseDuration accepts "1m30s"
    /// and rejects "1m 30s", which is exactly what humantime's Display emits.
    #[test]
    fn telegraf_rendering_never_contains_a_space() {
        for s in ["30s", "5m", "1h", "1h30m", "90s", "2m 30s", "1500ms"] {
            let rendered = parse(s).as_telegraf();
            assert!(
                !rendered.contains(' '),
                "{s} rendered as {rendered:?}, which Go cannot parse"
            );
        }
    }

    #[test]
    fn telegraf_rendering_uses_a_single_unit() {
        assert_eq!(parse("30s").as_telegraf(), "30s");
        assert_eq!(parse("5m").as_telegraf(), "300s");
        assert_eq!(parse("1h").as_telegraf(), "3600s");
        assert_eq!(parse("1h30m").as_telegraf(), "5400s");
        assert_eq!(parse("1500ms").as_telegraf(), "1500ms");
        assert_eq!(parse("500ms").as_telegraf(), "500ms");
    }

    /// Rendering must be a function of the value, not of how it was written —
    /// otherwise the generated configuration would depend on the author's
    /// spelling and the determinism guarantee would not hold.
    #[test]
    fn equal_durations_render_identically_however_they_were_written() {
        assert_eq!(parse("1h").as_telegraf(), parse("60m").as_telegraf());
        assert_eq!(parse("90s").as_telegraf(), parse("1m30s").as_telegraf());
        assert_eq!(parse("1000ms").as_telegraf(), parse("1s").as_telegraf());
    }

    /// Display is for humans and may use the spaced form; the two renderings are
    /// deliberately different and this pins that down.
    #[test]
    fn display_is_human_facing_and_telegraf_rendering_is_not() {
        let d = parse("90s");
        assert_eq!(d.to_string(), "1m 30s");
        assert_eq!(d.as_telegraf(), "90s");
    }
}
