# AGENTS.md

Canonical context for AI coding agents working on **muninn.io**. Single source of
truth for how to work here; every tool (Claude Code, Cursor, Aider, Gemini CLI, …)
should read it first. Human-facing depth lives in the linked docs — this file
summarises and points, it does not duplicate.

---

## 1. What this project is

muninn.io is a **supervisor and configuration layer around Telegraf**, written in
Rust. An operator writes one small YAML file; muninn validates it, generates a
complete Telegraf configuration, has Telegraf verify that configuration, starts
Telegraf as a child process, and then supervises it and serves health endpoints.
It ships as one hardened, multi-arch container holding both binaries.

Telegraf is the telemetry engine. muninn never touches a metric.

**Status: feature-complete; nothing is published yet.**
[`docs/roadmap.md`](docs/roadmap.md) carries what is still open and is the one
place it is tracked — do not restate it here, it goes stale. What already shipped
is in [`CHANGELOG.md`](CHANGELOG.md). Start with
[`docs/architecture.md`](docs/architecture.md).

Sibling project: [huginn.io](https://github.com/joshua-schnabel/huginn.io), same
maintainer. muninn takes its conventions.

---

## 2. Working with the maintainer

- **Language:** reply to the maintainer (**Joshua**) in **German**. Keep
  everything committed — code, comments, commit messages, docs, this file — in
  **English**.
- **Autonomy (solo project, Joshua is sole maintainer):** act pragmatically
  within the guardrails in §3. Execute the task, report concisely, ask only at
  genuine forks. Land work as a **pull request**, never directly on a protected
  branch.
- **Verify before changing.** Confirm versions, API shapes and facts against the
  actual source or official docs before editing. This project has already found
  three places where a plausible assumption about Telegraf was wrong — see §6.
  When unsure, check.
- **Security is a first-class priority.** muninn handles credentials, mounts the
  host filesystem, and can be given the Docker socket. Weigh every change through
  a security lens and call out anything with a security dimension. §9 is not
  optional polish.
- **Don't duplicate.** Reuse existing helpers; when documenting, link the
  canonical page rather than copying it.

---

## 3. Hard rules — never without explicit approval

Stops, not preferences. Ask first, every time:

1. **Never push to `main` or `dev`.** All changes go through a PR from a
   `feature|fix|chore|docs|test/<name>` branch.
2. **Never merge or approve PRs.** Opening them is fine.
3. **Never change repository settings, secrets or rulesets.** No Actions secrets,
   branch protection or repo configuration.
4. **Never add a dependency, and never rewrite history or force-push**, without
   asking. New crates change the supply-chain surface; history rewrites are
   destructive.
5. **Never accept a snapshot without reading the diff.** `cargo insta accept` on
   an unread diff turns the test suite into a record of whatever the code happens
   to do. Use `cargo snap-review`.

Everything else — editing code and docs, opening PRs, running the gates — is fair
game.

---

## 4. Architecture & where things live

Cargo workspace; one bounded responsibility per crate:

| Crate | Role |
|---|---|
| `muninn/` | Binary: CLI, logging init, startup sequence, supervisor wiring |
| `crates/muninn-core/` | Config model, loading, validation, secrets, durations, errors, exit codes |
| `crates/muninn-telegraf/` | Typed Telegraf model, TOML renderer, `config check` validator, child process, version check |
| `crates/muninn-modules/` | `MonitoringModule` trait, eleven modules, two outputs |
| `crates/muninn-health/` | Liveness, readiness, status, self-metrics |

Dependencies point one way: `muninn` → everything; `muninn-modules` →
`muninn-telegraf` → `muninn-core`; `muninn-health` → `muninn-core`. No cycles.
`muninn-core` knows nothing about Telegraf.

**Control flow** — a state machine over one child process, not a data pipeline:

```
config → validate → check runtime → render → write → telegraf config check
       → spawn telegraf → supervise → forward signals → wait → exit
```

Other locations: `config/` (the example YAMLs), `docs/reference/` (the verified
target TOML and the ordering fixtures), `docs/adr/`, `scripts/` (the system test
suites and the fixture builders they use), `deny.toml`, `.cargo/config.toml`.

---

## 5. Commands (the gates)

```bash
cargo fmt --all -- --check                                # format
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --locked
cargo deny check                                          # advisories, licences, bans, sources
cargo llvm-cov --workspace --lcov --output-path lcov.info --fail-under-lines 80
cargo build --release --locked
```

Aliases for all of these are in `.cargo/config.toml` (`cargo lint`, `cargo t-all`,
`cargo cov-ci`, `cargo audit-all`).

The design package's own verification, which CI does not yet run:

```bash
# the reference TOML must be accepted by the pinned Telegraf
docker run --rm -v "$PWD/docs/reference:/ref:ro" telegraf:1.39.2 \
  telegraf config check --strict-env-handling --config /ref/telegraf.reference.conf
```

CI runs all of these on every PR, plus the image build, Trivy, Semgrep and the
three system suites — see [`docs/ci-cd.md`](docs/ci-cd.md). Run them locally
anyway before pushing: the image jobs take tens of minutes, and a red pipeline
is a slower way to learn that `cargo fmt` was not run.

---

## 6. Three Telegraf facts that shape the code

Each cost real investigation and each contradicts a plausible assumption. Do not
undo them.

**`telegraf config check`, not `--test`.** `config check` initialises plugins
without starting them. `--test` runs a collection cycle, which means
`outputs.prometheus_client` binds `:9273` — the port the real process is about
to need — and it prints `Outputs are not used in testing mode!`, so it does not
validate the output path it appears to.
[ADR-0006](docs/adr/0006-validate-with-config-check.md)

**The renderer must not sort keys.** Exclusions have no plugin options and render
as `tagdrop` sub-tables, which Telegraf requires at the *end* of a plugin block.
Alphabetical ordering puts the table first and silently breaks every option after
it — measured: 5 disk metrics versus 15, and `config check` exits 0 both ways.
Fixtures in `docs/reference/ordering-{correct,broken}.conf`.
[ADR-0007](docs/adr/0007-tagdrop-and-render-order.md)

**There is no `inputs.load`.** Load, uptime and users are groups of
`inputs.system`, so the `load` and `system` modules merge into one instance. Two
instances would collect every metric twice, and nothing would complain.
[ADR-0008](docs/adr/0008-system-and-load-merge.md)

---

## 7. Conventions

### Coding style

- **Match the surrounding code** — naming, module layout, comment density,
  idioms. Consistency beats preference.
- `snake_case` items, `CamelCase` types, descriptive names. Test names read as
  English sentences (`rejects_unknown_key`), not `test_*` or `should_*`.
- **No `unwrap()` / `expect()` / `panic!` in non-test code.** Return a `Result`
  and propagate with `?`. Panics are for tests and genuinely unreachable
  invariants, with a comment saying why.
- Small, single-purpose functions; tight crate surfaces (`pub` only what other
  crates need).
- Doc-comment (`///`) public items. Comments explain the **why**, not the what.
- `cargo fmt` defaults (no `rustfmt.toml`); **fix** clippy rather than
  `#[allow]`-ing it, and if an allow is genuinely needed, give a one-line reason.
- Avoid `unsafe`. Adding a dependency needs approval (§3).

### Project idioms

- **Errors:** `MuninnError` (`thiserror`) in `muninn-core::error`, with
  `type Result<T>`; `anyhow` only at the binary boundary. Exit codes live in
  `muninn-core::exit` and are a public contract —
  [`docs/supervision.md`](docs/supervision.md).
- **Config:** every struct carries `#[serde(deny_unknown_fields)]`. A typo'd key
  must fail the load. Durations are `30s`/`5m`/`1h`, never bare integers.
- **Secrets:** file paths only, never inline values. Wrapped in a type whose
  `Debug` and `Display` render `***`, with `expose()` called in exactly one
  place. Errors name the path, never the contents.
- **Async:** single tokio runtime; long-running components are `tokio::spawn`ed;
  `tokio::select!` multiplexes shutdown with timers; `#[tokio::test]` for async
  tests.
- **Logging:** `tracing` + `tracing-subscriber` (`EnvFilter`), human by default,
  JSON via `logging.format`. Structured fields, not interpolated prose.
- **Rendering:** no module builds TOML with `format!`. Everything goes through
  the typed model and the one serialiser —
  [`docs/telegraf-rendering.md`](docs/telegraf-rendering.md).
- **MSRV 1.88**, edition 2024, resolver 3. Note resolver 3 is MSRV-aware and will
  hold a dependency back rather than require a newer compiler — which is how a
  project silently stays on the release before a CVE fix. If the floor starts
  constraining resolution, **raise the floor**, do not accept the old crate.
  The Docker build is the real MSRV gate.
- **Testing:** unit tests inline in `#[cfg(test)]`; cross-crate and whole-binary
  behaviour in `muninn/tests/`. **Don't sleep — poll.** Tests touching the
  environment must be serialised. ≥80 % workspace line coverage.
  [`docs/testing.md`](docs/testing.md)

---

## 8. Git & PR workflow

- Branch off `dev` with a valid prefix: `feature/` · `fix/` · `chore/` · `docs/`
  · `test/`.
- Flow: `feature/* → dev` (squash) → `main` (merge commit). No direct pushes
  (§3).
- **Conventional Commits:** `feat · fix · chore · docs · test · refactor · perf ·
  style`. End commit bodies with the `Co-Authored-By` trailer.
- Commit messages explain **why**, and state what was verified. "Verified: X
  passes" beats "should work".

---

## 9. Security posture — high priority

muninn mounts the host filesystem read-only, handles credentials, and can be
given the Docker socket. Any change touching secrets, the container, mounts,
network exposure, dependencies or workflow permissions must be reasoned about
explicitly and flagged in the PR.

- **Secrets are file paths only.** Never a YAML value, never an environment
  variable, never logged. The redaction is a property of the type, not a
  convention.
- **The generated config holds resolved secrets** and therefore lives on a tmpfs,
  root-only, never persisted, never mounted out.
  [ADR-0003](docs/adr/0003-ephemeral-generated-config.md)
- **The host mount is real exposure.** `/:/hostfs:ro` includes `/etc/shadow`.
  Documented plainly in [`docs/host-mounts.md`](docs/host-mounts.md), not softened.
- **The Docker socket is root-equivalent**, and `:ro` does not change that.
  Module off by default, proxy recommended, read-only API use.
  [ADR-0010](docs/adr/0010-docker-socket.md)
- **Container:** non-root, read-only root filesystem, `no-new-privileges`, all
  capabilities dropped, tmpfs at `/run/muninn`. Never `--privileged`.
- **Supply chain:** Telegraf pinned by checksum
  ([ADR-0011](docs/adr/0011-telegraf-pinning.md)), `Cargo.lock` committed,
  cargo-deny gating advisories and licences, OpenSSL banned outright.
- **A failed check never reports a healthy value.** This is the project's
  sharpest rule: `0 updates` when the check failed is worse than no metric at
  all, because an alert rule cannot tell them apart afterwards.

---

## 10. Doc map

| Topic | Read |
|---|---|
| What is still open | [`docs/roadmap.md`](docs/roadmap.md) |
| Architecture, startup, state machine | [`docs/architecture.md`](docs/architecture.md) |
| Config reference (every key) | [`docs/configuration.md`](docs/configuration.md) |
| Module reference | [`docs/modules.md`](docs/modules.md) |
| How the TOML is generated | [`docs/telegraf-rendering.md`](docs/telegraf-rendering.md) |
| Signals, exit codes, diagnosis | [`docs/supervision.md`](docs/supervision.md) |
| What to mount and why | [`docs/host-mounts.md`](docs/host-mounts.md) |
| Testing pyramid, coverage, no-sleep rule | [`docs/testing.md`](docs/testing.md) |
| Container hardening | [`docs/hardening.md`](docs/hardening.md) |
| Vulnerability reporting | [`docs/SECURITY.md`](docs/SECURITY.md) |
| Contributor workflow | [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) |
| SemVer policy, stable surface | [`docs/versioning.md`](docs/versioning.md) |
| CI/CD and repository setup | [`docs/ci-cd.md`](docs/ci-cd.md) |
| Open risks | [`docs/risks.md`](docs/risks.md) |
| Architecture decisions | [`docs/adr/`](docs/adr/) |
| Why the updates module works | [`docs/updates-evidence.md`](docs/updates-evidence.md) |
