# Releasing

The runbook. Why the pipeline is shaped this way is [`ci-cd.md`](ci-cd.md); what
each workflow does is [`workflows.md`](workflows.md).

**You pick a version number. That is the whole manual part.** Everything
downstream — the image, the tag, the GitHub Release, the SBOM and reopening the
changelog — is automated. You never edit `main` or a tag by hand, and no bot ever
pushes to `main` or `dev`.

**Never edit the version in `Cargo.toml`.** The post-release housekeeping PR sets
it, together with `Cargo.lock`, through `scripts/set-workspace-version.sh`. Every
CI job runs `--locked`, and a manifest whose version the lock file does not know
fails with *"cannot update the lock file"* before a single test runs. That is how
the housekeeping PR arrived red the first time it ran.

## One-click release (recommended)

Actions → **Release (dispatch)** → *Run workflow* → pick `patch`, `minor` or
`major`.

It computes the next version from the higher of the last `v*` tag and the topmost
released changelog version, stamps `## [Unreleased]` as `## [X.Y.Z] - <today>`,
fixes the compare links, bumps the workspace version, and opens an auto-merging
PR into `main`. Owner-only — the first step refuses anyone else.

It refuses to run when:

- `## [Unreleased]` is **empty**. A version documenting nothing is worse than no
  release, because the changelog is what tells an operator whether to upgrade.
- the computed tag **already exists**.

Two things about the button look like bugs and are not:

- **It is only listed once the workflow file is on the default branch.** That is
  how `workflow_dispatch` works; a release workflow living only on `dev` has no
  button anywhere.
- **Without `RELEASE_PAT` the PR does not trigger CI**, so its required checks
  never run and auto-merge waits forever. The workflow says so in a warning
  annotation — merge that PR by hand, or add the secret.

## Manual release (`dev → main`)

The same three steps by hand, and what you fall back to if the dispatch cannot
open its PR.

### 1. Pick the version and update the changelog, on `dev`

Rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD`; the accumulated entries
stay under it. Pick `X.Y.Z` per SemVer — [`versioning.md`](versioning.md) says
what the number promises.

### 2. Open the release PR `dev → main`

The **version gate** validates `X.Y.Z` before the merge is allowed: valid SemVer,
and strictly greater than the latest `v*` tag. A typo or a non-increasing version
fails the PR here rather than after the image is built.

### 3. Merge

A **merge commit**, not a squash — `dev → main` is the one place this repository
keeps history. That needs "Allow merge commits" enabled;
[`ci-cd.md`](ci-cd.md#repository-settings--maintainer-by-hand) has the full
settings checklist.

## What happens after the merge

```text
merge dev → main
      │
      ▼
ci.yml (main push)                          → Docker Hub  :latest  :0.2.0
      ├─ builds nothing new — tags the already-scanned digests
      ├─ mirrors the exact image to ghcr.io → ghcr.io/.../muninn.io :latest :0.2.0
      └─ creates the git tag (RELEASE_PAT)  → v0.2.0
                                                 │
                                                 ▼ (tag push)
release.yml
      ├─ re-runs the full suite with coverage at the tag
      ├─ GitHub Release v0.2.0
      │    • notes: CHANGELOG section + pull commands + manifest digest
      │      + the pinned Telegraf version + test summary
      │    • attaches test-report.md and muninn-0.2.0.cdx.json (SBOM)
      └─ opens a PR into dev that:
           • reopens a fresh ## [Unreleased]
           • fixes the compare links
           • bumps the workspace version (Cargo.toml AND Cargo.lock)
```

- The published image is **byte-identical** to the one that was scanned and
  system-tested. It is never rebuilt for publishing.
- `0.x` and any pre-release (`-rc.1`, `-beta`, …) are flagged **pre-release** on
  GitHub.
- The housekeeping PR **auto-merges** once its checks pass.
- A registry hiccup while resolving the manifest digest only omits that line from
  the notes; it never blocks the release. The same is true of the test report —
  the notes stay silent rather than claim a verdict they cannot show.

## Verify a release

```bash
# Image on both registries, multi-arch (two entries), same digests:
docker buildx imagetools inspect docker.io/jschnabel/muninn:0.2.0
docker buildx imagetools inspect ghcr.io/joshua-schnabel/muninn.io:0.2.0

# Tag and GitHub Release exist:
git ls-remote --tags origin v0.2.0
gh release view v0.2.0

# The test report and the SBOM are attached:
gh release view v0.2.0 --json assets --jq '.assets[].name'

# dev was reopened for the next cycle:
gh pr list --base dev --search "prepare next cycle"

# The staging tags were cleaned up (needs Delete scope on DOCKERHUB_TOKEN):
docker buildx imagetools inspect docker.io/jschnabel/muninn:staging-linux-amd64
```

## When a release goes half-way

`release.yml` has a `workflow_dispatch` entry point taking an existing tag. It
does the same work the tag push would have done and creates nothing twice:
`gh release view` makes creation idempotent, uploads use `--clobber`, and every
step reads the tag rather than the branch the button was pressed on.

Use it when the tag was pushed by `GITHUB_TOKEN` — which starts no workflow, and
is exactly what happened to v0.1.0: image, mirror and tag all shipped, and the
Release, the SBOM, the test report and the housekeeping PR did not, with every
job in the run green. Also use it when a run failed part-way through.

Do **not** cut a new version to fix a broken release run. The tag is fine; only
the work that happens after it is missing.

## Rules and gotchas

- **Never hand-push a `v*` tag.** Tags are created only by the pipeline, after
  every gate has passed. `refs/tags/v*` is not covered by branch protection and
  the version gate is a no-op on a tag push, so `release.yml` and `publish` both
  refuse a tag that does not point at a commit on `main` — that check is the only
  thing standing between a hand-pushed tag and a Release around every gate.
- **The version lives in `CHANGELOG.md` and nowhere else you touch.**
- **First release ever:** with no existing tag, the gate only checks that the
  version is valid SemVer — there is nothing to be greater than.
- **A `main` push without a version bump is safe.** The tag already exists, so
  the tag and release steps skip. Nothing breaks and nothing new is released.
- **Re-running a release run is safe** for the same reason.

## Related

- [`ci-cd.md`](ci-cd.md) — the pipeline and the secrets a release depends on
- [`workflows.md`](workflows.md) — `release.yml` and `release-dispatch.yml` job by job
- [`versioning.md`](versioning.md) — what the number you pick promises
