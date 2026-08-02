# ADR-0003 — The generated Telegraf configuration is ephemeral

**Status:** accepted · **Date:** 2026-08-02

## Context

muninn renders a complete Telegraf configuration from the operator's YAML. That
file has to exist on disk, because Telegraf reads configuration from a path.

Where it lives determines two things that matter: whether it can be edited out
of band, and how exposed the secrets inside it are. It contains resolved values —
the actual InfluxDB token, the actual basic-auth password — because Telegraf
needs them and muninn deliberately does not use environment-variable indirection
in the generated file.

## Decision

The generated configuration is written to `/run/muninn/telegraf.conf`, on a
tmpfs, recreated from scratch on every start.

The directory is created at startup, accessible only to the runtime user, never
persisted, and always overwritten. It is not a volume and must not be mounted
out.

Operators who want to see the configuration use `muninn render-config`, which
redacts secrets by default.

## Consequences

- Secrets never touch persistent storage. A stolen disk image or a backup of the
  container's volumes contains no tokens.
- There is exactly one source of truth. Editing the generated file is pointless
  — the next restart discards it — so the YAML cannot drift from what is running.
- A config change requires a restart. That is the intended operating model
  anyway; there is no reload.
- The container needs a writable tmpfs at `/run/muninn` even with a read-only
  root filesystem. Documented in `docs/hardening.md` and set in the shipped
  compose file.
- Debugging "what is Telegraf actually running" needs `render-config`, not `cat`.
  In exchange, that path redacts, so pasting its output into an issue is safe.

## Alternatives considered

**Write it to a persistent volume.** Rejected: it puts plaintext credentials on
disk for no benefit, and invites hand-editing that the next restart silently
reverts.

**Reference secrets from the generated config via environment variables
(`token = "${INFLUX_TOKEN}"`).** Rejected: it moves the secret from a tmpfs file
into the process environment, where it is visible in `/proc/*/environ` and in
`docker inspect`. A file on a memory-backed filesystem, readable only by the
runtime user, is the stronger position. It would also mean the config muninn
validates is not the config Telegraf resolves, which weakens the pre-start check.

**Keep the configuration in memory and pass it to Telegraf on stdin.** Rejected:
Telegraf takes `--config` as a path. A named pipe would work but complicates the
validation step, which needs to read the same content twice.
