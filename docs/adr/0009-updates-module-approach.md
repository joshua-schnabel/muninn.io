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

What the port buys is that the invariant stops being a convention. In the shell
probe, "never report zero on failure" was upheld by every `fail()` call site
remembering to `exit` before the counting; in Rust the counts live inside the
`Ok` arm of the result, so a failed check has nothing to print them from. The
precondition ladder and the `Inst`-line parsing are also ordinary unit tests now,
including on a developer machine that has no apt at all.

It costs one thing worth stating: the artefact under test is no longer the
artefact the spike measured. `scripts/updates-test.sh` closes that by running the
*image* against the same fixtures and the same ground truth, so a divergence
between the two shows up as a failing cell rather than as a number nobody
compares.

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
`12`: an absent host mount or a host that is not Debian-family is a deployment
that cannot support the module at all, and every module is treated the same way
there. What degrades muninn is a check that fails with its preconditions met —
apt refusing, an unreadable package database, an index format the image does not
understand. Those cannot be known before start, and none of them is a reason to
stop reporting CPU. Nothing is being misrepresented, so taking a working agent out of
service — and losing CPU, memory, disk and network collection — would cost far
more than it protects. muninn runs the check once at startup so the result is in
the logs, in `/status` and in `muninn_module_check_success` within seconds rather
than after the first hourly interval.

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
