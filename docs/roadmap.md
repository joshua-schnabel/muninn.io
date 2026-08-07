# Roadmap

What is left, and what is deliberately not being done. What already shipped is in
[`CHANGELOG.md`](../CHANGELOG.md); why it was built that way is in
[`adr/`](adr/).

## Where things stand

muninn is feature-complete and released. Every module works, the container image
builds for `linux/amd64` and `linux/arm64` and passes its tests under the full
hardening, the whole path from YAML to a running Telegraf is exercised end to
end, and the pipeline builds, scans, tests and publishes it.

**`0.1.0` is out** — on `jschnabel/muninn`, mirrored to
`ghcr.io/joshua-schnabel/muninn.io`. Pushes to `dev` keep publishing the
pre-release tags `0.1.0-dev` and `dev` alongside it. The next version is cut the
same way this one was: name it in `CHANGELOG.md` and open a `dev → main` pull
request; the version gate reads the changelog, and `publish` tags the release
from it.

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

**Measure `image_updates` against an authenticated registry.** The module is
verified against public images only. A private registry the host can already
pull from should work through the daemon's own stored credentials with no
change to muninn, but nothing in the repository records that it does, or what
an expired credential looks like — all of it lands in
`distribution_query_failed`. Needs a local authenticated registry in
`scripts/image-updates-test.sh` before any reason token is split.
[R9](risks.md), [ADR-0013](adr/0013-image-updates-via-docker-api.md).

**Six suppressed image findings expire 2026-11-03.** One gRPC-Go finding and five rclone
findings, all in Go modules vendored into the Telegraf binary, all unreachable
from any configuration muninn can generate, and none carried by a Telegraf
release — 1.39.2 is the newest and still vendors the affected rclone. Four of
the five rclone entries were added on 2026-08-06, when Trivy began reporting
them against an image nothing had changed. When the dates pass the image scan blocks again — which is the point,
so re-check upstream before then. [`hardening.md`](hardening.md).

**A bounded restart, if operational experience asks for it.** Off by default, at
most three attempts, exponential backoff — the room
[ADR-0002](adr/0002-supervisor-no-restart-loop.md) left. Decide from watching
muninn run, not in advance. [O3](risks.md).

**Hosts beyond Debian and Ubuntu.** The updates module is the only Debian-shaped
part; everything else is `gopsutil` reading `/proc` and works anywhere Telegraf
does. Adding a distribution means a second implementation behind the same
interface, not a rewrite. Nothing is planned until someone needs it.

**Reconcile `main`'s history with `dev`, at the next release.** They share no
ancestry beyond the first commit, because every release so far was squash-merged
into `main` — the branch ruleset allowed nothing else until 2026-08-07. The
consequence is a repository-wide add/add conflict on every release PR, which
v0.2.0 hit and had to be repaired by hand. The one-time step is in
[`releasing.md`](releasing.md); after it lands as a merge commit, it never
recurs.

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

## Related

- [`risks.md`](risks.md) — the open risks these items come from
- [`releasing.md`](releasing.md) — how the next version gets cut
- [`versioning.md`](versioning.md) — what a version number promises
