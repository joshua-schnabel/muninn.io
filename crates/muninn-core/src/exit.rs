//! Stable process exit codes.
//!
//! These are part of muninn's public contract: operators write alerting rules
//! and container restart policies against them, so a code may gain meaning in a
//! minor release but must never change meaning. `docs/supervision.md` documents
//! each one for humans; this module is the single source of truth the code uses.
//!
//! They exist already in the design package, ahead of the implementation, for
//! one reason: an exit code invented ad hoc at the call site is how a program
//! ends up with three different numbers for "the config was bad".

/// Clean shutdown — a stop signal was received and Telegraf exited normally.
pub const OK: u8 = 0;

/// Bad command line: unknown flag, missing argument, unusable `--config` path.
pub const CLI: u8 = 2;

/// The configuration could not be loaded or violates a rule: unreadable file,
/// invalid YAML, unknown key, missing or unsupported schema version, no output
/// enabled, port collision, incoherent module options.
pub const CONFIG: u8 = 10;

/// A required secret file is missing, unreadable, or empty. Separate from
/// [`CONFIG`] because the fix is different — the config names a path that is
/// fine, the *mount* behind it is not.
pub const SECRET: u8 = 11;

/// A runtime precondition for an enabled module is absent: a host path that was
/// not mounted, an inaccessible Docker socket, an unsupported host OS.
pub const RUNTIME: u8 = 12;

/// The generated Telegraf configuration was rejected by `telegraf config check`.
/// This is a muninn bug or a Telegraf version mismatch, never operator error —
/// the operator never writes TOML.
pub const TELEGRAF_CONFIG: u8 = 20;

/// Telegraf did not reach a running state within `runtime.telegraf_start_timeout`,
/// or the binary is missing or reports an unexpected version.
pub const TELEGRAF_START: u8 = 21;

/// Telegraf exited on its own while muninn was supervising it. The container
/// orchestrator is expected to restart the container; muninn deliberately does
/// not loop internally, so a crashing Telegraf never hides inside a
/// seemingly-healthy container.
pub const TELEGRAF_EXITED: u8 = 22;

/// An internal invariant broke. Always a muninn bug — worth a report.
pub const INTERNAL: u8 = 30;

#[cfg(test)]
mod tests {
    use super::*;

    /// The values are a contract, so the test states them literally rather than
    /// referring to the constants a second time. Changing a number must require
    /// changing this test, which is the point.
    #[test]
    fn exit_codes_have_their_documented_values() {
        assert_eq!(OK, 0);
        assert_eq!(CLI, 2);
        assert_eq!(CONFIG, 10);
        assert_eq!(SECRET, 11);
        assert_eq!(RUNTIME, 12);
        assert_eq!(TELEGRAF_CONFIG, 20);
        assert_eq!(TELEGRAF_START, 21);
        assert_eq!(TELEGRAF_EXITED, 22);
        assert_eq!(INTERNAL, 30);
    }

    /// Two failure modes sharing a code would make an alert rule ambiguous.
    #[test]
    fn exit_codes_are_distinct() {
        let all = [
            OK,
            CLI,
            CONFIG,
            SECRET,
            RUNTIME,
            TELEGRAF_CONFIG,
            TELEGRAF_START,
            TELEGRAF_EXITED,
            INTERNAL,
        ];
        let mut seen = std::collections::HashSet::new();
        for code in all {
            assert!(seen.insert(code), "exit code {code} is used twice");
        }
    }

    /// 1 is left free on purpose: it is what a panic, an unhandled error or a
    /// shell wrapper produces, and it must stay distinguishable from every
    /// deliberate exit muninn performs.
    #[test]
    fn one_is_not_assigned() {
        let all = [
            OK,
            CLI,
            CONFIG,
            SECRET,
            RUNTIME,
            TELEGRAF_CONFIG,
            TELEGRAF_START,
            TELEGRAF_EXITED,
            INTERNAL,
        ];
        assert!(
            !all.contains(&1),
            "1 must stay reserved for unexpected failure"
        );
    }
}
