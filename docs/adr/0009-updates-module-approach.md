# ADR-0009 — Read host package state via read-only mounts and a simulated upgrade

**Status:** accepted · **Date:** 2026-08-02 · **Decided by:** the
[measured evidence](../updates-evidence.md)

## Context

The updates module reports pending package updates on the host. Two facts
constrained it.

**Telegraf has no package input plugin.** All 249 input plugins of version 1.39.2
were checked. Whatever muninn does, the result reaches Telegraf through
`inputs.exec` with `data_format = "influx"`.

**`apt` inside the container reads the container's package database.** Running
`apt-get -s upgrade` in the muninn container reports the updates pending for
debian-slim — not an error, a number, and a believable one. For a monitoring
system that is the worst failure mode there is, so this decision was deferred
until a spike produced evidence.

## Decision

**Approach A.** Mount the host filesystem read-only, point apt's directory
options at the host's dpkg status, sources and package indices, and run
`apt-get -s dist-upgrade` against them:

```sh
apt-get -s dist-upgrade \
  -o Dir::State::status="$HOSTFS/var/lib/dpkg/status" \
  -o Dir::Etc::sourcelist="$HOSTFS/etc/apt/sources.list" \
  -o Dir::Etc::sourceparts="$HOSTFS/etc/apt/sources.list.d" \
  -o Dir::State::lists="$HOSTFS/var/lib/apt/lists" \
  -o Dir::Cache="$SCRATCH" \
  -o Debug::NoLocking=1
```

Real apt does the resolution. muninn counts `Inst` lines and classifies security
updates by the candidate version's origin suite.

The runtime image is therefore **debian-slim**, not distroless.

### Amendment, 2026-08-09 — classify by *every* origin, not the printed one

The rule above said "the candidate version's origin suite", and the
implementation read the origin apt prints on the `Inst` line:

```text
Inst libc6 [2.36-9+deb12u3] (2.36-9+deb12u7 Debian-Security:12/stable-security [amd64])
```

That names **one** origin — whichever pocket apt resolved the candidate
through. On Debian it is accurate. On Ubuntu it is a lower bound, because
Ubuntu publishes a security update to `<release>-security` *and* copies it into
`<release>-updates`. When apt resolves through the latter, the line reads
`Ubuntu:24.04/noble-updates` and a genuine security update is not counted.

**Measured, not argued.** The Ubuntu 24.04 fixture reported 66 pending / 34
security when it was first built, and 66 pending / **0** security when rebuilt
against a later archive. The packages were identical; only the pocket holding
the candidate had moved. A security count of zero on an Ubuntu host was
therefore not evidence that nothing security-relevant was pending — which is
the one thing that metric has to be, and is the failure mode
[AGENTS.md §9](../../AGENTS.md) singles out as the project's sharpest rule.

So the classification now asks a different question: **is this exact version
available from any security origin**, rather than which pocket apt happened to
name. `apt-cache policy` prints every origin a version is available from, and a
second invocation with the same `Dir::` options answers it. Ubuntu's own
`apt-check` classifies the same way.

The costs are the ones [R8](../risks.md) predicted and accepted:

- **A second apt invocation and a second parser.** It runs once per check, over
  the pending packages only, and is skipped entirely when
  `modules.updates.security_only_metric` is false — there is then no security
  series to publish.
- **Numbers that were measured have changed.** The Ubuntu cells of the evidence
  below are superseded; see [`updates-evidence.md`](../updates-evidence.md) for
  the dated record of both.
- **A failure in the second pass fails the whole check.** Reporting a correct
  total beside a security count that might be wrong is worse than reporting
  neither, and a missing series reads as zero on most dashboards — the same
  shape as the problem being fixed.

The fixture builders record **both** counts, `security` and
`security_printed_origin`, because the two differing is the finding and a
ground truth that recorded only the new number could not show it. That ground
truth is a second implementation of the same rule in awk, not an independent
authority: it catches a mistake in muninn's parser or its apt invocation, and
would not catch the rule itself being wrong. Validating against Ubuntu's
`apt-check` — which cannot be installed into a fixture without changing the
package state being measured — remains open.

Closed as finding F-04 of the 1.0 release review.

## Evidence

Twelve matrix cells, all passing. The counts are the host's own answer,
reproduced exactly:

| Host | Ground truth | Probe |
|---|---|---|
| debian:12, up to date | 0 / 0 | 0 / 0 |
| debian:12, outdated | 41 / 3 | **41 / 3** |
| debian:13, outdated | 39 / 2 | **39 / 2** |
| ubuntu:22.04, outdated | 50 / 40 | **50 / 40** |
| ubuntu:24.04, outdated | 66 / 34 | **66 / 34** |

A `debian:12-slim` container reading an Ubuntu 24.04 host produces that host's
exact answer — container and host being different distributions is the normal
case, and it works.

Under muninn's full hardening (non-root, `--cap-drop=ALL`, read-only root
filesystem, tmpfs `/tmp`) the probe still returns `41 / 3`. A SHA-256 over all
461 files of the host tree is identical before and after: nothing is written.

Failure is detectable, which is the property the module stands on. A missing
mount, an empty dpkg status and a corrupt dpkg status each produce
`check_success=0` with the pending counts **omitted** — never a zero.

Full detail, including the rejected approaches: [the measurements](../updates-evidence.md).

## Implementation

The spike deliberately left one question open: ship the shell probe and call it
through `inputs.exec`, or invoke `apt-get` from muninn and emit the line protocol
from Rust. Both run the same apt invocation, which is the part that had to be
proven.

**muninn runs itself.** `inputs.exec` is rendered as
`/usr/local/bin/muninn update-check`, and the check lives in
`crates/muninn-modules/src/updates/debian.rs`. The apt argument list is the one
above, unchanged — the measured agreement belongs to those arguments, and moving
them from `sh` to `std::process::Command` does not touch them.

What the port buys is that the invariant stops being a convention: the counts
live inside the `Ok` arm of the result, so a failed check has nothing to print
them from, where the shell probe relied on every `fail()` site remembering to
exit first. It costs one thing — the artefact under test is no longer the one
that was measured — which `scripts/updates-test.sh` closes by running the *image*
against the same fixtures and ground truth.

**The metric shape follows the specified names, not the probe's fields.** The
design fixed `muninn_updates_pending{severity="all"}`. Telegraf joins the
measurement and the field name, so that is a field called `pending` carrying a
`severity` tag — not the probe's `pending_all` and `pending_security`, which
would have produced `muninn_updates_pending_all`. `status` and `reason` are
present on the check line in both the success and failure cases (`reason=none`
when there is nothing to report), because a tag that appeared only on failure
would give one metric two label sets, and both would be exposed together for an
expiration interval after a check recovers.

**A failed check degrades muninn rather than stopping it.** This is the opposite
of the Docker module's rule ([ADR-0010](0010-docker-socket.md)), and the
difference is the point: an unreachable Docker endpoint produces silence that
reads as "no containers", while a failed update check produces `check_success=0`
with a reason.

The module's *preconditions* are unaffected and still refuse the start with exit
`12`: an absent host mount or a non-Debian host is a deployment that cannot
support the module at all. What degrades muninn instead is a check that fails
with its preconditions met — apt refusing, an unreadable package database, an
index format the image does not understand. None of those can be known before
start, none is misrepresented, and none is a reason to stop reporting CPU. The
check runs once at startup so the result reaches the logs, `/status` and
`muninn_module_check_success` within seconds rather than after the first hourly
interval.

## Consequences

- **The runtime image is debian-slim.** Measured against distroless/cc: 88
  packages instead of 10, 26 MB instead of 8, and 5 CRITICAL / 17 HIGH CVEs
  instead of none — all currently unfixable, four of the five CRITICAL in
  `perl-base`, which muninn never invokes and which is present because Debian
  marks it Essential.

  The Trivy gate (fixable CRITICAL/HIGH) stays green. The sharper cost is
  qualitative: a shell and a package manager now exist inside a container that
  mounts the host filesystem. The hardening measures in
  [`hardening.md`](../hardening.md) are therefore load-bearing rather than
  decoration, and all of them were verified to work with this approach.

  A two-variant scheme — distroless by default, debian-slim for the updates
  module — was considered and set aside in favour of one artefact and one CI
  path, consistent with the brief's self-contained-container goal.

- **No capabilities, no root, no writes.** The module adds nothing to the
  container's required privileges beyond the host mount that CPU, memory and disk
  metrics already need.

- **The host mount must include `/usr/lib`.** `/etc/os-release` is a symlink to
  `/usr/lib/os-release`; a mount set containing `/etc` alone leaves it dangling
  and OS detection fails. Supports [ADR-0005](0005-hostfs-mount.md).

- **Real apt does the resolution.** `apt-get -s dist-upgrade` performs full
  dependency resolution, honours holds and pins, and knows about phased updates.
  A hand-written "compare installed against newest candidate" would diverge from
  the host's own answer in exactly the cases that matter, and would forfeit the
  exact agreement measured above.

- **`dist-upgrade`, not `upgrade`.** `upgrade` refuses to install new packages,
  so it under-reports whenever a security fix pulls in a new dependency.

## Alternatives considered

**B — host root read-only plus `chroot`.** Rejected; it fails twice over. With
`--cap-drop=ALL` it cannot chroot at all (`Operation not permitted` — it needs
`CAP_SYS_CHROOT`). With default capabilities it still fails, because apt cannot
work inside a read-only chroot: `E: Unable to mkstemp
/tmp/clearsigned.message… (Read-only file system)`. Making it work would require
an overlay over the host root — more machinery than approach A needs in total.

**C — `nsenter` into the host namespaces.** Rejected. Verified empirically:
it works only with `--pid=host` and `CAP_SYS_ADMIN`, both excluded by the
hardening baseline. Sharing the host PID namespace to count packages is not a
trade worth making.

**D — external host helper.** A systemd timer on the host writing a file muninn
reads. It was planned as the fallback if A failed; since A works under full
hardening, it is not needed. It stays documented for operators who will not mount
the host filesystem at all.

**Parsing dpkg status and apt indices in Rust, avoiding apt entirely.** This
would have kept the image distroless, and it is the option that looks most
attractive on paper. Rejected: it means reimplementing Debian version comparison,
dependency resolution, pinning and hold handling. The measured result above is
worth precisely as much as it is *because* real apt produced it; a
reimplementation would be a plausible answer with no ground truth behind it,
which is the exact failure this module was most at risk of.
