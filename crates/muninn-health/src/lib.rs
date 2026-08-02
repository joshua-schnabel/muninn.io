//! The health server: liveness, readiness, status and muninn's own metrics.
//!
//! # Endpoints
//!
//! | Route | Meaning |
//! |---|---|
//! | `GET /health/live` | muninn's own event loop is responsive. Does **not** depend on Telegraf |
//! | `GET /health/ready` | Config loaded and validated, Telegraf config generated and checked, Telegraf running, listeners up. `503` otherwise |
//! | `GET /status` | Non-sensitive diagnostics: versions, uptime, enabled modules and outputs, last Telegraf exit, state |
//! | `GET /metrics` | muninn's own operational metrics (see below) |
//!
//! Liveness and readiness are deliberately different questions. A brief
//! InfluxDB outage must not fail liveness — muninn is fine, the network is not —
//! but a dead Telegraf process must fail readiness immediately, because at that
//! point nothing is being collected at all.
//!
//! # Why self-metrics live here and not in Telegraf
//!
//! `muninn_telegraf_running` and `muninn_ready` are worth reading precisely when
//! Telegraf is **not** running. Exposing them through
//! `outputs.prometheus_client` would mean they vanish in exactly the failure
//! they exist to report. So they are served by this crate's listener, which
//! shares muninn's own lifecycle.
//!
//! This means a deployment has two Prometheus endpoints — Telegraf's `:9273`
//! for host metrics, muninn's health port for agent metrics. That is a
//! documented trade, not an accident; see `docs/adr/0012-self-metrics-on-health-server.md`.
//!
//! # Planned layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | `server` | Axum router, listener, graceful shutdown |
//! | `state` | Shared supervisor state the handlers read; the supervisor is the only writer |
//! | `metrics` | Prometheus text rendering for the `muninn_*` families |
//!
//! Implementation lands in WP7 — see `docs/roadmap.md`.
