# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release pipeline reads the version from this file — see
[`docs/ci-cd.md`](docs/ci-cd.md). Never hand-push a `v*` tag.

## [Unreleased]

### Added

- Repository skeleton: ignore rules, LF line-ending policy, MIT licence.
- Cargo workspace of five crates — `muninn`, `muninn-core`, `muninn-telegraf`,
  `muninn-modules`, `muninn-health` — with module contracts documented and no
  implementation yet.
- Stable exit codes in `muninn-core::exit`, with tests. Shipped ahead of the
  implementation because they are an operator-facing contract.
- Schema V1 example configurations: annotated, minimal and integration.
- `docs/reference/telegraf.reference.conf` — the Telegraf configuration the
  renderer must produce, verified against Telegraf 1.39.2 with
  `telegraf config check` (exit 0) before any renderer code exists.
- `docs/reference/ordering-{correct,broken}.conf` — fixtures for the sub-table
  ordering rule.
- Twelve architecture decision records.
- `docs/roadmap.md` — work packages WP0–WP12 with scope and definition of done.
- `docs/analysis/huginn-review.md` — what huginn.io contributes and what was
  deliberately rejected.
- `docs/spikes/updates-spike.md` — plan and test matrix for reading host package
  state from a container.
- Reference documentation: architecture, configuration, modules, rendering,
  supervision, host mounts, hardening, testing, versioning, CI/CD, risks.

- WP1 host update spike: `spikes/updates/probe.sh` (the specification WP10
  implements), `spikes/updates/fixtures/build-host.sh` and
  `spikes/updates/run.sh`, reproducing all thirteen matrix cells — including
  T11b, which checks the probe from a container against a real host with a
  non-zero answer.

- WP2 configuration model: `muninn-core` gains typed errors with exit-code
  mapping, a `Secret` type whose `Debug`/`Display` render `***`, duration
  parsing, `ConfigV1` with `deny_unknown_fields` throughout, CLI/ENV overrides,
  semantic validation and a resolved `Config`. 99 tests, 89 % line coverage.

- WP3 Telegraf model and renderer: a typed `PluginInstance`/`TelegrafConfig`
  model and a deterministic TOML renderer. Scalars and sub-tables are separate
  fields, so the ordering rule Telegraf requires is enforced by the type rather
  than by convention. 29 tests, 95 % line coverage.

- WP4 and WP5: the `MonitoringModule` trait, eleven input modules, the `[agent]`
  section, InfluxDB v2 and Prometheus outputs, and the `muninn` CLI with
  `validate`, `render-config` and `version`. Rendering the shipped example now
  produces `docs/reference/telegraf.reference.conf` byte for byte, and that file
  is accepted by Telegraf 1.39.2. 165 tests.

### Fixed

- A validation rule compared `runtime.shutdown_grace_period` against
  `agent.flush_interval`, on the theory that a short grace period discards the
  collection cycle in progress. Telegraf flushes immediately on SIGTERM rather
  than waiting for the next tick, so the bound that matters is the output
  timeout. The rule and the four documents repeating the claim are corrected.
- `muninn.integration.yaml` set `shutdown_grace_period` equal to
  `outputs.influxdb.timeout`, leaving no room for a teardown flush.

### Decided

- Approach A adopted for reading host package state — read-only host mounts plus
  `apt-get -s dist-upgrade`. Measured to reproduce each host's own answer exactly
  on Debian 12/13 and Ubuntu 22.04/24.04, including across distributions, under
  non-root with all capabilities dropped, leaving the host tree byte-identical.
  ADR-0009 moves from proposed to accepted.
- The runtime base image is `debian:12-slim` rather than distroless, because
  approach A needs `apt` and `dpkg`. Measured cost: 88 packages instead of 10,
  5 CRITICAL / 17 HIGH CVEs instead of none — all currently unfixable, four of the
  five CRITICAL in `perl-base`, which muninn never invokes. Tracked as R7.

### Notes

Nothing is released yet. `muninn` builds but exits immediately with a pointer to
the roadmap — WP0 is a design package, not a working agent.

Three findings during WP0 changed the design away from the project brief:

- Validation uses `telegraf config check`, not `--test`, so it never binds the
  port the real process needs.
- Exclusion options do not exist on the relevant plugins and must render as
  `tagdrop` sub-tables, which forces a declared render order rather than sorted
  keys.
- There is no `inputs.load`; the `load` and `system` modules merge into one
  plugin instance.
