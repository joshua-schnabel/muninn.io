# CI/CD and repository setup

> **Status: planned (WP12).** No workflows exist yet. This page is the
> specification they are built to, and the checklist of settings a maintainer has
> to apply by hand. Until WP12 lands, run the gates locally — see
> [`CONTRIBUTING.md`](CONTRIBUTING.md).

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
       └ version-gate ────────────────┤
                                      ▼
            build (per arch, native) → image.tar artefact
               ├ scan        (Trivy on the artefact)
               └ integration (compose on the artefact)
                                      ▼
            push (per arch, push events only) → digest artefact
                                      ▼
            publish → multi-arch manifest, tag
```

`publish` needs both `scan` and `integration`, so nothing unscanned can ship.

## Jobs

| Job | Runs | Blocks on |
|---|---|---|
| `check` | `cargo fmt --check`, `cargo clippy -D warnings` | any warning |
| `test` | `cargo test --workspace --locked` on stable and beta | stable only — beta is a non-blocking canary for the next compiler |
| `supply-chain` | `cargo deny check` | advisories, disallowed licences, banned crates, unknown registries |
| `coverage` | `cargo llvm-cov --fail-under-lines 80` | under 80 % workspace lines |
| `version-gate` | CHANGELOG version is valid SemVer and greater than the last tag | invalid or non-successor version |
| `build` | Docker build per architecture into `image.tar` | build failure, Telegraf checksum mismatch |
| `scan` | Trivy on the artefact | fixable CRITICAL/HIGH |
| `integration` | Load the image, run the compose stack, assert | any assertion |
| `push` / `publish` | Push by digest, assemble the manifest, create the tag | — |

`version-gate` **always runs** and decides internally whether to enforce. A
skipped job in `needs:` skips its dependents, so it must not be conditional at
the job level — it is a deliberate no-op pass on non-release events.

Two additions over huginn.io, both from WP0 findings:

**Reference config check.** `docs/reference/telegraf.reference.conf` must be
accepted by the pinned Telegraf:

```bash
docker run --rm -v "$PWD/docs/reference:/ref:ro" telegraf:$TELEGRAF_VERSION \
  telegraf config check --strict-env-handling --config /ref/telegraf.reference.conf
```

**Plugin option cross-check.** Every `plugin.option` named in `docs/modules.md`
must exist in the pinned Telegraf's `sample.conf`. This is what catches
documentation drifting away from the version actually shipped, which is
[R5](risks.md).

## Security workflow

Separate from `ci.yml`, on every push and PR, because it needs no build and
should give feedback before a PR exists.

**Semgrep** with `p/rust` and `p/secrets`, pinned by image digest. Two passes: a
full scan uploading SARIF to the Security tab (never blocks), and a blocking scan
where ERROR-severity findings fail the run.

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
- require status checks: `check`, `test (stable)`, `supply-chain`, `coverage`,
  `version-gate`;
- require branches to be up to date before merging;
- disallow force pushes and deletion.

**Repository variables**

| Name | Purpose |
|---|---|
| `TELEGRAF_VERSION` | Optional; the Dockerfile pins it anyway. Useful for the reference-check job |

**Secrets** — none are needed for the default setup. Publishing to
`ghcr.io/joshua-schnabel/muninn.io` uses the built-in `GITHUB_TOKEN` with
`packages: write`.

A Docker Hub mirror, if wanted later, needs `DOCKERHUB_TOKEN` as a repository
secret and `DOCKERHUB_USERNAME` as a variable, plus a mirror step in `publish`.
That is [O2](risks.md) and is open.

**Permissions.** Least privilege per job. The default is `contents: read`;
`publish` needs `contents: write` for the tag and `packages: write` for the
registry; `scan` needs `security-events: write` for SARIF upload.

## Dependencies

**Dependabot** for Cargo dependencies and GitHub Actions, weekly. Actions are
pinned by commit SHA, and Dependabot updates the SHA while keeping the comment.

**Telegraf is not covered.** Dependabot does not track `dl.influxdata.com` URLs,
so bumping it is a manual change to a version string and two checksums —
deliberately visible in a diff rather than automatic.

Note also that Dependabot does not update `container:` references, so the pinned
Semgrep image needs a manual bump. That is acceptable: the rules are fetched at
scan time from the registry, so a pinned image still scans with current rules.

## Local equivalents

```bash
cargo fmt --all -- --check
cargo lint
cargo t-all
cargo audit-all
cargo cov-ci
```

## Related

- [`CONTRIBUTING.md`](CONTRIBUTING.md) — the gates and when to run them
- [`versioning.md`](versioning.md) — what a version number promises
- [`hardening.md`](hardening.md) — what the scans enforce
