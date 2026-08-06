# ADR-0012 — muninn's own metrics live on the health server

**Status:** accepted · **Date:** 2026-08-02

## Context

muninn reports on itself: whether Telegraf is running, how long config generation
took, whether each module's last check succeeded, how long it has been up.

The obvious home is the Prometheus output that already exists — Telegraf is
serving `:9273/metrics`, and adding a few families there costs nothing and gives
operators one endpoint.

## Decision

muninn serves its own metrics from its own HTTP server, on the health port,
at `/metrics`. They do not pass through Telegraf.

```text
muninn_info{version,telegraf_version}          gauge
muninn_uptime_seconds                          gauge
muninn_ready                                   gauge  0|1
muninn_telegraf_running                        gauge  0|1
muninn_telegraf_restarts_total                 counter
muninn_config_generation_duration_seconds      gauge
muninn_telegraf_validation_duration_seconds    gauge
muninn_module_check_success{module}            gauge  0|1
muninn_module_check_timestamp_seconds{module}  gauge
```

## Consequences

- **The metrics survive the failure they describe.** `muninn_telegraf_running 0`
  is only useful if you can read it while Telegraf is down — and if it were
  served by `outputs.prometheus_client`, that is exactly when the endpoint would
  be gone. An alert on "Telegraf is not running" would fire as "target down",
  which is also what a network partition, a full disk and a crashed host look
  like. Separating the endpoints means the agent can report its own failure.
- **A deployment has two Prometheus endpoints**, and that is a genuine cost.
  `:9273` carries host metrics, the health port carries agent metrics. Scraping
  the wrong one is the most likely setup mistake muninn invites, so it is called
  out in the annotated example config, in `configuration.md` and in the README,
  next to a working two-job scrape configuration.
- muninn needs a small Prometheus text renderer of its own. It is a few dozen
  lines for nine families with fixed label sets — much less than the coupling it
  avoids.
- Labels stay low-cardinality by construction: version, module name, result
  status. No error strings, no file paths, no PIDs, no container IDs.
- The health port carries both liveness/readiness and metrics, so it may be
  exposed to a metrics scraper. `/status` is on the same port and deliberately
  carries no secrets and no full configuration dump.

## Alternatives considered

**Feed them through Telegraf via `inputs.internal` or an exec input.** Rejected
for the reason above: the metrics vanish precisely when they matter. It also
inverts the dependency — muninn's self-reporting would depend on the process
muninn supervises.

**A third listener, separate from both health and Telegraf.** Rejected: three
ports to document and expose, no benefit over reusing a server that already
exists and already shares muninn's lifecycle.

**Skip self-metrics; the health endpoints are enough.** Rejected: `/health/ready`
answers yes or no at a point in time. It cannot say that generation is getting
slower, that Telegraf has restarted four times today, or that the updates module
has been failing since Tuesday while everything else is fine.
