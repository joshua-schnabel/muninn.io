# ADR-0005 — One read-only `/hostfs` mount, not individual host paths

**Status:** accepted · **Date:** 2026-08-02

## Context

A container sees its own namespace. Ask it for CPU, memory, disks or processes
and it answers about itself. For host monitoring that is not a subtle error — it
is a completely different machine, reported with total confidence.

Telegraf solves this through gopsutil's `HOST_*` environment variables, which
redirect its reads to a mounted copy of the host filesystem.

The project brief proposed mounting individual paths: `/proc`, `/sys`, `/etc`,
each at its own target. That looks tighter — mount less, expose less.

## Decision

Mount the host root read-only at a single prefix:

```yaml
volumes:
  - /:/hostfs:ro
```

muninn derives every gopsutil variable from `runtime.host_mount_prefix`:

```text
HOST_MOUNT_PREFIX=/hostfs
HOST_PROC=/hostfs/proc   HOST_SYS=/hostfs/sys   HOST_ETC=/hostfs/etc
HOST_VAR=/hostfs/var     HOST_RUN=/hostfs/run
```

This is the arrangement InfluxData documents, and it is what Telegraf is tested
against.

## Consequences

- `HOST_MOUNT_PREFIX` strips the prefix from reported paths, so a filesystem
  mounted at `/var` is tagged `path=/var` and not `path=/hostfs/var`. Without it,
  every disk metric carries a path that matches nothing an operator recognises,
  and every dashboard filter has to know about the container's internals. This is
  the concrete reason the single-prefix form wins: the individual-mount layout
  has no equivalent, because there is no common prefix to strip.
- The container can read the host filesystem, read-only. That includes `/etc`,
  and therefore `/etc/shadow`. This is a real exposure and is documented plainly
  in `docs/hardening.md` rather than buried.
- Setting `host_mount_prefix: ""` tells muninn it is running directly on the
  host. In a container that produces the failure this ADR exists to prevent —
  plausible numbers about the wrong machine — so muninn warns at startup when the
  prefix is empty and it detects a container.
- Modules declare which host paths they actually need through `requirements()`,
  so `muninn check-runtime` verifies only what is enabled. The single mount is
  the *deployment* simplification; it does not weaken per-module checking.

## Alternatives considered

**Mount individual paths** (`/proc:/host/proc:ro`, `/sys:/host/sys:ro`, …), as
the brief proposed. Rejected for three reasons: there is no common prefix to
strip, so path tags stay wrong; the set of required paths is not stable across
plugins or Telegraf versions, so a plugin addition silently breaks a deployment
that mounted "enough" last year; and it diverges from the configuration
InfluxData tests, which is a bad place to be original.

Note what is *not* gained by mounting individually: `/proc` and `/sys` are the
sensitive reads, and both are in every variant. The narrower mount set mostly
excludes `/home` and `/var/lib`, while keeping the parts that matter.

**Run with `pid: host` and no filesystem mount.** Rejected: it fixes process
visibility only, leaves disk and network wrong, and shares the host PID namespace
— a far larger concession than a read-only filesystem view.
