# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release pipeline reads the version from this file — see
[`docs/ci-cd.md`](docs/ci-cd.md). Never hand-push a `v*` tag.

## [Unreleased]

### Added

- **The `image_updates` module** — per running container, whether a newer
  image is available under the tag it is running. Off by default; needs the
  same Docker socket as `docker` and shares its startup reachability check.
  Rather than adding a TLS stack to speak to registries directly, it asks the
  Docker daemon to resolve each container's tag against the registry
  (`GET /distribution/{name}/json`) — the same resolution `docker pull`
  performs, without pulling — and compares the digest against what the daemon
  recorded when the running image was pulled. `muninn image-check` runs the
  same check `inputs.exec` does, for diagnosis by hand. Like `updates`, a
  failed check degrades that one container's series rather than muninn as a
  whole. See [`docs/modules.md#image_updates`](docs/modules.md#image_updates)
  and [ADR-0013](docs/adr/0013-image-updates-via-docker-api.md). Adds
  `serde_json` as a dependency, in `muninn-modules` only. Every request path
  the module's Docker API client builds is checked for a control character or
  a space before it is sent, closing off request-line injection at the one
  place muninn writes a raw HTTP request from daemon-reported strings —
  found in a security review before this module's first release.
- CI/CD, completed (WP12) — `.github/workflows/`, built to huginn.io's shape.
  Twelve jobs: format and lint, tests on stable and beta, `cargo deny`,
  coverage, the Telegraf reference check, the version gate, a per-architecture
  image build, Trivy, the system suites, and the push and publish path. Plus
  Semgrep, the release workflow, Dependabot and the branch automation.
- **The image is built once per architecture into an artefact, and every later
  job consumes that same artefact.** The bytes that are scanned, tested and
  published are byte-identical. A pipeline that rebuilds between scanning and
  publishing has not scanned what it published.
- Three jobs huginn.io does not have, each covering something no Rust test can
  see. `reference` re-checks that the pinned Telegraf still accepts
  `telegraf.reference.conf` — the file every snapshot is anchored to, so if it
  stops being valid the suite stays green and the artefact is wrong — plus the
  ordering fixtures and every plugin option the documentation names.
  `integration` runs the stack test and the hardened-image tests. `updates` runs
  the module against real Debian and Ubuntu trees.
- An SBOM per architecture, generated from the same tarball that was scanned,
  and one for the published multi-arch image attached to each GitHub Release.
  An SBOM describing different bytes than the ones that shipped is worse than
  none, because it will be believed.
- Images go to Docker Hub and are mirrored to `ghcr.io` from the finished
  manifest with `skopeo copy --all`, so both registries carry byte-identical
  images with the same digests from one build. This settles O2. Publishing needs
  a `DOCKERHUB_USERNAME` variable and a `DOCKERHUB_TOKEN` secret, which
  `docs/ci-cd.md` lists — creating them is the maintainer's, not the agent's.
- `scripts/changelog-version.sh` — the version, validated as SemVer before it is
  printed, so no consumer can be handed a changelog heading that reaches a shell
  as command substitution. `--allow-unreleased` falls back to the workspace
  version so pushes to `dev` keep producing an image before the first release;
  the version gate deliberately does not use it.
- `scripts/test-report.sh` — every GitHub Release ships a per-suite test report
  and a coverage figure, produced by re-running the suite on the tagged commit.
- ShellCheck and actionlint as their own gates. `scripts/*.sh` are the three
  system test suites, so a quoting bug in one of them is a test that passes
  without testing; Semgrep has no ruleset for shell. actionlint covers the
  workflows and the shell inside them.
- `.trivyignore.yaml` — read only by the *blocking* scan, so a suppressed
  finding still reaches the Security tab. Every entry needs an expiry date and a
  reason the code is unreachable in muninn's generated configuration. Two
  entries today, both Go modules vendored into the Telegraf binary, documented
  in `docs/hardening.md`.

- End-to-end tests, completed (WP11). `scripts/integration-test.sh` brings up
  `docker-compose.integration.yml` — muninn, Telegraf, InfluxDB 2.7 and
  Prometheus 3.5, every hop a real process — and follows one metric the whole
  way: collected from the host, written to a database, queried back with Flux,
  and scraped from both endpoints by a real Prometheus. 24 cells.
- A real Prometheus rather than a `curl` of `/metrics`, because a malformed
  exposition line is still bytes over HTTP: curl proves a string exists and
  nothing about whether Prometheus would accept it. Both endpoints are scraped —
  `:9273` alone cannot tell a dead agent from a dead host.
- `muninn/tests/secrets_and_mounts_test.rs` — twelve failure-path tests on the
  compiled binary. A missing, empty, whitespace-only or directory-shaped secret
  is exit `11` with the path named; no command prints a token whatever goes
  wrong; a host mount that is absent, empty or not a directory is exit `12`
  naming both the path and the module that needs it.

- Updates module, completed (WP10). muninn reports the host's pending package
  updates by mounting its apt and dpkg state read-only and letting real apt
  resolve them — the approach the WP1 spike measured, now implemented in Rust and
  verified against the same hosts: 41/3 on Debian 12, 39/2 on Debian 13, 50/40 on
  Ubuntu 22.04 and 66 pending on Ubuntu 24.04, each identical to the host's own
  answer at the time of the run.
- `muninn update-check` — the command the module runs through `inputs.exec`, and
  the one to run by hand when a count looks wrong. It prints the same influx line
  Telegraf parses, and the detail behind the failure reason on stderr. It always
  exits 0: a failed check is data, not a crash.
- A failed check emits `check_success=0` with a low-cardinality `reason` tag and
  **omits** the counts. It never reports zero — "no updates pending" and "I could
  not look" are opposite conclusions.
- muninn now runs the check once at startup, so an unreadable host shows up in
  the logs, in `/status` and in `muninn_module_check_success{module="updates"}`
  within seconds rather than after the first hourly interval. The result degrades
  muninn rather than stopping it: the failure is visible in the metrics, so
  nothing is being misrepresented and everything else keeps collecting.
- `scripts/updates-test.sh` — 17 system-test cells (brief §18.6) running the
  shipped image against real Debian and Ubuntu host trees, a real host through
  WSL, the failure cells that prove a failed check never reports a count, and the
  module end to end in a container. It caught two bugs a unit test could not: apt
  takes temp files outside `Dir::Cache`, which fails on the read-only root
  filesystem the deployment documents, and Telegraf 1.39 rejects the
  space-separated `commands` form the first rendering used.
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
- Reference documentation: architecture, configuration, modules, rendering,
  supervision, host mounts, hardening, testing, versioning, CI/CD, risks and
  roadmap.

- `docs/updates-evidence.md` — the measured basis for the updates module, with
  `scripts/fixtures/build-host.sh` and `build-host-native.sh` building the
  fixtures behind it. All thirteen cells reproduce, including the one that
  checks a container's answer against a real host with a non-zero result.

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

### Changed

- The repository no longer carries the scaffolding it was built with. The
  work-package roadmap is replaced by a short forward-looking one — what shipped
  is this file and the git history. `docs/spikes/updates-spike.md` becomes
  `docs/updates-evidence.md`, because it is the measured basis the updates module
  still rests on rather than a discarded experiment. `spikes/` is dissolved: the
  two fixture builders CI depends on move to `scripts/fixtures/`, the two scripts
  nothing runs are removed, and the fixture scratch directory moves to
  `.fixtures/`. `docs/analysis/huginn-review.md`, a one-time design-intake
  record, is removed.
- The README leads with the product rather than with project status: logo,
  badges, and the quick start at the top instead of behind a work-package
  report. The two-job Prometheus scrape configuration moves to
  `configuration.md`, which is where the endpoints are documented — the two
  pages had been pointing at each other.
- The README's badge row is the one huginn.io uses, adapted: CI and security
  status, licence, open issues, last change, and the published image's version,
  size and pull count. All eight resolve against live data.

### Fixed

- The README, `AGENTS.md`, the roadmap and this file all said no image was
  published. Pushes to `dev` have been publishing `0.1.0-dev` and `dev` to
  `jschnabel/muninn` and to `ghcr.io/joshua-schnabel/muninn.io` since the CI/CD
  work landed, and the quick start named `ghcr.io/…:0.1.0`, a tag that does not
  exist. The quick start now pins `0.1.0-dev`.

- `muninn validate --with-telegraf` failed inside the shipped image. It rendered
  its scratch file through `tempfile`, which writes to `/tmp`, and the hardened
  container has a read-only root filesystem — so the one command an operator
  would run *in the container* to check a configuration exited `30` with
  `Read-only file system`. It now writes into the tmpfs that
  `runtime.generated_config_path` names, and falls back to the system temp
  directory when that directory does not exist, so the command still works on a
  developer's machine. It does not create the directory: that would be muninn
  making a directory outside a container to hold a resolved credential.
- `render-config --output` wrote a world-readable file. With
  `--unsafe-show-secrets` that file holds a real token. The supervisor's writer
  had produced `0600` since WP6; this second one used a plain `fs::write`. Both
  now go through one writer, which sets the mode at creation rather than
  afterwards — closing the window in which the file exists with the umask's mode
  and already contains the token.
- `runtime.host_mount_prefix` was still validated with `starts_with('/')`, the
  Linux-shaped approximation of "absolute" that was corrected for
  `runtime.generated_config_path` and left here. Harmless in production, where
  the value is `/hostfs`; it rejected a host-absolute path on the machine the
  tests run on.
- A host whose `/etc/os-release` carries only `PRETTY_NAME` — Docker Desktop's VM
  does exactly this — was reported as "not Debian-family" and refused the start,
  because both the startup check and the updates module's preconditions read the
  first os-release file they could open and stopped there. `/usr/lib/os-release`
  held `ID=debian` all along. Both now read the locations in order and take the
  first non-empty value of each field, through one shared function.
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

Nothing is released yet. Every command works — `run`, `validate`,
`render-config`, `check-runtime`, `healthcheck`, `update-check` and `version` —
and the container image builds and passes its tests under the full hardening.
The pipeline builds, scans, tests and publishes it: pushes to `dev` produce the
pre-release tags `0.1.0-dev` and `dev` on `jschnabel/muninn`, mirrored
byte-identically to `ghcr.io/joshua-schnabel/muninn.io`. What is missing is a
cut version, which is what turns those into `0.1.0`.

Three findings during the design package changed the design away from the
project brief:

- Validation uses `telegraf config check`, not `--test`, so it never binds the
  port the real process needs.
- Exclusion options do not exist on the relevant plugins and must render as
  `tagdrop` sub-tables, which forces a declared render order rather than sorted
  keys.
- There is no `inputs.load`; the `load` and `system` modules merge into one
  plugin instance.
