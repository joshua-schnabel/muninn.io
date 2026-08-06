# ADR-0001 — Embed Telegraf rather than write a metrics engine

**Status:** accepted · **Date:** 2026-08-02

## Context

muninn's goal is uniform server monitoring that an operator can set up without
becoming an expert in the monitoring agent. Two ways to get there: write a
collector, or wrap one.

Writing one means implementing per-platform collection for CPU, memory, load,
disks, block devices, network interfaces, processes, swap and containers, then
output protocols for InfluxDB and Prometheus, then buffering, batching and retry
for both — and then maintaining all of it across kernel and distribution
changes. That work is done, several times over, and none of it is what makes
muninn useful.

What makes muninn useful is the layer above: a small YAML file instead of a
sprawling TOML one, validation that fails before the agent starts, and a process
that behaves correctly in a container.

## Decision

Telegraf is the telemetry engine. muninn owns configuration, validation, process
supervision and health reporting, and ships Telegraf in the same image at a
pinned version.

muninn never handles a metric. It has no metrics pipeline, no buffering, no
retry logic for outputs — all of that is Telegraf's, configured through the
generated file.

## Consequences

- The plugin surface muninn can expose is bounded by Telegraf's. Where Telegraf
  has no plugin, muninn has no module without writing a helper — which is exactly
  the situation the updates module is in (see [ADR-0009](0009-updates-module-approach.md)).
- Telegraf's option names and semantics are an external dependency that can drift
  between minor versions. muninn pins the version and checks it at startup; see
  [ADR-0011](0011-telegraf-pinning.md).
- The image carries two binaries and is larger than a single-purpose agent.
- muninn inherits Telegraf's correctness for the hard parts: gopsutil's
  per-platform collection, and output protocols that already handle backpressure.
- The container needs a supervisor, because muninn is PID 1 and Telegraf is its
  child. See [ADR-0002](0002-supervisor-no-restart-loop.md).

## Alternatives considered

**Write a native collector.** Rejected on scope. It would take the majority of
the project's effort to reach parity with something already available, and the
resulting agent would be less correct on the platform edge cases that gopsutil
has already absorbed.

**Ship a Telegraf configuration generator with no runtime component.** Rejected:
it moves the whole operational problem — validating before start, knowing whether
the agent is actually running, shutting down cleanly — back onto the operator,
which is the problem muninn exists to solve.

**Run Telegraf as a sidecar container and have muninn only write the config to a
shared volume.** Rejected: two containers with an ordering dependency and a
shared writable volume is a more fragile deployment than one container, and the
config would have to be persisted rather than ephemeral
(see [ADR-0003](0003-ephemeral-generated-config.md)).
