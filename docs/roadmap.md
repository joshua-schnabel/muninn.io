# Roadmap

What is left, and what is deliberately not being done. What already shipped is in
[`CHANGELOG.md`](../CHANGELOG.md); why it was built that way is in
[`adr/`](adr/).

## Where things stand

muninn is feature-complete and released. Every module works, the container image
builds for `linux/amd64` and `linux/arm64` and passes its tests under the full
hardening, the whole path from YAML to a running Telegraf is exercised end to
end, and the pipeline builds, scans, tests and publishes it.

Releases go to `jschnabel/muninn`, mirrored to
`ghcr.io/joshua-schnabel/muninn.io`. Pushes to `dev` publish a pre-release tag
alongside them. `CHANGELOG.md` is the record of what has shipped and the version
gate's authority — the next version is cut by naming it there and opening a
`dev → main` pull request, and `publish` tags the release from it.
[`releasing.md`](releasing.md) is the runbook.

**The staging-tag cleanup may still be failing, and a green run does not tell
you.** The `publish` job's Docker Hub `DELETE` was reported returning `HTTP 403`,
leaving `staging-linux-amd64` and `staging-linux-arm64` behind on every run. The
step is deliberately best-effort and exits zero whatever the status, so the
pipeline stays green either way — which means this is not something CI will
report resolved. Check the tag list on Docker Hub; if they are still there, the
`DOCKERHUB_TOKEN` needs the **Delete** scope, which
[`ci-cd.md`](ci-cd.md#repository-settings--maintainer-by-hand) already
specifies.

## Next

**Validate the Ubuntu security classification against `apt-check`.** The
classification itself is fixed — it asks `apt-cache policy` which origins the
candidate version is available from, rather than reading the one origin apt
prints ([ADR-0009](adr/0009-updates-module-approach.md), [R8](risks.md)). What
is still open is the *ground truth*: it is a second implementation of the same
rule in awk, so if the rule is wrong both are wrong together. Ubuntu's own
`/usr/lib/update-notifier/apt-check` is the independent authority, and it cannot
be installed into a fixture without changing the package state being measured.
Somewhere between "build the fixture, then install it" and "run apt-check in a
sibling container against the exported rootfs".

**Measure `image_updates` against an authenticated registry.** The module is
verified against public images only. A private registry the host can already
pull from should work through the daemon's own stored credentials with no
change to muninn, but nothing in the repository records that it does, or what
an expired credential looks like — all of it lands in
`distribution_query_failed`. Needs a local authenticated registry in
`scripts/image-updates-test.sh` before any reason token is split.
[R9](risks.md), [ADR-0013](adr/0013-image-updates-via-docker-api.md).

**Six suppressed image findings expire 2026-11-03.** One gRPC-Go finding and
five rclone findings, all in Go modules vendored into the Telegraf binary, all
unreachable from any configuration muninn can generate, and none carried by a
Telegraf release. Four of the five rclone entries were added on 2026-08-06, when
Trivy began reporting them against an image nothing had changed. Rechecked
2026-08-09: still no newer Telegraf release, so the dates were deliberately not
extended — an expiry that moves because the answer was "no change" is not an
expiry. When they pass the image scan blocks again, which is the point.
[`hardening.md`](hardening.md), and `.trivyignore.yaml` is the authority.

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

## Related

- [`risks.md`](risks.md) — the open risks these items come from
- [`releasing.md`](releasing.md) — how the next version gets cut
- [`versioning.md`](versioning.md) — what a version number promises
