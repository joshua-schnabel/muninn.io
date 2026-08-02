# ADR-0009 — Read host package state via read-only mounts and a simulated upgrade

**Status:** accepted · **Date:** 2026-08-02 · **Decided by:** the
[WP1 spike](../spikes/updates-spike.md)

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

Full detail, including the rejected approaches: [the spike](../spikes/updates-spike.md).

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
