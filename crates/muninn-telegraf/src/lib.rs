//! Typed Telegraf configuration model, deterministic TOML renderer, and control
//! of the Telegraf child process.
//!
//! # Planned layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | `model` | `TelegrafConfig`, `PluginInstance` — a typed tree, never strings |
//! | `renderer` | The one place that turns the tree into TOML, and the one place that escapes |
//! | `validator` | Runs `telegraf config check` against a rendered file |
//! | `process` | Spawn Telegraf, own its PID, capture stdout/stderr, forward signals |
//! | `version` | Compare the runtime binary against the version pinned at build time |
//!
//! # Why the renderer does not sort fields
//!
//! The obvious way to make TOML output deterministic is to sort keys. That is
//! wrong here. Telegraf's `CONFIGURATION.md` states that `tagpass` / `tagdrop`
//! sub-tables *"must be defined at the **end** of the plugin definition"* —
//! otherwise every option written after them is parsed as part of the table
//! instead of the plugin. Alphabetical order would put `[inputs.disk.tagdrop]`
//! before `ignore_fs` and silently produce a config that parses but does not do
//! what it says.
//!
//! So determinism comes from a *declared* order instead: each
//! [`PluginInstance`] keeps its scalars in declaration order and its sub-tables
//! separately, and the renderer always emits scalars first, sub-tables last.
//! Instances are ordered by an explicit rank plus plugin name. Same input,
//! byte-identical output — and valid Telegraf.
//!
//! See `docs/adr/0007-tagdrop-and-render-order.md`.
//!
//! # Why validation uses `config check`, not `--test`
//!
//! `telegraf config check --config <file>` loads the configuration and
//! initialises the plugins **without starting them**. `--test` actually runs a
//! collection, which means service inputs bind their ports — a validation step
//! that competes with the real process for `:9273` is not a validation step.
//!
//! See `docs/adr/0006-validate-with-config-check.md`.
//!
//! Implementation lands in WP3 and WP6 — see `docs/roadmap.md`.
