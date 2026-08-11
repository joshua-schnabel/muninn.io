# muninn's own metrics

Every `muninn_*` family, what it means, and what its labels are.

These are the metrics **about muninn**, served on the health port at
`/metrics`. Host metrics come from Telegraf on its own port —
[`configuration.md`](configuration.md#two-metrics-endpoints) explains why there
are two endpoints and which one you probably want.

[`versioning.md`](versioning.md) makes the names, label names and units here a
**stable surface**: new families may appear in a minor release, and renaming or
removing one is breaking. This page exists because that promise had no canonical
list — the only enumeration was
[ADR-0012](adr/0012-self-metrics-on-health-server.md), which is a decision
record rather than a reference, and it had already drifted: it omitted
`muninn_state` and still listed a counter that no longer exists (finding
F-17 of the 1.0 review).

**Checked, not maintained by hand.** `every_documented_metric_is_rendered_and_no_other` in
`crates/muninn-health/src/metrics.rs` renders a state with every field populated
and asserts that the set of families it emits is exactly the set named in the
table below. A family added to the renderer and not to this page fails the
test, and so does the reverse.

## The families

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `muninn_info` | gauge | `version`, `telegraf_version` | Always `1`. The conventional "constant with the interesting bits as labels", so a dashboard can join on version without parsing it out of anything |
| `muninn_state` | gauge | `state` | Always `1`; the supervisor state is the label. One of the ten in [`architecture.md`](architecture.md#state-machine) — every one of them reachable, which is a rule rather than an accident |
| `muninn_uptime_seconds` | gauge | — | Process uptime. muninn's, not Telegraf's |
| `muninn_ready` | gauge | — | `1` when `/health/ready` would answer 200. `degraded` counts as ready, deliberately — see [`architecture.md`](architecture.md#why-degraded-is-ready) |
| `muninn_telegraf_running` | gauge | — | `1` when Telegraf is running as a supervised child. Worth reading precisely when it is `0`, which is when Telegraf's own endpoint is gone |
| `muninn_config_generation_duration_seconds` | gauge | — | Time to render the Telegraf configuration. **Absent** until it has happened |
| `muninn_telegraf_validation_duration_seconds` | gauge | — | Time taken by `telegraf config check`. **Absent** until it has happened |
| `muninn_module_check_success` | gauge | `module` | `1` when that module's last self-check succeeded. Absent when no module runs one |
| `muninn_module_check_timestamp_seconds` | gauge | `module` | When that check last completed, in seconds since the Unix epoch |

### Absent is not zero

The two duration families are **omitted** rather than reported as `0` before the
step they measure has run. Zero reads as "instantaneous" on a graph, which is a
different claim from "has not happened", and the difference matters on exactly
the scrape where you are trying to work out how far startup got.

The same reasoning is why `muninn_module_check_*` is absent when no module has a
self-check, rather than present and empty.

### `muninn_telegraf_restarts_total` is gone

It reported the number of times muninn had restarted Telegraf, and the answer
was always zero — muninn deliberately has no internal restart loop
([ADR-0002](adr/0002-supervisor-no-restart-loop.md)), so nothing ever
incremented it. Its own help text said as much.

A counter that can only be zero invites an alert rule that can never fire, and
`versioning.md` would have frozen it at 1.0. Removed rather than kept for a
bounded restart that may never be built; if one is, the metric comes back with
it, which is a minor release rather than a breaking one.

A process-local counter could not have answered the question ADR-0012 used as
its example — "how many restarts today" — in any case, because a muninn restart
resets it.

## What is deliberately not here

**Anything from the configuration.** `/metrics` and `/status` both refuse to
carry it: no paths, no addresses beyond the one actually bound, no rendered
TOML, no secrets. A diagnostic endpoint is not a reason to hand out the agent's
configuration, and both surfaces may be exposed to a scraper.

**Per-module metric data.** `muninn_updates_pending` and the
`muninn_container_image_updates_*` families are *host* facts, produced by the
modules and carried by Telegraf to the outputs. They are documented in
[`modules.md`](modules.md). Only muninn's own operational state is here.

## Related

- [`architecture.md`](architecture.md) — the state machine behind `muninn_state`
- [`versioning.md`](versioning.md) — what makes these names stable
- [`modules.md`](modules.md) — the metrics the modules produce
- [ADR-0012](adr/0012-self-metrics-on-health-server.md) — why these are on the
  health port and not on Telegraf's
