# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release pipeline reads the version from this file — see
[`docs/ci-cd.md`](docs/ci-cd.md). Never hand-push a `v*` tag.

## [Unreleased]

### Added

- Docker module, completed (WP9). Enabling it with an endpoint that does not
  answer is now a **startup failure** (exit `12`) rather than an empty metric
  set: muninn issues `GET /_ping` against the configured endpoint and requires a
  `200`. A Docker module reporting nothing is indistinguishable from a host
  running no containers, and a monitoring system must not leave that ambiguous.
- `modules.docker.container_states` — which container states to collect,
  defaulting to `[running]`. Add `exited` to keep reporting containers that
  stopped. Values are validated against Docker's own vocabulary, because
  Telegraf accepts an unknown state silently and it then matches no container.
- `docker-compose.docker-module.yml` and `config/muninn.docker-module.yaml` —
  the socket-proxy deployment documented as the recommendation, now shipped and
  exercised by `scripts/container-test.sh` (22 checks, up from 15).
- Startup now runs the runtime preconditions of every enabled module — the step
  `architecture.md` has documented since WP0 and that `muninn run` never
  performed. `muninn check-runtime` reported these problems; nothing stopped a
  start.

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

- WP6 Telegraf process management: version check against the build-time pin,
  `telegraf config check` before start, Telegraf as a supervised child with its
  output re-emitted through muninn's logger, SIGTERM/SIGINT forwarding with a
  grace period and SIGKILL fallback, and the twelve-state machine. `muninn run`,
  `muninn validate --with-telegraf` and `muninn version` now work.
- `scripts/test-linux.sh` runs the suite in a Linux container against the pinned
  Telegraf, so the `#[cfg(unix)]` tests — signals, permissions, reaping — are
  actually executed rather than silently skipped on a Windows development
  machine.

- WP7 health server: `/health/live`, `/health/ready`, `/status` and `/metrics`,
  served alongside the supervisor. `muninn healthcheck` queries the local
  endpoint and maps it to an exit code Docker's `HEALTHCHECK` understands.
  The state machine moved from the binary into `muninn-health` as `HealthState`,
  next to the endpoints that read it. 223 tests.

- WP8 container image: a three-stage `Dockerfile` that fetches Telegraf by
  version and verifies its checksum per architecture, builds muninn, and
  assembles a `debian:12-slim` runtime carrying only the two binaries. Runs
  non-root (uid 10001) with a `HEALTHCHECK`. `docker-compose.yml` shows the
  hardened deployment, and `scripts/container-test.sh` verifies it — 15 checks.
- `muninn check-runtime`: reports every unmet runtime precondition rather than
  stopping at the first — missing mounts, an occupied port, an unwritable
  runtime directory, a non-Debian host for the updates module. Exits 12.

### Fixed

- The port-collision check treated two listeners on port 0 as a conflict. Port 0
  means "any free port" and the OS hands out a different one to each, so this
  rejected a configuration that works — and it is what the tests and the
  integration stack use to avoid fighting over ports.

### Fixed (earlier)

- Signal handlers were installed inside the supervise loop, leaving a window
  during startup where SIGTERM had its default disposition and killed muninn
  instead of shutting it down. They are now installed before any startup work.
  Found by the lifecycle test on its first run under Linux.
- `runtime.generated_config_path` was validated with `starts_with('/')`, a
  Linux-specific approximation of "absolute". It now accepts both a POSIX
  absolute path and a host-absolute one, so a production configuration validates
  on a developer's machine and the tests can use a temporary directory.

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

Nothing is released yet. `muninn run`, `validate`, `render-config` and `version`
work; `check-runtime` and `healthcheck` still fail with a pointer to the work
package that delivers them. There is no container image yet (WP8) and no health
server (WP7), so muninn is not deployable — only runnable.

Three findings during WP0 changed the design away from the project brief:

- Validation uses `telegraf config check`, not `--test`, so it never binds the
  port the real process needs.
- Exclusion options do not exist on the relevant plugins and must render as
  `tagdrop` sub-tables, which forces a declared render order rather than sorted
  keys.
- There is no `inputs.load`; the `load` and `system` modules merge into one
  plugin instance.
