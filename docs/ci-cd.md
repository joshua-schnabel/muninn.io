# CI/CD and repository setup

> **Status: shipped (WP12).** The workflows are in `.github/workflows/`. This
> page describes what they do and lists the settings a maintainer still has to
> apply by hand — the pipeline cannot configure its own branch protection or
> create its own credentials.
>
> **Before the first push to `dev` after this lands**, add the two Docker Hub
> settings in [Repository settings](#repository-settings--maintainer-by-hand).
> Without them `push` fails with a message naming them, and nothing is
> published.

## Pipeline shape

Adapted from huginn.io, whose central property is worth restating: **the image is
built once per architecture into an artefact, and every later job consumes that
same artefact.** The bytes that are scanned, integration-tested and published are
byte-identical. A pipeline that rebuilds between scanning and publishing has not
scanned what it published.

```
check ─┬ test (stable + beta canary) ─┐
       ├ supply-chain ────────────────┤
       ├ coverage (needs test) ───────┤
       ├ reference ───────────────────┤
       └ version-gate ────────────────┤
                                      ▼
            build (per arch, native) → image.tar artefact
               ├ scan        (Trivy on the artefact)
               ├ integration (compose stack + hardened image)
               └ updates     (the module against real host trees)
                                      ▼
            push (per arch, push events only) → digest artefact
                                      ▼
            publish → multi-arch manifest, ghcr mirror, tag
                                      ▼
            release.yml → GitHub Release + housekeeping PR into dev
```

`publish` needs `scan`, `integration` **and** `updates`, so nothing unscanned or
untested can ship.

## Jobs

| Job | Runs | Blocks on |
|---|---|---|
| `check` | `cargo fmt --check`, `cargo clippy -D warnings` | any warning |
| `test` | `cargo test --workspace --locked` on stable and beta | stable only — beta is a non-blocking canary for the next compiler |
| `supply-chain` | `cargo deny check` | advisories, disallowed licences, banned crates, unknown registries |
| `coverage` | `cargo llvm-cov --fail-under-lines 80` | under 80 % workspace lines |
| `reference` | `scripts/verify-design-package.sh` (checks 2–7) | the pinned Telegraf rejecting the reference config, a documented plugin option that does not exist, a checksum mismatch, a broken doc link |
| `version-gate` | CHANGELOG version is valid SemVer and greater than the last tag | invalid or non-successor version |
| `build` | Docker build per architecture into `image.tar` | build failure, Telegraf checksum mismatch |
| `scan` | Trivy on the artefact | **fixable** CRITICAL/HIGH |
| `integration` | Load the image; `integration-test.sh` then `container-test.sh` | any assertion |
| `updates` | Load the image; `updates-test.sh` against real Debian and Ubuntu trees | any assertion |
| `push` / `publish` | Push by digest, assemble the manifest, mirror to ghcr, create the tag | — |

`version-gate` **always runs** and decides internally whether to enforce. A
skipped job in `needs:` skips its dependents, so it must not be conditional at
the job level — it is a deliberate no-op pass on non-release events.

`test` and `coverage` extract Telegraf from the pinned image and set
`MUNINN_TELEGRAF_BIN`. Without it the tests that need a real Telegraf skip
loudly, and CI would report a green suite that never started a child process —
which is the class of bug `muninn/tests/` exists for.

`scan` blocks on **fixable** CRITICAL/HIGH only. The runtime base is
`debian:12-slim` rather than distroless because the updates module needs real
apt and dpkg; the cost of that trade is measured in
[`hardening.md`](hardening.md) and is CVEs with no fix available. A gate that
blocks on findings nobody can act on is a gate that gets switched off.

### Three jobs huginn.io does not have

All three come from WP0 findings, and each covers something no Rust test can
see.

**`reference`.** `docs/reference/telegraf.reference.conf` is what the renderer
targets and what every snapshot is anchored to. If the pinned Telegraf stops
accepting it, the snapshots still pass and the artefact is wrong. The same job
re-checks the ordering fixtures behind [ADR-0007](adr/0007-tagdrop-and-render-order.md)
— both pass `config check`, and only the emitted metric count tells them apart —
and cross-checks every `plugin.option` named in [`modules.md`](modules.md)
against the pinned Telegraf's `sample.conf`, which is what catches documentation
drifting away from the shipped version ([R5](risks.md)).

It also asserts that the workflow's `TELEGRAF_VERSION` matches the Dockerfile's,
so the reference can never be verified against a version the image does not
carry.

**`integration`** runs both system suites, not just the stack: the compose stack
proves a metric reaches a database, and `container-test.sh` proves the image
still behaves under the full hardening, including the Docker module against a
real socket and through a socket proxy.

**`updates`** is its own job because it builds Debian and Ubuntu fixture trees,
which is minutes of apt work that should not sit in front of the stack test.
Cell S11 — a real host through WSL — **skips** on a runner and says so; the
other sixteen run. It is the only cell that can skip, and it is counted and
printed separately from a pass, because a skip that reads like a pass is worse
than no cell at all.

## Source scanning

`security.yml` runs on every push and every PR, and needs no build:

| Job | Covers |
|---|---|
| `shellcheck` | `scripts/*.sh` at `--severity=warning`. The shell here is not glue — those files *are* the three system test suites, and a quoting bug in one is a test that passes without testing |
| `actionlint` | The workflows themselves: unknown action inputs, bad job dependencies, and shell errors inside `run:` blocks |
| `semgrep` | `p/rust` and `p/secrets`, two passes — full scan to the Security tab, then a blocking pass on ERROR severity |

Semgrep has no registry ruleset for shell (`p/bash` and `p/shell` are both 404),
which is why ShellCheck is a separate job rather than another `--config`.

actionlint runs from a digest-pinned image rather than through the upstream
install script: `curl | bash` from a moving branch is exactly the supply-chain
shape this repository refuses everywhere else.

## Suppressed image findings

The blocking Trivy scan reads `.trivyignore.yaml`; the full scan deliberately
does not, so a suppressed finding still reaches the Security tab. Every entry
needs an expiry date and a reason the code is unreachable in muninn's generated
configuration — not merely "not fixed upstream yet".

Two entries today, both Go modules vendored into the Telegraf binary rather than
muninn's own dependencies. The reasoning, and the table of what they are, is in
[`hardening.md`](hardening.md#the-two-suppressed-findings-and-why).

## Architectures

`linux/amd64` and `linux/arm64`, each built on a native runner — no QEMU. Telegraf
is pinned and checksum-verified for both; see
[ADR-0011](adr/0011-telegraf-pinning.md) for the values.

Trivy scans run on `ubuntu-latest` for both, since it reads the tarball's layers
and the host architecture is irrelevant. Integration tests need matching hardware.

## Releasing

The version comes from `CHANGELOG.md`. **Never hand-push a `v*` tag** — the
pipeline creates it after every gate has passed.

1. On `dev`, rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD`.
2. Open a PR `dev → main`. The version gate blocks the merge unless the version is
   valid SemVer and greater than the last tag.
3. On merge: the image is published, the tag is created, and the release notes
   are drawn from the changelog entry.

A tag pointing at a commit that is not on `main` is refused: `refs/tags/v*` is not
covered by branch protection, and the version gate is a no-op on a tag push, so
without that check a hand-pushed tag on any commit would publish an image.

## Repository settings — maintainer, by hand

Deliberately not automated. Changing repository settings, secrets or rulesets is
outside what an agent does on this project (`AGENTS.md` §3), so this is the
checklist.

**Branch protection** on `main` and `dev`:

- require a pull request before merging;
- require these status checks — the names are the jobs' display names, matrix
  substitution included:
  - `Format & Lint`
  - `Tests (stable)` — **not** `Tests (beta)`, which is a non-blocking canary
  - `Supply-Chain Security`
  - `Code Coverage (≥ 80%)`
  - `Telegraf reference & docs`
  - `Version gate`
  - `Semgrep SAST`
- require branches to be up to date before merging;
- disallow force pushes and deletion.

The image jobs (`build`, `scan`, `integration`, `updates`) are deliberately not
required checks: they take tens of minutes, and requiring them would make every
documentation typo wait for two container builds. They still run on every PR and
still gate `publish`, so nothing unscanned ships either way.

**Enable "Allow auto-merge"** (Settings → General → Pull Requests). Both
`dependabot-auto-merge.yml` and the release housekeeping PR queue their merges
with `gh pr merge --auto`, which does nothing without it.

**Enable "Allow GitHub Actions to create and approve pull requests"** (Settings
→ Actions → General → Workflow permissions). Without it the API refuses with
*"GitHub Actions is not permitted to create or approve pull requests"*, and two
things stop working: `auto-pr.yml` cannot open its draft PR, and the
post-release housekeeping PR cannot be opened. Both now warn instead of failing
— the branch is pushed either way, so nothing is lost and you open the PR by
hand. The setting is off by default on new repositories.

**Repository variables**

| Name | Value | Needed for |
|---|---|---|
| `DOCKERHUB_USERNAME` | the Docker Hub account owning `<user>/muninn` | `push`, `publish`, the ghcr mirror |

**Secrets**

| Name | Needed for | Consequence if absent |
|---|---|---|
| `DOCKERHUB_TOKEN` | pushing the image | `push` fails with a message naming it; nothing is published |
| `RELEASE_PAT` | the post-release housekeeping PR | the PR is still opened with `GITHUB_TOKEN`, but CI does not trigger on it and auto-merge hangs — merge it by hand |

`GITHUB_TOKEN` is built in and needs no setup. It carries the ghcr mirror
(`packages: write`), the git tag (`contents: write`) and the SARIF uploads
(`security-events: write`).

A Docker Hub **access token**, not the account password: scope it to
Read/Write/Delete on this repository. Delete is required — `publish` removes the
two `staging-*` tags once the multi-arch manifest points at their digests.

**Why Docker Hub is primary and ghcr is the mirror.** The image is pushed to
Docker Hub by digest from the scanned tarball, and `skopeo copy --all` then
copies the manifest list and every blob to ghcr — so both registries carry
byte-identical images with the same digests, from one build, without a second
push path to keep correct. This settles [O2](risks.md), which had left the
question open in favour of a ghcr-only setup needing no secrets.

**Permissions.** Least privilege per job. The default is `contents: read`;
`publish` needs `contents: write` for the tag and `packages: write` for the
registry; `scan` needs `security-events: write` for SARIF upload.

## Dependencies

**Dependabot** for Cargo crates, GitHub Actions and the Dockerfile's base
images, weekly, one grouped PR per ecosystem. Actions are pinned by commit SHA,
and Dependabot updates the SHA while keeping the version comment.

`dependabot-auto-merge.yml` queues patch and minor bumps for auto-merge once the
required checks pass. A major stays open for review — a green suite does not
prove a major has no breaking runtime behaviour the tests miss. A 3-day cooldown
applies to version updates and not to security updates, so a CVE fix is never
delayed by it.

**Telegraf is not covered.** Dependabot does not track `dl.influxdata.com` URLs,
so bumping it is a manual change to a version string and two checksums in the
Dockerfile, plus the same version in `ci.yml` — deliberately visible in a diff
rather than automatic, and the `reference` job fails if the two disagree.

Two other things it does not update, both deliberate: `container:` references,
so the pinned Semgrep image needs a manual bump (acceptable — the rules are
fetched at scan time, so a pinned image still scans with current rules), and the
InfluxDB and Prometheus images in `docker-compose.integration.yml`, which are
test scaffolding whose silent bump would change what the stack test means.

## Local equivalents

Everything CI runs, in the order it runs it:

```bash
cargo fmt --all -- --check
cargo lint                                    # clippy -D warnings
cargo t-all                                   # test --workspace --locked
cargo audit-all                               # cargo deny check
cargo cov-ci                                  # llvm-cov --fail-under-lines 80
bash scripts/verify-design-package.sh         # the `reference` job, plus the cargo gates

docker build -t muninn:dev .
bash scripts/integration-test.sh muninn:dev
bash scripts/container-test.sh muninn:dev
bash scripts/updates-test.sh muninn:dev
```

`scripts/test-linux.sh` is the one with no CI counterpart, because CI already
runs on Linux. On the maintainer's Windows machine it is what keeps the
`#[cfg(unix)]` tests from being silently absent.

## Related

- [`CONTRIBUTING.md`](CONTRIBUTING.md) — the gates and when to run them
- [`versioning.md`](versioning.md) — what a version number promises
- [`hardening.md`](hardening.md) — what the scans enforce
