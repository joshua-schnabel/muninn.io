# CI/CD and repository setup

> **Status: shipped.** The workflows are in `.github/workflows/`. This
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
release-dispatch.yml (optional entry point) → release PR into main
                                      ▼
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
                          (needs RELEASE_PAT on the tag push, or it never starts)
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
| `updates` | Load the image; `updates-test.sh` against real Debian and Ubuntu trees, then `image-updates-test.sh` against the runner's own daemon | any assertion |
| `push` / `publish` | Push by digest, assemble the manifest, mirror to ghcr, create the tag | — |

`version-gate` **always runs** and decides internally whether to enforce. A
skipped job in `needs:` skips its dependents, so it must not be conditional at
the job level — it is a deliberate no-op pass on non-release events.

`test` and `coverage` fetch Telegraf with `scripts/fetch-telegraf.sh` and set
`MUNINN_TELEGRAF_BIN`. Without it the tests that need a real Telegraf skip
loudly, and CI would report a green suite that never started a child process —
which is the class of bug `muninn/tests/` exists for.

That script exists rather than a `docker create telegraf:x.y.z`, which is what
these jobs used to do. A tag is mutable, and nothing here would notice it move —
Dependabot does not read a `docker create` reference
(dependabot-core#5819, the same limitation `security.yml` records for its
Semgrep image). Rather than add a second pin to keep current by hand, the script
reads the version *and* the per-architecture SHA-256 out of the `Dockerfile` and
verifies the download against them. There is one pin, it is the one
[ADR-0011](adr/0011-telegraf-pinning.md) already describes, and bumping Telegraf
stays the three-line Dockerfile change that ADR specifies.

## What can reach a credential

The pipeline's own hardening is summarised in
[`SECURITY.md`](SECURITY.md#the-build-pipeline-is-part-of-the-surface). Two
shapes are worth knowing when editing a workflow here:

**No job runs `cargo` with a write token.** `actions/checkout` persists its
token into `.git/config`, and `cargo` executes build scripts from every
dependency. Every checkout therefore sets `persist-credentials: false` except
`ci.yml`'s `publish` and `release.yml`'s `prepare-dev`, which push and run no
third-party code. `release.yml` is split into `test-report` (`contents: read`,
runs `cargo`, uploads an artefact) and `github-release` (`contents: write`,
downloads it) for exactly this reason — if you add a `cargo` step, it belongs in
the first job.

**Credentials go through stdin or a file, never argv.** `/proc/<pid>/cmdline` is
readable by every process on the runner. `skopeo login --password-stdin` with
`REGISTRY_AUTH_FILE` replaces `--dest-creds`, and `curl --data @-` / `-K -`
replaces `-d` and `-H` where a token is involved.

`scan` blocks on **fixable** CRITICAL/HIGH only. The runtime base is
`debian:12-slim` rather than distroless because the updates module needs real
apt and dpkg; the cost of that trade is measured in
[`hardening.md`](hardening.md) and is CVEs with no fix available. A gate that
blocks on findings nobody can act on is a gate that gets switched off.

Two jobs are worth explaining, because both cover something no Rust test can see.

**`reference`** guards the anchor. Every snapshot is anchored to
`docs/reference/telegraf.reference.conf`, so if the pinned Telegraf stops
accepting it, the snapshots still pass and the artefact is wrong. The job also
re-checks the ordering fixtures behind
[ADR-0007](adr/0007-tagdrop-and-render-order.md) — both pass `config check`, and
only the emitted metric count tells them apart — cross-checks every
`plugin.option` in [`modules.md`](modules.md) against the pinned `sample.conf`
([R5](risks.md)), and asserts that the workflow's `TELEGRAF_VERSION` matches the
Dockerfile's, so the reference can never be verified against a version the image
does not carry.

**`updates`** is separate because building the Debian and Ubuntu fixture trees is
minutes of apt work that should not sit in front of the stack test. Cell S11 — a
real host through WSL — **skips** on a runner and says so; it is the only cell
that can, and a skip is counted and printed separately from a pass, because a
skip that reads like a pass is worse than no cell at all.

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
pipeline creates it after every gate has passed. There are two ways to start a
release and they converge immediately: both end at a PR into `main`, and
everything after that merge is identical.

**One-click (`release-dispatch.yml`).** Actions → **Release (dispatch)** → *Run
workflow*, pick `patch` / `minor` / `major`. It computes the next version from
the higher of the last `v*` tag and the topmost released changelog version,
stamps `## [Unreleased]` as `## [X.Y.Z] - <today>`, fixes the links, bumps
`Cargo.toml`, and opens an auto-merging PR into `main`. It refuses to run if
`## [Unreleased]` is empty — a version documenting nothing is worse than no
release, because the changelog is what tells an operator whether to upgrade — or
if the computed tag already exists. Owner-only.

Two things about the button itself, both of which look like bugs and are not:

- **It is only listed once the workflow file is on the default branch.** That is
  how `workflow_dispatch` works. A release workflow that lives only on `dev` has
  no button anywhere.
- **Without `RELEASE_PAT` the PR does not trigger CI**, so its required checks
  never run and auto-merge waits forever. The workflow says so in a warning
  annotation; merge that PR by hand, or add the secret.

**By hand.** The same three steps, done manually — which is also what you fall
back to if the dispatch cannot open its PR:

1. On `dev`, rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD`.
2. Open a PR `dev → main`. The version gate blocks the merge unless the version is
   valid SemVer and greater than the last tag.
3. On merge: the image is published, the tag is created, and the release notes
   are drawn from the changelog entry.

A tag pointing at a commit that is not on `main` is refused: `refs/tags/v*` is not
covered by branch protection, and the version gate is a no-op on a tag push, so
without that check a hand-pushed tag on any commit would publish an image.

### Why the tag is pushed with `RELEASE_PAT`

**GitHub does not start a workflow from an event the built-in `GITHUB_TOKEN`
created.** It is a recursion guard, it has no opt-out, and it is the single
sharpest edge in this pipeline: `publish` pushes `vX.Y.Z`, `release.yml` listens
on `push: tags`, and with the built-in token that push fires nothing at all.

v0.1.0 was released that way and shows exactly what it costs. The image, the ghcr
mirror and the tag were all correct — and there was no GitHub Release, no SBOM,
no test report, and no housekeeping PR, with every job in the run green. Nothing
reports this: the run that should have started simply does not exist.

`publish`'s checkout therefore takes `RELEASE_PAT` when it is set, falling back
to `GITHUB_TOKEN`. With the secret the release path completes on its own; without
it, everything up to and including the tag still happens and `release.yml` has to
be started by hand.

### Driving `release.yml` by hand

Actions → **Release** → *Run workflow*, with the existing tag (`v0.1.0`) as the
input. It does the same work the tag push would have: re-runs the suite on the
tagged commit, creates the Release with notes, test report and SBOM, and opens
the housekeeping PR.

It is safe to run more than once. `gh release view` guards creation, the asset
uploads use `--clobber`, and `prepare-dev` exits early when `dev` is already
prepared. Every step reads the *tag*, not the branch the button was pressed on —
which is why the "is this tag on main" check compares the checked-out commit
rather than `github.sha`.

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
| `RELEASE_PAT` | the release tag push, the release-dispatch PR, and the post-release housekeeping PR | **`release.yml` never runs** — the tag is pushed by `GITHUB_TOKEN`, which starts no workflow, so there is no GitHub Release, SBOM or test report until you [drive it by hand](#driving-releaseyml-by-hand). The two PRs are still opened, but CI does not trigger on them and auto-merge hangs |

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

Every gate above runs locally, and the commands are in the
[README](../README.md#development) — with `scripts/verify-design-package.sh`
covering the `reference` job. Run them before pushing: the image jobs take tens
of minutes to tell you what `cargo fmt` would have said in one second.

`scripts/test-linux.sh` is the one script with no CI counterpart, because CI
already runs on Linux. On the maintainer's Windows machine it is what keeps the
`#[cfg(unix)]` tests from being silently absent.

## Related

- [`CONTRIBUTING.md`](CONTRIBUTING.md) — the gates and when to run them
- [`versioning.md`](versioning.md) — what a version number promises
- [`hardening.md`](hardening.md) — what the scans enforce
