# Roadmap

The work packages for muninn.io, in order. This file is the place to resume
from: each package states what it touches, what "done" means, and which part of
the project brief it satisfies. Nothing here depends on remembering a
conversation.

**Status legend:** ✅ done · 🔨 in progress · ⬜ not started

| WP | Title | Status |
|---|---|---|
| [WP0](#wp0--design-package) | Design package | ✅ |
| [WP1](#wp1--host-update-spike) | Host update spike | ✅ |
| [WP2](#wp2--configuration-model-v1) | Configuration model V1 | ✅ |
| [WP3](#wp3--telegraf-model-and-renderer) | Telegraf model and renderer | ✅ |
| [WP4](#wp4--base-modules) | Base modules | ✅ |
| [WP5](#wp5--outputs) | Outputs | ✅ |
| [WP6](#wp6--telegraf-process-management) | Telegraf process management | ✅ |
| [WP7](#wp7--health-server-and-state-machine) | Health server and state machine | ✅ |
| [WP8](#wp8--container-image) | Container image | ✅ |
| [WP9](#wp9--docker-module) | Docker module | ✅ |
| [WP10](#wp10--updates-module) | Updates module | ✅ |
| [WP11](#wp11--end-to-end-tests) | End-to-end tests | ✅ |
| [WP12](#wp12--cicd-and-release) | CI/CD and release | ✅ |

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

**Status: complete, 2026-08-02.** Approach A adopted; see
[ADR-0009](adr/0009-updates-module-approach.md) and the
[spike results](spikes/updates-spike.md). WP8 and WP10 are unblocked.

**Outcome.** Mounting the host's apt and dpkg state read-only and running
`apt-get -s dist-upgrade` against it reproduces the host's own answer exactly —
41/3 on Debian 12, 39/2 on Debian 13, 50/40 on Ubuntu 22.04, 66/34 on Ubuntu
24.04 — including from a container running a different distribution. It works
under non-root with `--cap-drop=ALL` and a read-only root filesystem, and leaves
the host tree byte-identical. Approaches B and C both need capabilities the
hardening baseline excludes; B fails even when granted them.

**Consequence.** The runtime base image is `debian:12-slim`, not distroless: 88
packages instead of 10, 5 CRITICAL / 17 HIGH CVEs instead of none, all currently
unfixable. Accepted as a trade, with the mitigations in
[`hardening.md`](hardening.md) now load-bearing. Tracked as
[R7](risks.md).

**Delivered**

- `spikes/updates/probe.sh` — the specification WP10 implements
- `spikes/updates/fixtures/build-host.sh` (container fixtures),
  `build-host-native.sh` (a real host), `spikes/updates/run.sh` — reproducible
  via `bash spikes/updates/run.sh`
- `docs/spikes/updates-spike.md` — thirteen matrix cells with measured results
- ADR-0009 finalised; `hardening.md` and `risks.md` updated

**Done when** — all met:

1. ✅ Approaches A–D each evaluated and recorded, including the rejected ones.
2. ✅ T1–T11b recorded across Debian 12/13 and Ubuntu 22.04/24.04.
3. ✅ T11/T11b ran against a real host (WSL Debian). T11b is the faithful one —
   probe in a container against the host's filesystem, with fresh indices —
   and reproduces that host's own answer of 41 pending / 11 security exactly.
4. ✅ T8/T9/T9b/T10 demonstrate that a failed check reports failure, never zero.
5. ✅ No host data modified — SHA-256 over the host tree identical before and after.
6. ✅ Base image decided and written down with its measurements.
7. ✅ `bash spikes/updates/run.sh` reproduces all thirteen cells.

**Brief:** §8 in full, §18.6, §29.11.

---

## WP2 — Configuration model V1

**Status: complete, 2026-08-02.** 99 tests, 89 % line coverage in `muninn-core`.

**Goal.** Turn a YAML file into a validated, normalised internal model, or into
an error that names the offending key.

**Touches** `crates/muninn-core/src/` — `config/{model,loader,validation,normalised}.rs`,
`secret.rs`, `duration.rs`, `error.rs`; `tests/shipped_configs_test.rs`.

**Two things it found in WP0's own output**, which is what the tests were for:

- The shipped defaults tripped a validation rule that was itself wrong.
  `shutdown_grace_period` was being compared against `agent.flush_interval` on
  the theory that a short grace period discards the collection cycle in
  progress. Telegraf does not work that way — it flushes immediately on SIGTERM
  (`agent.go`: *"Hang on, flushing any cached metrics before shutdown"*) rather
  than waiting for the next tick. The bound that matters is the output timeout.
  Rule corrected, and the claim removed from four documents that repeated it.
- `muninn.integration.yaml` had `shutdown_grace_period` equal to
  `outputs.influxdb.timeout`, so a teardown flush could not complete one write
  attempt.

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
10. The shipped example configurations pass muninn's own validation, so the file
    people copy cannot drift away from the schema.

All met. Numbering the rules above 1–10, each has at least one test; the
`rejects()` helper additionally asserts that every error message **names the
offending key**.

**Brief:** §6, §15 "fatal before start", §18.1, §18.7, §26 step 3.

---

## WP3 — Telegraf model and renderer

**Status: complete, 2026-08-02.** 29 tests, 95 % line coverage in
`muninn-telegraf`.

**Goal.** Turn a typed model into byte-identical, valid Telegraf TOML.

**Scope corrected.** As originally written, this package listed two criteria it
could not possibly meet: rendering `muninn.example.yaml` and shipping
`muninn render-config` both need the modules (WP4) and outputs (WP5) that
produce the plugin instances. There is nothing to render until those exist.
Both moved to [WP5](#wp5--outputs), where the whole pipeline first has an
end-to-end result — and where matching the Telegraf-verified reference config
becomes the real milestone.

**Touches** `crates/muninn-telegraf/src/{model,renderer}.rs`.

**Done when** — all met:

1. ✅ `PluginInstance` keeps scalars and sub-tables in separate fields, and the
   renderer always emits scalars first. The rule is enforced by the type, not by
   convention: a scalar added *after* a sub-table still renders before it.
2. ✅ The broken ordering is a property test over the rendered bytes — Telegraf
   cannot be the judge, since `config check` accepts both shapes.
3. ✅ Rendering twice is byte-identical, and insertion order does not change the
   output.
4. ✅ Escaping is centralised in `TomlValue::render` and tested against quotes,
   backslashes, tabs, newlines, non-ASCII and spaces — including an injection
   attempt that tries to close its own string and append `[[inputs.exec]]`.
5. ✅ Merging: instances sharing a `merge_key` become one, array options union,
   and the union does not depend on which module ran first.
6. ✅ Nothing outside the renderer produces TOML text.

**Brief:** §11, §18.2, §26 step 4.

---

## WP4 — Base modules

**Status: complete, 2026-08-02.**

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

**Status: complete, 2026-08-02.** Together with WP4 this reaches the milestone
the two packages exist for: rendering `config/muninn.example.yaml` produces
`docs/reference/telegraf.reference.conf` byte for byte, and that file is
accepted by Telegraf 1.39.2 (`config check`, exit 0). Run with `--test` against
the real thing it collects 49 metrics across all eight plugin instances, with
the `tmpfs` exclusions taking effect (0 of the disk metrics) and `load` and
`system` arriving as one measurement carrying `load1`, `load5`, `load15` and
`uptime`.

**Goal.** InfluxDB v2 and Prometheus, separately and together.

**Touches** `crates/muninn-modules/src/outputs/{influxdb,prometheus}.rs`.

**Done when**

0. Rendering `config/muninn.example.yaml` matches
   `docs/reference/telegraf.reference.conf` byte for byte, and
   `muninn render-config` writes to stdout or a named file, redacts secrets by
   default, and starts nothing. *(Moved here from WP3, which could not meet it —
   there is nothing to render before the modules and outputs exist.)*
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

**Status: complete, 2026-08-02.** `muninn run` works. 196 tests on Windows, all
of them plus the ten unix-only lifecycle cases on Linux via
`bash scripts/test-linux.sh`.

**What the lifecycle tests caught**, which is the reason they exist: signal
handlers were installed inside the supervise loop, leaving a window during
startup where SIGTERM still had its default disposition. A `docker stop` landing
in that window killed muninn outright instead of shutting it down. Nothing on
the development machine could have found it — the test is `#[cfg(unix)]` and
only ran once the suite was taken to Linux. Handlers are now installed before
any startup work; tokio's signal streams buffer, so a signal arriving during
startup is delivered as soon as the supervise loop first polls.

**Goal.** Start Telegraf, supervise it, and shut it down cleanly.

**Touches** `crates/muninn-telegraf/src/{validator,process,version}.rs`,
`muninn/src/{main,cli,logging,supervisor}.rs`, `muninn/tests/lifecycle_test.rs`,
`scripts/test-linux.sh`.

**Done when** — all met:

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

**Status: complete, 2026-08-02.** 223 tests.

**Goal.** Make muninn's state observable and correct.

**A structural move.** `State` and `SharedState` were in the binary crate, where
a library cannot reach them. They now live in `muninn-health` as `HealthState`,
next to the endpoints that read them — the supervisor writes, the handlers read,
and the binary wires the two. That also keeps readiness from becoming three call
sites that might disagree.

**Touches** `crates/muninn-health/src/{lib,server,state,metrics}.rs`,
`muninn/src/{main,supervisor}.rs`, `muninn/tests/lifecycle_test.rs`.

**Done when** — all met:

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
   produces `Failed`. The transition and its readiness consequence are
   implemented and tested; the *module* that triggers it lands in WP10.

Plus two the original list did not have, both worth stating because they are
what makes the endpoints trustworthy:

7. ✅ The endpoints are exercised against the **running binary**, not a
   hand-set state — `the_health_endpoints_follow_the_real_lifecycle` watches
   liveness come up before readiness and reads the durations the real startup
   path recorded.
8. ✅ `muninn healthcheck` fails when nothing is running and succeeds once the
   agent is ready, which is the contract Docker's `HEALTHCHECK` depends on.

**Brief:** §13, §16, §22, §26 step 8.

---

## WP8 — Container image

**Status: complete, 2026-08-02.** `bash scripts/container-test.sh` — 15 checks,
all under the full hardening.

**Goal.** One self-contained, hardened image.

**Unblocked by WP1**: the runtime base is `debian:12-slim`, carrying `apt` and
`dpkg` for the updates module. The hardening measures are load-bearing rather
than optional — see [`hardening.md`](hardening.md) and [R7](risks.md).

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

All met. `scripts/container-test.sh` runs every check under the posture the
documentation promises — non-root, read-only root filesystem, `--cap-drop=ALL`,
`no-new-privileges`, tmpfs — because a test that quietly relaxed one of them
would prove the image works in a configuration nobody ships.

**Two things the tests taught, both now comments in the script:**

- `cpu_usage_*` is a *delta*, so it does not exist until the second collection
  cycle. Disk figures are absolute and appear on the first flush. A single check
  right after readiness sees disks and concludes, wrongly, that CPU collection
  is broken.
- With `--cap-drop=ALL` there is no `CAP_KILL`, so **uid 0 cannot signal a
  process owned by another user** — `docker exec -u 0 … kill` fails with
  "Operation not permitted". The hardening is stricter than it looks.

**Brief:** §3.2, §9, §12, §17.1, §17.3, §18.5, §26 step 9.

---

## WP9 — Docker module

**Status: complete, 2026-08-03.** `bash scripts/container-test.sh` — 22 checks,
including the module against a real socket and through a real socket proxy.

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

All met. The rendering existed from WP4; what WP9 added is the part that makes
enabling the module a decision with a verdict.

**Three things this work changed, none of them foreseen in the plan:**

**The requirements were wrong for the recommended deployment.** They named
`/var/run/docker.sock` whatever the configuration said, so a `tcp://` proxy
endpoint failed a precondition for a file that deployment deliberately does not
have — the configuration the documentation recommends was the one that could not
start. `MonitoringModule::requirements` now takes the configuration, because for
this module the answer genuinely depends on it.

**Startup step 5 was documented but never implemented.**
[`architecture.md`](architecture.md) has listed "check runtime preconditions for
enabled modules" as a startup step since WP0, and `muninn run` only ever checked
the Telegraf version. `check-runtime` reported the problems; nothing stopped a
start. It does now — which is what makes criterion 1 true rather than aspirational.
Preconditions run *after* the version check, because a missing Telegraf binary is
a defect in the image and reporting a host-mount problem ahead of it would point
the operator at their own deployment for something they did not cause.

**The check is a request, not a connection.** muninn issues `GET /_ping` and
requires a `200`. A connect alone passes against a socket proxy running with
`PING: 0` — it accepts the connection and denies the call — and that is precisely
the deployment recommended here. Test 10 asserts the denying proxy is refused, so
the weaker check cannot come back unnoticed.

**And one thing the tests taught.** The Linux suite runs muninn inside a
container with host modules enabled. Once startup enforced the preconditions, six
lifecycle tests failed — correctly: they had been running muninn in exactly the
configuration it now refuses, where Telegraf reports the container's CPU as the
host's. `scripts/test-linux.sh` mounts `/:/hostfs:ro` and the fixture uses it, so
the suite now exercises the documented deployment instead of one muninn rejects.

**Brief:** §7.2 Docker, §17.2, §26 step 10.

---

## WP10 — Updates module

**Status: complete, 2026-08-03.** `bash scripts/updates-test.sh` — 17 cells, all
passing: the shipped image against real Debian and Ubuntu host trees, a real host
through WSL, the failure cells, and the module running end to end in a container.
Numbers identical to the spike's: 41/3 on Debian 12, 39/2 on Debian 13, 50/40 on
Ubuntu 22.04, 66 pending on Ubuntu 24.04, and 41/11 against the real WSL host.

**Goal.** Implement approach A, as specified by `spikes/updates/probe.sh`.

**Unblocked by WP1.** The probe script is the specification: preconditions first,
each failing with its own low-cardinality reason, then the simulated upgrade,
then counting. One question the spike deliberately left open — whether to ship
the shell helper and call it through `inputs.exec`, or to invoke `apt-get` from
muninn and emit the line protocol from Rust. Both run the same apt invocation,
which is the part that had to be proven.

**Touches** `crates/muninn-modules/src/updates/{mod,debian}.rs`,
`muninn/src/{cli,main,supervisor}.rs`, `scripts/updates-test.sh`,
`docs/modules.md`.

**Done when**

1. The implementation matches ADR-0009 — no substitute chosen during
   implementation without amending the ADR.
2. Metrics are as specified in `docs/spikes/updates-spike.md`.
3. A failed check emits `check_success=0` and omits the pending counts. It never
   emits zero.
4. Missing preconditions are detected at startup when the module is enabled.
5. System tests per brief §18.6, including the comparison against a native host
   check.
6. A module failure degrades muninn rather than stopping it, and is visible in
   logs, `/status` and the metrics.

All met.

**The open question, decided: muninn runs itself.** `inputs.exec` executes
`/usr/local/bin/muninn update-check`; there is no separate helper binary. The apt
argument list is the spike's, unchanged — that is the part that was measured, and
it did not move. What the port buys is that the invariant stops being a
convention: in the shell probe, "never report zero on failure" held because every
`fail()` call site remembered to exit before the counting, and in Rust the counts
live inside the `Ok` arm, so a failed check has nothing to print them from.
Recorded in [ADR-0009](adr/0009-updates-module-approach.md).

**Three things this work changed, none of them foreseen in the plan:**

**The specified metric names did not follow from the probe's fields.** The design
fixed `muninn_updates_pending{severity="all"}`; the probe emitted fields called
`pending_all` and `pending_security`, which Telegraf would have exported as
`muninn_updates_pending_all`. The shipped shape is a field named `pending` with a
`severity` tag. `status` and `reason` are on the check line in both the success
and failure cases, because a tag present only on failure gives one metric two
label sets, and both are exposed together for an expiration interval after a
check recovers.

**A failing check degrades muninn rather than stopping it** — the opposite of the
Docker module's rule, and the contrast is what makes both correct. An unreachable
Docker endpoint produces silence that reads as "no containers"; a failed update
check produces `check_success=0` with a reason. Nothing is misrepresented, so
taking a working agent out of service would cost more than it protects. muninn
runs the check once at startup so the result reaches the logs, `/status` and
`muninn_module_check_success` in seconds rather than after the first hourly
interval.

The module's *preconditions* are unchanged by this: an absent mount or a host
that is not Debian-family still refuses the start with exit 12, as for every
module.

**And the bug that refusal turned up.** The Linux suite ran the module against a
real host mount and muninn refused to start: "the host reports ID=\"\", which is
not Debian-family". The host was Debian. Docker Desktop's VM ships an
`/etc/os-release` containing only `PRETTY_NAME="Docker Desktop"` while
`/usr/lib/os-release` holds `ID=debian`, and both muninn's startup check and the
module's own preconditions read the first *file* they could open and stopped
there. The result was a confident wrong conclusion about a supported host —
precisely the failure mode this module exists to avoid, arrived at from the other
direction. Both now read the files in order and take the first non-empty value of
each field, in one shared function so the two cannot drift apart.

**An empty `/hostfs` is a missing mount.** The image creates the directory so a
bind mount has somewhere to land, so forgetting the mount left it existing and
empty — and the first implementation reported `dpkg_status_unreadable`, which is
true and points at the wrong thing. Caught by system-test cell S8 rather than by
reasoning.

**And the one that would have shipped broken.** apt takes ordinary temp files
outside `Dir::Cache` — `mkstemp /tmp/clearsigned.message.XXXXXX`, while reading
signed release files, even under `-s`. The rendered `inputs.exec` sets `TMPDIR` to
the runtime tmpfs and worked; muninn's own startup check inherited an environment
without it, hit the read-only root filesystem, and reported `apt_failed` on a host
it could read perfectly. The spike never saw this because its hardened cell had a
tmpfs on `/tmp`, which the documented deployment does not. `TMPDIR` is now set for
the apt child itself, so it holds for every caller. Cell S12 found it — the first
cell to run the module the way an operator will.

**And one thing the tests taught, which is a limit rather than a bug.** Ubuntu
copies security updates into `<release>-updates` as well as `<release>-security`.
The Ubuntu 24.04 fixture that reported 66 pending / 34 security during the spike
now reports 66 / **0** — same packages, and the host's own apt says the same
thing, because the candidate now resolves through `-updates`. The total is exact;
the security subset is a lower bound on Ubuntu. Documented at the metric and
tracked as [R8](risks.md), with what a more thorough classification would cost.

**Brief:** §7.2 Updates, §8.3, §18.6, §26 step 11.

---

## WP11 — End-to-end tests

**Status: complete, 2026-08-03.** `bash scripts/integration-test.sh` — 24 cells,
all passing, against a stack of muninn, Telegraf, InfluxDB 2.7 and Prometheus
3.5. Plus 12 binary-level failure-path tests in
`muninn/tests/secrets_and_mounts_test.rs`.

**Goal.** Prove the whole path, not the pieces.

**Touches** `scripts/integration-test.sh`, `docker-compose.integration.yml`,
`config/prometheus.integration.yml`, `muninn/tests/secrets_and_mounts_test.rs`,
`muninn/src/generated_config.rs`.

**Done when**

1. The ten steps of brief §18.3 run against a real Telegraf: load, generate,
   validate, start, confirm running, readiness, scrape Prometheus, find a real
   system metric, shut down, confirm cleanup.
2. A temporary InfluxDB container receives metrics and a query confirms the
   expected measurements, with throwaway credentials only.
3. A Telegraf crash is injected and detected.
4. Secrets and mounts have failure-path tests, not only happy paths.

All met, as cells I1–I17 and the twelve tests in `secrets_and_mounts_test.rs`.

**A real Prometheus, not a curl of the endpoint.** The brief's step 7 says
"scrape Prometheus", and curling `:9273/metrics` looks like it satisfies that.
It does not: a malformed exposition line is still bytes over HTTP, so a curl
that finds `cpu_usage_idle` proves the string exists and nothing about whether
Prometheus would accept it. The stack runs Prometheus 3.5 against both
endpoints and asserts on `/api/v1/query`, which is the same claim actually
tested. Scraping *both* is deliberate and is [R2](risks.md): `:9273` alone
cannot distinguish a dead agent from a dead host, because both look like a
target that stopped answering.

**Two bugs, both found by the read-only root filesystem — the same place WP10's
apt bug came from.**

`muninn validate --with-telegraf` **failed inside the shipped image.** It
rendered its scratch file through `tempfile`, which writes to `/tmp`, and the
hardened container has a read-only root filesystem — so the one command an
operator would run *in the container* to check a configuration exited 30 with
`Read-only file system`. It now writes into the directory
`runtime.generated_config_path` names, the tmpfs the deployment already
provides, and falls back to the system temp directory when that does not exist
so the command still works on a laptop. It does not create the directory: doing
so would have muninn making a directory outside a container to hold a resolved
credential.

**`render-config --output` wrote a world-readable file.** With
`--unsafe-show-secrets` that file holds a real token. The supervisor's own
writer had taken care to produce 0600 since WP6; this second writer, added in
WP4, used a plain `fs::write`. Both now go through `muninn/src/generated_config.rs`
— one writer, one permission rule — and the mode is set at creation rather than
by a `set_permissions` call afterwards, which closes the window in which the
file exists with the umask's mode and already contains the token.

**And one left over from an earlier fix.** `runtime.host_mount_prefix` was still
validated with `starts_with('/')`, the Linux-shaped approximation of "absolute"
that was corrected for `runtime.generated_config_path` during WP6 and not for
this key. Harmless in production, where the value is `/hostfs`; it meant the
mount tests could not point the prefix at a real directory on the machine they
run on, which is the reason the shared helper exists.

**Brief:** §18.3, §18.4, §26 step 12.

---

## WP12 — CI/CD and release

**Status: complete, 2026-08-03.** Twelve jobs in `ci.yml`, plus Semgrep, the
release workflow, Dependabot and the branch automation — built to huginn.io's
shape, with three jobs it does not have.

**Goal.** Every gate automated, images published reproducibly.

**Touches** `.github/workflows/{ci,security,release,auto-pr,dependabot-auto-merge}.yml`,
`.github/dependabot.yml`, `scripts/{changelog-version,test-report}.sh`,
`docs/ci-cd.md`.

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

All met.

**Three jobs huginn.io does not have**, each covering something no Rust test can
see. `reference` re-checks that the pinned Telegraf still accepts
`telegraf.reference.conf` — the file every snapshot is anchored to, so if it
stops being valid the suite stays green and the artefact is wrong — plus the
ADR-0007 ordering fixtures and every plugin option named in `modules.md`
([R5](risks.md)). `integration` runs the stack test *and* the hardened-image
tests. `updates` runs the module against real Debian and Ubuntu trees in its own
job, because building those fixtures is minutes of apt work that should not sit
in front of the stack test.

`reference` also asserts that `ci.yml`'s `TELEGRAF_VERSION` matches the
Dockerfile's, so the reference can never be verified against a version the image
does not carry.

**`test` and `coverage` extract Telegraf from the pinned image.** Without
`MUNINN_TELEGRAF_BIN` the tests that need a real one skip loudly, and CI would
report a green suite that never started a child process — the exact bug class
`muninn/tests/` exists for.

**O2 is decided: Docker Hub first, ghcr mirrored from the finished manifest.**
`skopeo copy --all` copies the manifest list and every blob, so both registries
carry byte-identical images with the same digests, from one build. The cost is
that publishing now needs a `DOCKERHUB_USERNAME` variable and a
`DOCKERHUB_TOKEN` secret, which the agent must not create — `push` fails with a
message naming them, and [`ci-cd.md`](ci-cd.md) carries the checklist.

**What is deliberately not a required check.** `build`, `scan`, `integration`
and `updates` run on every PR and gate `publish`, but requiring them for merge
would make a documentation typo wait for two container builds. Nothing
unscanned ships either way, because `publish` needs all three.

**One thing this work changed in the tree.** Before it, `CHANGELOG.md` had only
`## [Unreleased]`, and a pipeline that reads the version from the changelog
cannot run at all in that state. `scripts/changelog-version.sh` grew an
`--allow-unreleased` fallback to the workspace version, used by `publish` so
pushes to `dev` keep producing a `:x.y.z-dev` image before the first release —
and deliberately *not* by the version gate, so a real release still has to name
its version in the changelog.

**Brief:** §19, §17.3, §26 step 13.

---

## Definition of done for the MVP

The twenty-seven acceptance criteria in brief §27. The one that outranks the
rest, from §26 step 14: a new user can start muninn on a Debian or Ubuntu host
using only the README, without ever seeing Telegraf's TOML.
