# ADR-0011 — Pin Telegraf by tarball and verified checksum

**Status:** accepted · **Date:** 2026-08-02

## Context

The image ships a specific Telegraf build. muninn generates configuration against
a specific plugin surface — option names, defaults and semantics all move between
minor releases (`inputs.system.include` and
`outputs.prometheus_client.name_sanitization` are both recent additions) — so
"whatever Telegraf is current" is not an acceptable input to the build.

Two ways to pin: copy the binary out of the official image at a digest, or
download the release tarball and verify its checksum.

## Decision

Download the release tarball from `dl.influxdata.com` at an explicit version and
verify it against the checksum recorded here, per architecture.

**Telegraf 1.39.2** (released 2026-07-20):

| Architecture | SHA-256 |
|---|---|
| `linux_amd64` | `3ecf733bec389b8a0e1072f134ce379d79efe0d3caf984c164bd4cfc515a86d6` |
| `linux_arm64` | `7626df978e86b4788aed477f7acb4528ff517b506c721f1bd4c9ac77464a93e5` |

Upstream publishes these as `<tarball>.DIGESTS`, in `sha256sum -c` format. The
build fails on a mismatch — it does not warn and continue.

muninn additionally compares the runtime binary's reported version against the
version it was built for, and refuses to start on a mismatch
(`TELEGRAF_START`, 21).

## Consequences

- Bumping Telegraf is a deliberate change to two checksums and a version string,
  visible in the diff and reviewable. It cannot happen by rebuilding.
- The two checksums have to be updated together, and a mismatch on one
  architecture fails only that leg of the build — which is the correct outcome,
  but worth knowing when a release looks half-broken.
- Verification is explicit and auditable in the Dockerfile: anyone reading it can
  see what is being fetched and what it must hash to. There is no trust in a tag
  or a registry account.
- A single verified artefact per architecture means the SBOM and the licence
  documentation have one unambiguous Telegraf version to name.
- The runtime image contains only the Telegraf *binary*, not the upstream image's
  entrypoint scripts, default configuration or user setup — none of which muninn
  wants, since it manages configuration and lifecycle itself.
- Updating requires watching upstream releases. Dependabot does not track
  `dl.influxdata.com` URLs, so this is a manual or scripted check, noted in
  `docs/ci-cd.md`.

## Alternatives considered

**`COPY --from=telegraf:1.39.2@sha256:…`** — pull the official image at a digest
and copy the binary out. One value covers both architectures, and Renovate can
update it automatically. Genuinely attractive.

Rejected because the digest pins the *image*, and what actually needs pinning is
the binary. The image contains an entrypoint, a default config and a user setup
that muninn discards, so the digest ties the build to bytes it does not use and
would churn whenever any of them changes. The tarball checksum names exactly the
artefact that ships. It also keeps the build independent of Docker Hub
availability and rate limits, which matters in CI.

**Build Telegraf from source at a pinned commit.** Rejected: it adds a Go
toolchain and a long compile to every image build, for a reproducibility gain the
checksum already provides.

**Use the distribution's Telegraf package.** Rejected: version depends on the
base image's release cycle, which is precisely the moving target this ADR exists
to eliminate.
