//! The health server: liveness, readiness, status and muninn's own metrics.
//!
//! | Route | Meaning |
//! |---|---|
//! | `GET /health/live` | muninn's own loop is responsive. Does **not** depend on Telegraf |
//! | `GET /health/ready` | Config loaded and validated, Telegraf running. `503` otherwise |
//! | `GET /status` | Non-sensitive diagnostics: versions, uptime, enabled modules, last Telegraf exit |
//! | `GET /metrics` | muninn's own operational metrics |
//!
//! Liveness and readiness are deliberately different questions. A brief InfluxDB
//! outage must not fail liveness — muninn is fine, the network is not — but a
//! dead Telegraf must fail readiness immediately, because at that point nothing
//! is being collected.
//!
//! # Why self-metrics live here and not in Telegraf
//!
//! `muninn_telegraf_running` and `muninn_ready` are worth reading precisely when
//! Telegraf is **not** running. Exposing them through
//! `outputs.prometheus_client` would mean they vanish in exactly the failure
//! they exist to report.
//!
//! This means a deployment has two Prometheus endpoints — Telegraf's `:9273` for
//! host metrics, muninn's health port for agent metrics. That is a documented
//! trade, not an accident; see
//! `docs/adr/0012-self-metrics-on-health-server.md`.
//!
//! # Who writes what
//!
//! The supervisor is the only writer. The handlers only read, and never hold a
//! lock across an await, so a slow or stalled client cannot delay a state
//! transition — least of all the shutdown it is meant to observe.

pub mod metrics;
mod serve;
pub mod server;
pub mod state;

pub use server::{ServerState, bind, serve, serve_on};
pub use state::{Details, HealthState, ModuleCheck, State};
