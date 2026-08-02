# ADR-0009 — How the updates module reads the host's package state

**Status:** proposed — decided by the WP1 spike · **Date:** 2026-08-02

This ADR is deliberately unfinished. It records the constraints and the
candidates; the spike in [`../spikes/updates-spike.md`](../spikes/updates-spike.md)
supplies the answer, and this file is completed then. Implementing the module
before that would mean choosing without evidence, which for this particular
feature is how you ship a number that is wrong and looks right.

## Context

The module reports pending package updates on the host. Two facts constrain it.

**Telegraf has no package input plugin.** All 249 input plugins of version 1.39.2
were checked; there is nothing for apt, dpkg or updates. Whatever muninn does,
the result reaches Telegraf through `inputs.exec` with
`data_format = "influx"` — a helper command muninn ships, writing line protocol
to stdout.

**`apt` inside the container reads the container's package database.** Running
`apt-get -s upgrade` in the muninn container reports the updates pending for
debian-slim. Not an error, not a crash: a number. A plausible one. For a
monitoring system this is the worst possible failure mode, because it is
indistinguishable from success until someone checks by hand.

## Candidates

**A — Read-only host mounts, simulated upgrade.** Mount `/var/lib/dpkg`,
`/var/lib/apt/lists`, `/etc/apt` and `/etc/os-release` read-only; point apt at
them with `-o Dir::State::status=…` and friends; run `apt-get -s upgrade`.
Security updates from the candidate version's origin suite. Open questions: where
apt writes despite `-s`, whether the container's apt understands the host's list
format across Debian 12→13 and Ubuntu 22.04→24.04.
*Consequence if chosen:* apt and dpkg must be in the runtime image, so the base
becomes debian-slim rather than distroless.

**B — Host root read-only plus `chroot`.** `chroot /hostfs apt-get -s upgrade`.
Needs `CAP_SYS_CHROOT`, and apt needs writable paths that a read-only chroot does
not provide.

**C — `nsenter` into the host namespaces.** Needs `--pid=host` and
`CAP_SYS_ADMIN`/`CAP_SYS_PTRACE`. This contradicts the hardening requirements
directly and is evaluated only for the record.

**D — External host helper.** A systemd timer on the host writes the result to a
file muninn reads read-only. Contradicts the self-contained container goal, and
is the only option guaranteed to be correct. Documented as a fallback.

## Constraints on any accepted answer

Whatever the spike selects must satisfy all of:

1. It works reproducibly across Debian 12/13 and Ubuntu 22.04/24.04.
2. It never modifies host package data.
3. Failures are unambiguous.
4. It has been verified against a native check on a real host.
5. Required mounts and permissions are documented.

## The invariant that holds regardless

**A failed check reports failure, never zero.**

If the module cannot read the host's package data — missing mount, unreadable
dpkg status, incompatible format — it emits `muninn_updates_check_success 0` and
omits the pending counts entirely. It does not emit `0 updates`, because "no
updates pending" and "I could not look" are opposite conclusions that must never
share a representation.

Until the spike concludes, the module is refused at startup if enabled, rather
than quietly doing nothing.

If no candidate satisfies the constraints, the module ships marked
`experimental`, off by default, checking its preconditions hard when enabled.

## Consequences (pending)

To be completed by WP1. The one already known: the choice determines the runtime
base image, which is why WP1 precedes WP8.
