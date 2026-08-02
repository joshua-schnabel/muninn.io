# Roadmap

The work packages for muninn.io, in order. This file is the place to resume
from: each package states what it touches, what "done" means, and which part of
the project brief it satisfies. Nothing here depends on remembering a
conversation.

**Status legend:** ✅ done · 🔨 in progress · ⬜ not started

| WP | Title | Status |
|---|---|---|
| [WP0](#wp0--design-package) | Design package | ✅ |
| [WP1](#wp1--host-update-spike) | Host update spike | ⬜ |
| [WP2](#wp2--configuration-model-v1) | Configuration model V1 | ⬜ |
| [WP3](#wp3--telegraf-model-and-renderer) | Telegraf model and renderer | ⬜ |
| [WP4](#wp4--base-modules) | Base modules | ⬜ |
| [WP5](#wp5--outputs) | Outputs | ⬜ |
| [WP6](#wp6--telegraf-process-management) | Telegraf process management | ⬜ |
| [WP7](#wp7--health-server-and-state-machine) | Health server and state machine | ⬜ |
| [WP8](#wp8--container-image) | Container image | ⬜ |
| [WP9](#wp9--docker-module) | Docker module | ⬜ |
| [WP10](#wp10--updates-module) | Updates module | ⬜ |
| [WP11](#wp11--end-to-end-tests) | End-to-end tests | ⬜ |
| [WP12](#wp12--cicd-and-release) | CI/CD and release | ⬜ |

## Why this order differs from the brief

The brief puts the update spike at step 11. It is WP1 here.

The spike decides whether reading the host's package state from a container is
possible without `apt` and `dpkg` inside the image. If it is not, the runtime
image cannot be distroless and has to be debian-slim, which changes the
Dockerfile, the hardening documentation and the Trivy baseline. Discovering that
after WP8 means writing all three twice. Everything else keeps the brief's
sequence.

---

## WP0 — Design package

**Goal.** Establish the repository, the conventions and the design, so that
every later package is an implementation task rather than a design task.

**Touches**

- `AGENTS.md`, `README.md`, `CHANGELOG.md`, `LICENSE`
- `Cargo.toml` + five crate skeletons, `deny.toml`, `.cargo/config.toml`
- `config/muninn.{example,minimal,integration}.yaml`
- `docs/` — architecture, configuration, modules, telegraf-rendering,
  supervision, host-mounts, risks, roadmap, analysis, adr/0001–0012,
  spikes/updates-spike.md
- `docs/reference/telegraf.reference.conf` and the ordering fixtures
- `crates/muninn-core/src/exit.rs` — the exit-code contract

**Done when**

1. `cargo build --workspace --locked`, `cargo fmt --all -- --check`,
   `cargo clippy --workspace --all-targets --all-features -- -D warnings` and
   `cargo test --workspace` are clean.
2. All three example configs parse as YAML.
3. `telegraf config check` accepts `docs/reference/telegraf.reference.conf`
   against the pinned Telegraf version, exit 0.
4. Every Telegraf plugin option named in `docs/modules.md` exists in the pinned
   version's `sample.conf`.
5. The pinned Telegraf tarball checksums verify for both architectures.
6. Every relative Markdown link resolves.

**Brief:** §26 step 1–2, §29 (all thirteen items).

---

## WP1 — Host update spike

**Goal.** Decide, with evidence, how muninn reads the *host's* pending package
updates from inside a container — or establish that it cannot be done safely and
the module ships as experimental.

This is the highest-risk package in the project. Telegraf has no package input
plugin (checked: none of its 249 inputs), and `apt` inside the container reads
the container's package database, so a naive implementation is not merely wrong
but *plausibly* wrong.

**Touches**

- `spikes/updates/` — runner script, per-approach probes, fixture rootfs setup
- `docs/spikes/updates-spike.md` — results per matrix cell, decision
- `docs/adr/0009-updates-module-approach.md` — finalised from "open"
- `docs/hardening.md` — base-image consequence

**Done when**

1. Approaches A (read-only host mounts), B (chroot), C (nsenter) and D (host
   helper) are each evaluated and recorded, including the ones rejected.
2. Test matrix T1–T11 from `docs/spikes/updates-spike.md` has a recorded result
   per cell, across Debian 12/13 and Ubuntu 22.04/24.04.
3. T11 compares against a real host (WSL Debian), not only container fixtures.
4. T8/T9/T10 demonstrate that a failed check reports failure — never "0 updates".
5. No host package data is modified by any approach that is kept.
6. The runtime base image is decided and written down with its reasoning.
7. `bash spikes/updates/run.sh` reproduces the results.

**Blocks:** WP8 (base image), WP10 (implementation).
**Brief:** §8 in full, §18.6, §29.11.

---

## WP2 — Configuration model V1

**Goal.** Turn a YAML file into a validated, normalised internal model, or into
an error that names the offending key.

**Touches** `crates/muninn-core/src/` — `config/{model,loader,validation,normalised}.rs`,
`secrets.rs`, `duration.rs`, `error.rs`.

**Done when**

1. `ConfigV1` mirrors the schema in `docs/configuration.md`, every struct with
   `#[serde(deny_unknown_fields)]`.
2. An unknown or misspelled key fails the load and the message names the key path.
3. An absent, unknown or non-integer `version` fails with a distinct message.
4. Durations accept `30s`/`5m`/`1h`; zero, negative and nonsensical values are
   rejected with a reason.
5. Secret files: missing, unreadable and empty are all distinct errors naming the
   path; a trailing newline is stripped; the *contents* appear in no error and no
   log line.
6. The secret type renders `***` through both `Debug` and `Display`, with a test
   that formats one and asserts the value does not appear.
7. Semantic rules hold: no output enabled is fatal; a port collision between
   `health.listen` and `outputs.prometheus.listen` is fatal, including the case
   where one is a wildcard address; InfluxDB enabled without a readable token is
   fatal.
8. At least one negative test per configuration field (brief §18.7).
9. `ConfigV1` converts into the normalised model, so a future `ConfigV2` has a
   place to convert into.

**Brief:** §6, §15 "fatal before start", §18.1, §18.7, §26 step 3.

---

## WP3 — Telegraf model and renderer

**Goal.** Turn the normalised model into byte-identical, valid Telegraf TOML.

**Touches** `crates/muninn-telegraf/src/{model,renderer}.rs`, `muninn/src/cli.rs`
(`render-config`).

**Done when**

1. `PluginInstance` keeps scalars in declaration order and sub-tables separately;
   the renderer emits scalars first and sub-tables last.
2. `ordering-broken.conf` is a regression test: the renderer can never produce
   that shape. Note it passes `telegraf config check` — the test asserts on the
   rendered bytes, not on Telegraf's verdict.
3. Rendering the same config twice produces identical bytes; module order,
   field order and whitespace are all stable, with no timestamps or generated
   identifiers.
4. Rendering `config/muninn.example.yaml` matches
   `docs/reference/telegraf.reference.conf`.
5. Escaping is centralised and tested against paths and globs containing spaces,
   `"`, `\` and non-ASCII characters.
6. `muninn render-config` writes to stdout or a named file, redacts secrets by
   default, and starts nothing.
7. No module builds TOML with `format!`.

**Brief:** §11, §18.2, §26 step 4.

---

## WP4 — Base modules

**Goal.** The nine modules that need no privileges beyond read-only host paths.

**Touches** `crates/muninn-modules/src/` — `cpu`, `memory`, `load`, `system`,
`swap`, `processes`, `disks`, `disk_io`, `network`, plus the `MonitoringModule`
trait and registry.

**Done when**

1. Every module implements validate / requirements / render separately.
2. `load` and `system` merge into one `[[inputs.system]]` instance with the union
   of their include groups; enabling one, the other, both and neither are four
   tested cases.
3. Every `exclude_*` option renders into a `tagdrop` sub-table on the right tag:
   `path` for disk, `name` for diskio, `interface` for net.
4. Snapshot test per module, reviewed rather than auto-accepted.
5. `requirements()` reports the host paths each module needs, so
   `check-runtime` can verify only what is enabled.

**Brief:** §7, §18.2, §26 step 5.

---

## WP5 — Outputs

**Goal.** InfluxDB v2 and Prometheus, separately and together.

**Touches** `crates/muninn-modules/src/outputs/{influxdb,prometheus}.rs`.

**Done when**

1. `outputs.influxdb_v2` renders with `urls` as an array, the token resolved from
   its file, and TLS options only when configured.
2. `outputs.prometheus_client` renders listen, path and expiration interval;
   basic auth only when both username and password file are set.
3. All five validation cases from brief §10.3 are tested: no output; both
   outputs; InfluxDB without its secret; Prometheus colliding with the health
   port; an unparseable listen address.
4. `insecure_skip_verify: true` logs a prominent warning and is documented as
   unsafe.
5. Snapshots for InfluxDB alone, Prometheus alone, and both.

**Brief:** §10, §26 step 6.

---

## WP6 — Telegraf process management

**Goal.** Start Telegraf, supervise it, and shut it down cleanly.

**Touches** `crates/muninn-telegraf/src/{validator,process,version}.rs`,
`muninn/src/main.rs`.

**Done when**

1. The runtime Telegraf version is compared against the version pinned at build
   time; a mismatch exits with `TELEGRAF_START` rather than running a config
   written for a different plugin surface.
2. `telegraf config check --strict-env-handling` runs against the generated file
   before Telegraf is started; failure exits `TELEGRAF_CONFIG`.
3. Telegraf runs as a direct child; muninn knows its PID and its status.
4. Telegraf's stdout and stderr are captured and re-emitted with the source
   identifiable, in whichever log format is configured.
5. An unexpected exit is detected, its code and signal logged, readiness set
   false, and muninn exits `TELEGRAF_EXITED`. No internal restart loop.
6. SIGTERM and SIGINT: readiness false, signal forwarded, wait up to
   `shutdown_grace_period`, then SIGKILL, then exit.
7. An integration test uses a real Telegraf binary, not a mock.
8. A test runs the compiled binary as a subprocess and asserts it stays alive —
   the process-lifecycle bug class that in-process tests structurally cannot see
   (see `docs/testing.md`).

**Brief:** §5.1, §5.2, §5.3, §23, §18.3, §26 step 7.

---

## WP7 — Health server and state machine

**Goal.** Make muninn's state observable and correct.

**Touches** `crates/muninn-health/src/{server,state,metrics}.rs`,
`muninn/src/supervisor/`.

**Done when**

1. The twelve states and their transitions from `docs/architecture.md` are
   implemented, with a test per legal transition and per rejected one.
2. `/health/live` reflects only muninn's own responsiveness; a transient output
   failure does not fail it.
3. `/health/ready` succeeds only in `Ready` (and the defined `Degraded`), returns
   503 otherwise, and reports Telegraf's PID and running state.
4. `/status` exposes versions, uptime, enabled modules and outputs, and the last
   Telegraf exit — and no secrets, no full configuration dump.
5. `/metrics` serves the `muninn_*` families, with no high-cardinality labels: no
   error strings, no paths, no PIDs.
6. A failing updates module produces `Degraded`, not `Failed`; a dead Telegraf
   produces `Failed`.

**Brief:** §13, §16, §22, §26 step 8.

---

## WP8 — Container image

**Goal.** One self-contained, hardened image.

**Blocked by WP1** — the base image depends on the spike's outcome.

**Touches** `Dockerfile`, `docker-compose.yml`,
`docker-compose.integration.yml`, `muninn/src/cli.rs`
(`check-runtime`, `healthcheck`), `docs/hardening.md`, `docs/host-mounts.md`.

**Done when**

1. Multi-stage build; the runtime stage carries the muninn binary, the pinned
   Telegraf binary and nothing else it does not need.
2. Telegraf is fetched by version and verified against the checksum recorded in
   ADR-0011, for both `linux/amd64` and `linux/arm64`. The build fails on a
   mismatch.
3. Non-root, read-only root filesystem, `no-new-privileges`, no capabilities
   beyond those a module declares.
4. `/run/muninn` is a tmpfs writable only by the runtime user.
5. `muninn check-runtime` verifies mounts, permissions, secrets, port conflicts,
   the Docker socket and the host OS, and exits non-zero on any problem.
6. `muninn healthcheck` queries the local health endpoint and returns an exit
   code Docker's `HEALTHCHECK` can use.
7. Container tests from brief §18.5 pass: missing config, missing secret, wrong
   mount, reachable Prometheus port, clean SIGTERM, detected Telegraf crash, and
   the container not staying falsely healthy.

**Brief:** §3.2, §9, §12, §17.1, §17.3, §18.5, §26 step 9.

---

## WP9 — Docker module

**Goal.** Per-container metrics, without normalising socket access.

**Touches** `crates/muninn-modules/src/docker.rs`, `docs/modules.md`,
`docs/adr/0010-docker-socket.md`.

**Done when**

1. Off by default; enabling it is refused unless the endpoint is actually
   reachable — no silent empty metric set.
2. Include/exclude filters, timeout and container state selection render
   correctly.
3. A socket-proxy configuration is documented and tested as the recommended
   deployment.
4. The security implications are documented where an operator will read them
   before enabling, not in an appendix.
5. Integration test against a real Docker socket.

**Brief:** §7.2 Docker, §17.2, §26 step 10.

---

## WP10 — Updates module

**Goal.** Implement whatever WP1 concluded.

**Blocked by WP1.**

**Touches** `crates/muninn-modules/src/updates/{mod,debian}.rs`, the update
helper binary, `docs/modules.md`.

**Done when**

1. The implementation matches the approach recorded in ADR-0009 — no substitute
   chosen during implementation without amending the ADR.
2. Metrics are as specified in `docs/spikes/updates-spike.md`.
3. A failed check emits `check_success=0` and omits the pending counts. It never
   emits zero.
4. Missing preconditions are detected at startup when the module is enabled.
5. System tests per brief §18.6, including the comparison against a native host
   check.
6. A module failure degrades muninn rather than stopping it, and is visible in
   logs, `/status` and the metrics.

**Brief:** §7.2 Updates, §8.3, §18.6, §26 step 11.

---

## WP11 — End-to-end tests

**Goal.** Prove the whole path, not the pieces.

**Touches** `scripts/integration-test.sh`, `docker-compose.integration.yml`,
`muninn/tests/`.

**Done when**

1. The ten steps of brief §18.3 run against a real Telegraf: load, generate,
   validate, start, confirm running, readiness, scrape Prometheus, find a real
   system metric, shut down, confirm cleanup.
2. A temporary InfluxDB container receives metrics and a query confirms the
   expected measurements, with throwaway credentials only.
3. A Telegraf crash is injected and detected.
4. Secrets and mounts have failure-path tests, not only happy paths.

**Brief:** §18.3, §18.4, §26 step 12.

---

## WP12 — CI/CD and release

**Goal.** Every gate automated, images published reproducibly.

**Touches** `.github/workflows/{ci,security,release,auto-pr}.yml`,
`.github/dependabot.yml`, `scripts/changelog-version.sh`, `docs/ci-cd.md`.

**Done when**

1. Jobs: fmt, clippy (`-D warnings`), unit tests, integration tests, coverage
   (≥80 % workspace lines), `cargo deny`, licence check, Docker build, container
   integration tests, Trivy scan, SBOM, release build.
2. The image is built once per architecture into an artefact; scan, integration
   and publish all consume that same artefact, so the bytes tested are the bytes
   published.
3. `linux/amd64` and `linux/arm64`, with Telegraf pinned and verified for both.
4. Every action pinned by commit SHA; least-privilege `permissions:` per job.
5. A version gate blocks a release whose version is not a valid SemVer successor.
6. `docs/ci-cd.md` records the repository settings a maintainer must apply by
   hand — branch protection and any registry secrets. Those are deliberately not
   automated.

**Brief:** §19, §17.3, §26 step 13.

---

## Definition of done for the MVP

The twenty-seven acceptance criteria in brief §27. The one that outranks the
rest, from §26 step 14: a new user can start muninn on a Debian or Ubuntu host
using only the README, without ever seeing Telegraf's TOML.
