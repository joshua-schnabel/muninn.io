# Roadmap

What is left, and what is deliberately not being done. What already shipped is in
[`CHANGELOG.md`](../CHANGELOG.md); why it was built that way is in
[`adr/`](adr/).

## Where things stand

muninn is feature-complete for its first release. Every module works, the
container image builds for `linux/amd64` and `linux/arm64` and passes its tests
under the full hardening, the whole path from YAML to a running Telegraf is
exercised end to end, and the pipeline builds, scans, tests and publishes it.

**No version has been released yet.** Pushes to `dev` already publish the
pre-release tags `0.1.0-dev` and `dev` to `jschnabel/muninn`, mirrored to
`ghcr.io/joshua-schnabel/muninn.io`. Cutting `0.1.0` means naming it in
`CHANGELOG.md` and opening a `dev → main` pull request; the version gate reads
the changelog, and `publish` tags the release from it.

One thing is still wrong there: the `publish` job's staging-tag cleanup gets
`HTTP 403` from Docker Hub, so `staging-linux-amd64` and `staging-linux-arm64`
survive every run. The `DOCKERHUB_TOKEN` needs the **Delete** scope, which
[`ci-cd.md`](ci-cd.md#repository-settings--maintainer-by-hand) already
specifies.

## Next

**Classify Ubuntu security updates by candidate version, not by printed origin.**
The security subset is a lower bound on Ubuntu today. Fixing it costs a second
apt invocation and a second parser, and changes numbers that were measured — so
it needs an amendment to
[ADR-0009](adr/0009-updates-module-approach.md) and its own ground truth.
[R8](risks.md).

**Two suppressed image findings expire 2026-11-03.** `CVE-2026-49980` and
`GHSA-hrxh-6v49-42gf` sit in Go modules vendored into the Telegraf binary, are
unreachable from any configuration muninn can generate, and have no upstream fix
in 1.39.2. When the dates pass the image scan blocks again — which is the point,
so re-check upstream before then. [`hardening.md`](hardening.md).

**A bounded restart, if operational experience asks for it.** Off by default, at
most three attempts, exponential backoff — the room
[ADR-0002](adr/0002-supervisor-no-restart-loop.md) left. Decide from watching
muninn run, not in advance. [O3](risks.md).

**Hosts beyond Debian and Ubuntu.** The updates module is the only Debian-shaped
part; everything else is `gopsutil` reading `/proc` and works anywhere Telegraf
does. Adding a distribution means a second implementation behind the same
interface, not a rewrite. Nothing is planned until someone needs it.

## Not planned

- **Raw Telegraf TOML.** [ADR-0004](adr/0004-no-raw-toml.md) — it is what makes
  validation, determinism and useful error messages possible.
- **Configuration reload.** Change the YAML, restart the container.
- **Windows and macOS hosts.**

## History

muninn was built in thirteen work packages between 2026-07 and 2026-08-03, from
the design package through to the release pipeline. The record is the git
history and [`CHANGELOG.md`](../CHANGELOG.md); the decisions that outlived the
process are in [`adr/`](adr/), and the measurements behind the updates module are
in [`updates-evidence.md`](updates-evidence.md).
