# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release pipeline reads the version from this file — see
[`docs/ci-cd.md`](docs/ci-cd.md). Never hand-push a `v*` tag.

## [0.2.0] - 2026-08-07

### Added

- **Three guides taken from huginn.io**, the ones you reach for when something
  is already wrong. [`docs/troubleshooting.md`](docs/troubleshooting.md) is
  symptom, cause and fix, organised by what you actually see — an exit code, a
  metric reading zero, a fresh time series after every recreate — and it says
  which behaviours are intended rather than broken: `Degraded` staying ready, no
  internal restart loop, and a security count of zero on Ubuntu being a lower
  bound. [`docs/workflows.md`](docs/workflows.md) explains every workflow, its
  triggers and its gotchas. [`docs/releasing.md`](docs/releasing.md) is the
  runbook for both release paths, with the verification commands; the release
  passages stay in `ci-cd.md` only as far as the *why*.
- **A coverage percentage in the Actions job summary**, computed from
  `lcov.info`'s `LF`/`LH` records, plus a coverage-gate badge in the README.
- **`.vscode/extensions.json` and a committed `.claude/settings.json`**, shared
  with huginn.io. The settings file is deliberately read-only — gates,
  inspection and `gh`/`git` queries, nothing that writes, pushes or merges.

### Changed

- **`release-dispatch.yml` merges `dev → main` with a merge commit**, not a
  squash. `AGENTS.md` §8 and `docs/CONTRIBUTING.md` both say that merge keeps
  history, and 0.1.0 and 0.1.1 were squashed anyway because merge commits were
  the only method the repository had not enabled — so the documented branch
  model was false for the two releases that exist. The setting is enabled now
  and recorded with the rest of the by-hand checklist in `docs/ci-cd.md`.
- **The base images moved forward**: the Rust builder and the debian-slim
  runtime, the latter to a new Debian major release. Verified by the full
  pipeline, including the updates module against real Debian and Ubuntu host
  trees.
- **No version number is repeated in prose.** A version lives where it is the
  authority — `rust-version` in `Cargo.toml`, the pins in the `Dockerfile`, a
  fixed-version in `.trivyignore.yaml` — and documentation names the field
  instead, so a Dependabot bump no longer leaves a dozen sentences quietly
  wrong. Measurements and historical incidents keep their numbers and now say
  *when* they were taken: the tables in
  [`docs/updates-evidence.md`](docs/updates-evidence.md), the ADR-0009 cells and
  the base-image comparison in `hardening.md` are records, not claims about
  today. The rule is a convention in `AGENTS.md` §7.
- **Every document ends with a `## Related` footer** — eight did not — and the
  doc map, README shape and `AGENTS.md` layout now match huginn.io's exactly.
  The two projects are kept aligned deliberately, so a change to either has an
  obvious counterpart in the other.
- **`AGENTS.md` §8** records that `auto-pr.yml` deletes a branch whose name does
  not match the prefix list, and §3 that the same prefixes are what a PR branch
  must use.

### Security

- **Four further rclone findings are suppressed** with an expiry and a
  reachability argument: `CVE-2026-54572`, `CVE-2026-59733`, `CVE-2026-71309`
  and `CVE-2026-71312`. Trivy began reporting them on 2026-08-06 against an
  image nothing had changed, and all four are marked fixed upstream — but no
  Telegraf release carries the fixed rclone, so `ignore-unfixed` does not filter
  them and there is no version to move to. Each requires rclone to be
  *executed*, as a sync client, a `serve restic` server or against an SFTP
  backend; muninn never executes rclone, and cannot be made to
  ([ADR-0004](docs/adr/0004-no-raw-toml.md)). All six entries expire together on
  2026-11-03.

## [0.1.1] - 2026-08-06

Fixes the release path itself. muninn's own code is untouched — the image
`0.1.1` publishes is `0.1.0` rebuilt from the same sources.

### Added

- **A one-click release** — `release-dispatch.yml`, taken from huginn.io. Pick
  `patch`, `minor` or `major`; it computes the version from the last release,
  stamps the changelog and `Cargo.toml`, and opens the release PR into `main`.
  Owner-only, and it refuses an empty `## [Unreleased]`: a version that
  documents nothing is worse than no release, because the changelog is what
  tells an operator whether to upgrade. It is an entry point, not a second
  release path — it produces the same PR the manual flow does, and every gate
  still runs on it. Three things differ from huginn.io's copy, each because a
  rule here requires it: the changelog version is read through the validating
  `scripts/changelog-version.sh` rather than a bare `grep`, the `Cargo.toml`
  edit is scoped to `[workspace.package]`, and the merge is a squash because
  that is the only method this repository enables.

### Fixed

- **`release.yml` never fired for `v0.1.0`, and could not have.** `ci.yml`'s
  `publish` pushes the tag with the built-in `GITHUB_TOKEN`, and GitHub does not
  start a workflow from an event that token created — the recursion guard. So
  the image, the ghcr mirror and the tag all shipped, and the GitHub Release,
  the SBOM, the test report and the housekeeping PR did not. The tag push now
  uses `RELEASE_PAT` where it is available, which is the same reason
  `prepare-dev` already needed it, and `release.yml` gains a
  `workflow_dispatch` entry point so an existing tag can be released — or
  re-released — without touching the tag itself.
- The `## [Unreleased]` block and the changelog's compare links, reopened by
  hand here because the workflow that does it never ran.
- **`scripts/test-report.sh` could never have parsed a real CI log.** Cargo
  colours its `Running` status lines even when its output is piped to a file,
  and the escape sequences sit before the word — so the anchored pattern that
  opens a test suite never matched, every `test result:` line was discarded as
  belonging to no suite, and the script exited with *no `test result:` lines
  found in input* against a log holding ten of them. It now strips ANSI before
  parsing, which is the right layer: the input is a captured log, and a parser
  that only works on logs captured one particular way breaks on the next
  caller. Verified against the exact bytes of the failing run — 9 suites, 402
  tests, 91.24 % line coverage.

  This is the second thing v0.1.0 found by being the first release ever cut,
  and both were invisible until then. Nothing before it had run
  `release.yml`.
- **A failing test suite could not fail the release run.** `cargo llvm-cov …
  | tee` takes its exit status from `tee`, and GitHub's default shell has no
  `pipefail` — so a red suite left the step green, and the only thing between
  it and a published Release was the report generator noticing a non-zero
  failure count. That step now runs under `shell: bash`, which brings
  `pipefail`.
- Building the report is now best-effort, and the Release no longer depends on
  it: tests are the gate, the report only describes them. When it cannot be
  built the run says so in a warning and the notes omit the test section
  rather than claiming a verdict they cannot show. This is also what lets an
  older tag be released by dispatch — such a run executes *that tag's* copy of
  the script, bug included.
- **Every automated version bump would have produced a red PR.** Both places
  that raise the version — `release.yml`'s housekeeping and the new dispatch —
  edited `Cargo.toml` and left `Cargo.lock` behind, and every CI job runs
  `--locked`, so the next job to start would have died on *cannot update the
  lock file* before a single test ran. New `scripts/set-workspace-version.sh`
  sets both, and both callers use it. Which packages are the workspace's is
  read from the lock file itself — they are the ones with no `source` line —
  rather than from a hard-coded list of crate names that would need keeping in
  step. Verified byte-identical to `cargo update --workspace`.

  It does the job without invoking cargo on purpose. Both callers hold a write
  token, and "no job runs cargo with a write token" is a security property this
  repository shipped rather than a preference. Dependency resolution executes no
  build script, so the rule would arguably permit it — but an invariant that
  holds except where someone reasoned it away is not an invariant.
- **Dependabot targeted `main`**, because that is the default branch and
  `dependabot.yml` never said otherwise — so every bump landed on `main` without
  passing through `dev`, and the next `dev → main` release PR reverted it.
  Found while opening the 0.1.1 release PR: it would have downgraded
  `codeql-action/upload-sarif` from v4.37.5 back to v4.37.4, the bump that had
  merged into `main` an hour after v0.1.0. Every ecosystem now carries
  `target-branch: dev`, and that bump is carried into `dev` here so the release
  PR no longer reverses it. A silent downgrade of a security-scanning action is
  the kind of change that is only ever noticed by looking for it.

  The recovery itself then made the same mistake in miniature: the bump touched
  two files, `security.yml` and `ci.yml`, and only the first was carried over,
  because it was the one the release diff happened to show first. Every `uses:`
  pin in every workflow is now compared between the two branches rather than
  chased one grep at a time — one difference remained, and it was this one.

## [0.1.0] - 2026-08-06

First release. Everything below is what muninn is on day one.

### Security

Findings from a full audit of the repository and the pipeline. No exploitable
vulnerability was found; these close assumptions about things this repository
does not control, which is the class it closes everywhere else.

- **No CI job runs `cargo` with a write token any more.**
  `actions/checkout` persists its token into `.git/config`, and `cargo`
  compiles and executes `build.rs` and proc-macros from every dependency in the
  tree — so a compromised crate could read a token that a job never needed on
  disk. Every checkout now sets `persist-credentials: false` except the two that
  push, and `release.yml` is split: `test-report` runs the suite with
  `contents: read` and uploads the report; `github-release` downloads it and
  runs no third-party code.
- **Credentials no longer pass through argv**, where `/proc/<pid>/cmdline` makes
  them readable to every process on the runner. `skopeo login --password-stdin`
  with a `0600` `REGISTRY_AUTH_FILE` replaces `--dest-creds`; `curl --data @-`
  and `-K -` replace `-d` and `-H` for the DockerHub session token.
- **`security-events: write` is scoped to the job that uploads SARIF**, instead
  of being granted workflow-wide to ShellCheck and actionlint as well.
- **`cargo-deny` is pinned** (`--version 0.20.2`). It was deliberately unpinned
  for freshness — but the freshness comes from the advisory database, fetched at
  run time, not from the tool binary. Pinning costs no coverage and removes an
  unpinned `cargo install` from every run.
- **Base images are pinned by digest** as well as by tag. Dependabot updates the
  pair together, so this costs no manual upkeep — the same weekly PR as before,
  with the digest in it.
- **CI takes Telegraf from the checksum-pinned tarball, not a mutable image
  tag.** New `scripts/fetch-telegraf.sh` reads the version and the per-arch
  SHA-256 out of the `Dockerfile` and verifies the download, so there is one pin
  — the one [ADR-0011](docs/adr/0011-telegraf-pinning.md) already describes —
  rather than a second one to keep current by hand.
- **Telegraf's output is scrubbed of known secrets before muninn logs it.**
  muninn re-emits the child's stdout and stderr through its own logger, and the
  configuration Telegraf reads holds resolved secrets. `Secret`'s redaction is a
  property of the type and so cannot reach text another process formatted; a new
  `Redactor`, built from the resolved configuration, closes that. Whether
  Telegraf ever quotes a config value in a diagnostic is a property of Telegraf
  — this no longer depends on the answer. See
  [`docs/hardening.md`](docs/hardening.md#secrets).

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
  `serde_json` as a dependency, in `muninn-modules` only.

  Repository names are normalised before they are compared, so a container
  created as `docker.io/library/nginx` is judged rather than dismissed as a
  different repository; an image ID where a reference belongs gets its own
  reason (`image_id_reference`) instead of a true answer to a nonsense
  question. The registry lookup has its own, longer timeout
  (`registry_timeout`, default `30s`) because it is the one call that leaves
  the host. The run carries a budget of half its interval: containers not
  reached report `budget_exceeded` rather than the whole helper being killed by
  Telegraf holding results it had already found, and the rendered `inputs.exec`
  timeout is derived from that budget so the ordering holds by construction.

  Two guards close the same class of hole in both directions: every request
  path the module's Docker API client builds is checked for a control character
  or a space before it is sent, and control characters in the container name
  and image reference are replaced before they reach influx line protocol —
  request-line injection on one side, a fabricated metric series on the other,
  both from strings muninn did not choose. Found in a security review before
  this module's first release.

  The Docker API client reassembles chunked responses. It first refused them by
  name, on the reasoning that none of these endpoints streams — the daemon
  chunks all three anyway, so the module reported `docker_unreachable` against
  every real daemon. Every unit test passed; `scripts/image-updates-test.sh`,
  which runs the whole path against a live daemon including a deliberately
  stale container, found it on its first run. Recorded in `AGENTS.md` §6 so it
  is not undone.
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
  work landed, and the quick start named `ghcr.io/…:0.1.0`, a tag that did not
  exist. The quick start pinned `0.1.0-dev` until this release cut the tag it
  had been naming; it now pins `0.1.0`.

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

This is a `0.x` release: the surfaces [`docs/versioning.md`](docs/versioning.md)
names are the ones muninn intends to keep, and it has not yet run long enough
anywhere to promise a `1.0`. Every command works — `run`, `validate`,
`render-config`, `check-runtime`, `healthcheck`, `update-check`, `image-check`
and `version` — and the container image builds for `linux/amd64` and
`linux/arm64` and passes its tests under the full hardening. `0.1.0` is the same
pipeline output the pre-release tags `0.1.0-dev` and `dev` have been carrying,
published from one build to `jschnabel/muninn` and mirrored byte-identically to
`ghcr.io/joshua-schnabel/muninn.io`.

Three findings during the design package changed the design away from the
project brief:

- Validation uses `telegraf config check`, not `--test`, so it never binds the
  port the real process needs.
- Exclusion options do not exist on the relevant plugins and must render as
  `tagdrop` sub-tables, which forces a declared render order rather than sorted
  keys.
- There is no `inputs.load`; the `load` and `system` modules merge into one
  plugin instance.

[Unreleased]: https://github.com/joshua-schnabel/muninn.io/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/joshua-schnabel/muninn.io/releases/tag/v0.2.0
[0.1.1]: https://github.com/joshua-schnabel/muninn.io/releases/tag/v0.1.1
[0.1.0]: https://github.com/joshua-schnabel/muninn.io/releases/tag/v0.1.0
