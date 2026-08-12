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
── Source ─────────────────────────────────────────────────────────────────
check ─┬ test (stable + beta canary) ─┐
       ├ msrv ────────────────────────┤
       ├ supply-chain ────────────────┤
       ├ coverage ────────────────────┤
       ├ reference ───────────────────┤
       └ version-gate ────────────────┤
                                      ▼
                                 SOURCE GATE
── Image ─────────────────────────────┼────────────────────────────────────
                                      ▼
            build (per arch, native) → image.tar artefact
               ├ scan        (Trivy on the artefact)
               ├ integration (compose stack + hardened image)
               └ updates     (the module against real host trees)
                                      ▼
                                  IMAGE GATE
── Release (push events only) ────────┼────────────────────────────────────
                                      ▼
            push (per arch) → digest artefact
                                      ▼
            publish → multi-arch manifest, ghcr mirror, tag
                                      ▼
            release.yml → GitHub Release + housekeeping PR into dev
                          (needs RELEASE_PAT on the tag push, or it never starts)
```

**The gates are the stage boundaries**, not a verdict standing beside them:
`build` depends on `source-gate` and `push` on `image-gate`, so each stage's
membership is written down exactly once, in that gate's `needs`. `build` used to
re-list the source jobs, which made the gate's list a second copy of the same
set — and that copy had already drifted: `msrv` was in neither, so a job that
ran on every pull request for a whole release gated nothing at all.

`push` reaches a registry only behind `image-gate`, which covers `scan`,
`integration` **and** `updates`, so nothing unscanned or untested can ship.

**`coverage` hangs off `check`, not off `test`**, so it runs alongside the two
toolchain legs. `cargo llvm-cov --workspace` runs the whole suite itself,
instrumented, so waiting for `test` bought no extra confidence — only the
duration of a full workspace suite on the critical path, part of it spent
waiting on the beta leg, which `continue-on-error` forbids from blocking
anything. The cost is a coverage run that is also spent on a red push; both jobs
are still in `source-gate`, so neither stops blocking.

Job display names carry a `Source ·` / `Image ·` / `Release ·` / `Security ·`
prefix. GitHub has no stages of its own — the run graph is drawn from `needs`
alone — so the prefixes are what makes the job list group by stage, and each
gate writes its members and their results to the run summary. The three **gate**
names deliberately carry no prefix; see "Repository settings" below.

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
| `source-gate` / `image-gate` | Nothing. They read `toJSON(needs)` and fail if any member is not `success` | any member of their stage |
| `push` / `publish` | Push by digest, assemble the manifest, mirror to ghcr, create the tag | — |

`version-gate` **always runs** and decides internally whether to enforce. It
must not be conditional at the job level: `source-gate` counts anything that is
not `success` as a failure, so a *skipped* `version-gate` would fail the gate
and take `build` with it. It is a deliberate no-op pass on non-release events.

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
`release.yml`'s `prepare-dev`, which pushes and runs no third-party code.
`release.yml` is split into `test-report` (`contents: read`, runs `cargo`,
uploads an artefact) and `github-release` (`contents: write`, downloads it) for
exactly this reason — if you add a `cargo` step, it belongs in the first job.

**`ci.yml`'s `publish` used to be the second exception**, on the argument that
it runs no cargo. It runs `download-artifact`, two `docker/login-action` steps
and skopeo, which is third-party code with the token in `.git/config` beside it
— so the argument was about the wrong thing, and the exception is gone (F-13).
The credential now reaches only the step that pushes the tag, through git's
`extraheader` rather than a remote URL, and a `trap` removes it on the way out
including when the push fails. A URL is argv, and argv is world-readable on the
runner and survives in `git remote -v` for whatever runs next.

**Credentials go through stdin or a file, never argv.** `/proc/<pid>/cmdline` is
readable by every process on the runner. `skopeo login --password-stdin` with
`REGISTRY_AUTH_FILE` replaces `--dest-creds`, and `curl --data @-` / `-K -`
replaces `-d` and `-H` where a token is involved.

`scan` blocks on **fixable** CRITICAL/HIGH only. The runtime base is debian-slim
rather than distroless because the updates module needs real apt and dpkg; the
cost of that trade is measured in
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

Every entry is a Go module vendored into the Telegraf binary rather than one of
muninn's own dependencies. The reasoning, and the table of what they are, is in
[`hardening.md`](hardening.md#the-suppressed-findings-and-why) — the file itself
is the authority, because a count repeated here is wrong the next time one is
added, and this one was.

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

**The tag is the pipeline's output, not a second way into it.** `ci.yml` runs on
pushes to `dev` and `main` and on pull requests — not on tags. It used to run on
tags too, and `publish` creates the tag as its last step, so a release built and
published the image twice: once from the `main` push, once from the tag that push
created. Two builds, two manifests, and one moving `:x.y.z` tag that ended up
pointing at whichever finished last — while `release.yml`, fired by the same tag,
assembled the SBOM and the notes from whatever was there at that moment. Every
gate stayed green because nothing compared the two. v0.3.0 shipped that way.

A hand-pushed tag therefore publishes nothing now. It still reaches `release.yml`,
which refuses it twice over: the commit must be on `main`, and the tag must carry
the `muninn-manifest-digest:` annotation that `publish` writes.

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

One consequence is worth knowing before you dispatch an **older** tag: the suite
and `scripts/test-report.sh` both come from that tag's commit, not from `dev`. A
reporting bug fixed since then is still present there. That is why the report is
best-effort — the tests decide whether a release happens, the report only
describes them, and a Release whose tests passed is not withheld because a
formatter failed. When there is no report, the run carries a warning and the
notes omit the test section instead of asserting a verdict they cannot show.

## Repository settings — maintainer, by hand

Deliberately not automated. Changing repository settings, secrets or rulesets is
outside what an agent does on this project (`AGENTS.md` §3), so this is the
checklist.

**Branch protection** on `main` and `dev`:

- require a pull request before merging;
- require exactly **three** status checks, each a fan-in job that runs no build
  and no test of its own:
  - `Source gate` — `check`, `test`, `msrv`, `supply-chain`, `coverage`,
    `reference`, `version-gate`
  - `Image gate` — `build`, `scan`, `integration`, `updates`
  - `Security gate` — `shellcheck`, `actionlint`, `semgrep`
- require branches to be up to date before merging;
- disallow force pushes and deletion.

**Why three names instead of eleven.** A check that is not in the required set
is an *indicator*: it runs, it goes visibly red, and it stops nobody. Keeping
that set in step with `ci.yml` by hand is the failure this replaces, and it had
already happened here — `MSRV` ran on every pull request for a whole release
without blocking one, and so did `Version gate`, `Telegraf reference & docs` and
both `Updates module` legs. `Version gate` mattered most: the one-click release
opens an **auto-merging** PR, and auto-merge waits only for required checks, so
the check written to fail a bad release version was being bypassed by the very
path it exists for.

Each gate derives its verdict from its own `needs` — the same list the pipeline
must maintain anyway to order itself. A job added to `needs` is covered the
moment it is added, rather than the moment somebody remembers to edit a
repository setting that is invisible from the code.

Four consequences worth knowing before anyone changes this:

- **The three gate names are a fixed surface.** `build` and `push` now depend on
  their gate, and the ruleset names it, so the string appears in two places that
  cannot see each other. Rename a required check and it never reports again —
  and a check that never reports blocks every pull request indefinitely. This is
  why the gates alone carry no `Source ·` / `Image ·` / `Security ·` prefix.
- **`if: always()` on each gate is load-bearing.** Without it, a gate whose
  dependency failed is *skipped* rather than failed — and a skipped required
  check counts as satisfied. The gate would be green by absence in exactly the
  case it exists for. It is also what makes the gate safe to depend on: a job
  that can never be skipped always resolves to a real verdict, so `build` and
  `push` are skipped precisely when their stage did not pass.
- **`Tests (beta)` still does not block.** Its leg carries `continue-on-error`,
  so it reports `success` to `needs` even when it fails. It stays a canary.
- **`push` and `publish` are deliberately in no gate.** Both are `push`-only, so
  on a pull request they report `skipped`, and the gates treat anything that is
  not `success` as a failure.

> **Changing the ruleset is a manual step, and its order matters.** The gate
> jobs must exist and have reported once before they are made required — a
> required check that never reports blocks every pull request indefinitely. The
> old names must come out in the same edit, because the pull request that
> renames them is itself blocked by them.

**The image jobs are required, deliberately.** `Image gate` waits on `build`, so
a documentation-only PR waits for two container builds, and an advisory
published that morning against something in the image blocks a branch that never
touched the image — that happened on 2026-08-06. The decision is that this is
the right way round: a finding that blocks is a finding someone looks at, and
the alternative lets a fixable CRITICAL reach `dev` and be caught one step
later, at `publish`.

**Everything else that runs, blocks.** That is new: `ShellCheck` and
`Actionlint` used to run on every push and PR and go visibly red while a PR
merged straight past them, and they now block through `Security gate`.
`Version gate` blocks through `Source gate` rather than by being listed in
`build`'s `needs` — that transitive route is gone on purpose, because `build`
names only the gate now, so there is one list of the source stage instead of two
that can drift.

**Enable "Allow auto-merge"** (Settings → General → Pull Requests). Both
`dependabot-auto-merge.yml` and the release housekeeping PR queue their merges
with `gh pr merge --auto`, which does nothing without it.

**Enable both "Allow merge commits" and "Allow squash merging"** (same page).
The branch model uses one of each: `feature/* → dev` squashes, `dev → main`
keeps a merge commit, and `release-dispatch.yml` asks for `--merge` explicitly.
0.1.0 and 0.1.1 were squashed into `main` because merge commits were the only
method *not* enabled — which is why `main` does not share `dev`'s history for
those two releases. With merge commits disabled the release PR is still opened,
but auto-merge warns and you merge it by hand.

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

| `GPG_PRIVATE_KEY` | signing the commits `release.yml` and `release-dispatch.yml` create | those jobs fail with a message naming the secret. Without it the commits are unsigned, and both branch rulesets carry `required_signatures` — the pull request opens, every check goes green, and the merge button stays blocked with nothing failing |
| `GPG_PASSPHRASE` | only if the signing key has one | gpg cannot use the key |

**The signing key.** Generate one **for CI**, not a copy of a personal key: if it
leaks, revoking a dedicated key costs nothing, and a personal one costs
everything it ever signed. No passphrase is the normal choice — the private key
is already a secret, and a passphrase beside it in the same secret store adds no
layer.

```bash
gpg --batch --quick-generate-key "<name> <email>" ed25519 sign never
gpg --armor --export-secret-keys <email>   # -> the GPG_PRIVATE_KEY secret
gpg --armor --export <email>               # -> GitHub, Settings -> SSH and GPG keys
```

The email has to be **verified on the GitHub account** that holds the public key,
or the signature is valid and GitHub still labels the commit *Unverified*. The
workflows read the name and email out of the key's UID rather than hard-coding
them, so whichever identity you pick is the one that signs — but that identity
and the account must agree.

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
