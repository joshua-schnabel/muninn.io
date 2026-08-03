# Roadmap

What is left, and what is deliberately not being done. What already shipped is in
[`CHANGELOG.md`](../CHANGELOG.md); why it was built that way is in
[`adr/`](adr/).

## Where things stand

muninn is feature-complete for its first release. Every module works, the
container image builds for `linux/amd64` and `linux/arm64` and passes its tests
under the full hardening, the whole path from YAML to a running Telegraf is
exercised end to end, and the pipeline builds, scans, tests and publishes it.

**No version has been released yet.** Cutting `0.1.0` means naming it in
`CHANGELOG.md` and opening a `dev → main` pull request; the version gate reads
the changelog, and `publish` tags the release from it. Publishing additionally
needs the two repository settings in
[`ci-cd.md`](ci-cd.md#repository-settings--maintainer-by-hand), which a
maintainer has to create by hand.

## Next

**Ubuntu security counts are a lower bound.** `muninn_updates_pending{severity="security"}`
classifies from the origin apt prints for the candidate version, which on Ubuntu
is often `-updates` even for a security fix. The total is unaffected. Ubuntu's
own `apt-check` asks whether the candidate *version* exists in any security
origin instead, which `apt-cache policy` exposes — a second apt invocation and a
second parser. It changes numbers that were measured, so it needs an amendment to
[ADR-0009](adr/0009-updates-module-approach.md) and its own ground truth, not a
quiet change. [R8](risks.md), and documented at the metric in
[`modules.md`](modules.md#updates).

**Two suppressed image findings expire 2026-11-03.** `.trivyignore.yaml` holds
`CVE-2026-49980` and `GHSA-hrxh-6v49-42gf`, both in Go modules vendored into the
Telegraf binary and both unreachable from any configuration muninn can generate.
Telegraf 1.39.2 is the newest release and carries neither fix. When the dates
pass, the image scan blocks again — which is the point. Re-check upstream before
then; the reasoning is in [`hardening.md`](hardening.md).

**A bounded restart, if operational experience asks for it.**
[ADR-0002](adr/0002-supervisor-no-restart-loop.md) leaves room for an optional
restart — off by default, at most three attempts, exponential backoff. Whether it
earns its complexity should be decided from watching muninn run, not in advance.
[O3](risks.md).

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
