# huginn.io — what muninn takes, and what it deliberately leaves

muninn.io was asked to build on [huginn.io](https://github.com/joshua-schnabel/huginn.io),
the same maintainer's Rust uptime monitor. This document records what was
reviewed, what is being reused, and — more usefully — what was rejected and why.
Copying a structure that does not fit the new problem is more expensive than
starting from scratch, because the mismatch is invisible until it hurts.

Reviewed at branch `dev`, roughly 11,000 lines across five crates.

## What huginn is

A Cargo workspace that runs configurable probes (TCP, HTTP, SMTP, IMAP, UDP,
DNS, TLS expiry) on a schedule, measures up/down and latency, and writes results
to InfluxDB via batched line protocol. An optional Axum debug UI streams results
over SSE. It ships as a distroless, non-root, multi-arch image.

| Crate | Role |
|---|---|
| `huginn/` | Binary: CLI, config load, logging, scheduler, graceful shutdown |
| `crates/huginn-core/` | Shared types, config structs, `HuginError`, the `EventHub` |
| `crates/huginn-probes/` | `Probe` trait, `ProbeRegistry`, one module per protocol |
| `crates/huginn-influx/` | Line-protocol writer: batching, bounded retry queue |
| `crates/huginn-web/` | Axum debug UI plus a separately-gated Prometheus listener |

## The structural difference that drives everything else

huginn's centre of gravity is a **data flow**. The scheduler is the sole
publisher; console output, the InfluxDB writer and the web UI all subscribe to
one `EventHub` (a tokio broadcast channel). Almost every design decision in the
tree serves that flow — the bounded retry queue, the batcher that never awaits
I/O, the subscribe-before-spawn discipline.

muninn has no such flow. It never touches a metric. Its centre of gravity is a
**state machine supervising one child process**: read config, render, validate,
spawn, watch, forward signals, exit. The interesting failure modes are "the
config was wrong", "the mount was missing" and "Telegraf died" — none of which a
pub/sub bus helps with.

So the reuse is at the level of *conventions and craft*, not architecture.

## Adopted

| Area | Source | What carries over |
|---|---|---|
| Agent instructions | `AGENTS.md` | The whole shape: hard rules, gates, conventions, doc map. muninn's is an adaptation, not a rewrite |
| Error type | `huginn-core/src/error.rs` | `thiserror` enum plus `type Result<T>`. `Secret { path, message }` carries the path and never the value |
| Secret loading | `config.rs:124` `read_token`, `:212` `read_api_key` | File-only, trimmed, empty is an error, fail closed. `read_api_key`'s comment — "the operator asked for auth, so silently serving unauthenticated would be the worst possible fallback" — is the rule muninn generalises |
| Validation style | `config.rs:507` `validate()` | Every rule carries a comment explaining what breaks without it, and the message names the offending key. The port-collision check at `:573` is muninn's starting point |
| ENV override handling | `config.rs:409` | A bad value warns and keeps the previous setting instead of silently resetting. Warnings are *returned*, not logged, because config is loaded before tracing exists — logging there would go nowhere |
| Logging | `main.rs:153` | `tracing` + `EnvFilter`, one switch between human and JSON |
| Signals | `main.rs:97` | SIGTERM as well as SIGINT. huginn's comment records that catching only SIGINT meant the shutdown drain never ran under systemd or `docker stop` |
| Testing | `docs/testing.md` | The pyramid, the no-sleep-poll rule, `ENV_LOCK`/`with_env` for environment tests, the ≥80 % coverage floor and the honesty about what that floor does not prove |
| Artefact testing | `tests/binary_lifecycle_test.rs` | Running the compiled binary as a subprocess. See below |
| CI shape | `.github/workflows/ci.yml` | Build once per architecture into a tarball; scan, integration and publish all consume that same artefact. Actions pinned by SHA. `version-gate` always runs and decides internally whether to enforce, because a skipped job would skip its dependents |
| Supply chain | `deny.toml`, `security.yml` | cargo-deny for advisories, licences, bans and sources; Semgrep pinned by digest; Trivy blocking on fixable CRITICAL/HIGH |
| Release flow | `docs/releasing.md`, `scripts/changelog-version.sh` | Version derived from the CHANGELOG, tags created by the pipeline and never by hand |

### The one test lesson worth restating

`huginn/tests/binary_lifecycle_test.rs` exists because of a specific bug: `run()`
returned immediately without a keep-alive, so `main()` exited, the Tokio runtime
was dropped, and every probe was cancelled before it fired. The daemon monitored
nothing, and the tests passed for months — because they all spawned `run()` into
the *test's* runtime, which outlived it. Production has no such runtime.

This matters more for muninn than for huginn. muninn is PID 1 with a child
process; the entire lifecycle — signal handling, grace periods, child reaping,
exit codes — is exactly the class of behaviour an in-process test cannot observe.
WP6 therefore treats subprocess tests as mandatory, not as a nice extra.

## Rejected

**The `EventHub` and the broadcast bus.** No metrics flow through muninn. A bus
with one publisher and no subscribers is structure without content.

**`huginn-probes`, `huginn-influx`, `huginn-web`'s UI assets.** Entirely
superseded — Telegraf does the collecting and the writing.

**Config without `deny_unknown_fields`.** huginn's `AppConfig` silently accepts
unknown YAML keys, so a misspelled key looks exactly like a deliberate omission.
muninn's brief requires the opposite, and it is the right requirement: a
monitoring agent that quietly drops half its configuration is worse than one that
refuses to start.

**`interval_secs: u64`.** muninn takes `30s`, `5m`, `1h`. Seconds-as-integer
reads badly at `interval_secs: 3600` and invites unit mistakes.

**A flat `AppConfig`.** huginn has no `version:` field and no migration path, so
a schema change would be a breaking change with nowhere to put the compatibility
shim. muninn separates `ConfigV1` from the normalised internal model from the
start.

**Dependencies.** `chrono`, `colored`, `reqwest`, `hickory-resolver`,
`x509-parser` and `wiremock` all serve probe work that muninn does not do.

## Divergences worth flagging

Three places where muninn does *not* follow huginn, deliberately:

1. **Edition 2024 with resolver 3**, where huginn is on 2021/resolver 2. This
   greenfield tree has nothing to keep compatible. The catch, documented in
   `Cargo.toml`: resolver 3 is MSRV-aware and will hold a dependency back rather
   than require a newer compiler — which is how a project silently stays on the
   release *before* a CVE fix, exactly what huginn hit with hickory-resolver and
   RUSTSEC-2026-0119. Verified that nothing is currently held back; if that
   changes, the floor gets raised rather than the old crate accepted.

2. **`OpenSSL` removed from the licence allow-list and banned outright.** huginn
   allows it. muninn adds no TLS stack of its own — Telegraf is a separate
   process with Go's TLS, and muninn's own HTTP surface is a plaintext health
   port — so an OpenSSL dependency appearing would be an accident worth failing
   the build over.

3. **Stronger port-collision checking.** huginn compares `ui.bind`/`metrics.bind`
   for exact equality, which misses `0.0.0.0:8080` against `127.0.0.1:8080` —
   two configurations that cannot both bind. muninn checks for wildcard overlap.

## What was checked and found not to apply

huginn's InfluxDB writer is careful work — the batcher never awaits I/O so a dead
InfluxDB cannot stall the event source, and the retry queue is bounded in bytes
with drop-oldest semantics. None of it transfers: Telegraf owns buffering,
batching and retry in muninn's design, and reimplementing any of it would mean
two competing buffers with different failure behaviour.

It is recorded here because "we looked and chose not to" is worth more to the
next reader than silence.
