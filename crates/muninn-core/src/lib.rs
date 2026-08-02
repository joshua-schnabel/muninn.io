//! Configuration model, secret handling and error types for muninn.io.
//!
//! This crate owns everything between "an operator wrote a YAML file" and "a
//! validated, normalised model the rest of the program can render from". It
//! knows nothing about Telegraf.
//!
//! # Planned layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | `config::model` | `ConfigV1` — the serde target, mirroring the YAML 1:1 |
//! | `config::loader` | Read the file, deserialise, apply CLI/ENV precedence |
//! | `config::validation` | Semantic rules: port collisions, output presence, module coherence |
//! | `config::normalised` | Version-independent internal model; `ConfigV1` migrates into it |
//! | `secrets` | Read secret files: exists, readable, non-empty, trailing newline stripped |
//! | `duration` | `30s` / `5m` / `1h` parsing plus the "is this value sane" rules |
//! | `error` | `MuninnError` and the stable exit-code mapping |
//!
//! # Two invariants this crate exists to hold
//!
//! **Unknown keys are errors.** Every config struct carries
//! `#[serde(deny_unknown_fields)]`. A typo'd key must fail the load rather than
//! be silently ignored — a monitoring agent that quietly drops half its
//! configuration is worse than one that refuses to start.
//!
//! **A secret value never leaves this crate as a printable string.** Secrets are
//! wrapped in a type whose `Debug` and `Display` render `"***"`; the real value
//! is reachable only through an explicit accessor, called in exactly one place
//! (the Telegraf renderer). Errors name the *path* of a secret file, never its
//! contents.
//!
//! Implementation lands in WP2 — see `docs/roadmap.md`. The exception is
//! [`exit`], which ships with the design package: the exit codes are part of the
//! documented operator contract, and pinning them down before any call site
//! exists is how a program avoids growing three numbers for "bad config".

pub mod exit;
