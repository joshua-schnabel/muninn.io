# ADR-0006 — Validate the generated config with `telegraf config check`

**Status:** accepted · **Date:** 2026-08-02

## Context

Before starting Telegraf, muninn must confirm the configuration it just
generated is one Telegraf accepts. Shipping a broken config to a child process
and reading the failure out of its stderr is not validation.

The project brief proposed `telegraf --test`, noting that listener plugins behave
differently under it and would need special handling.

## Decision

Validate with:

```bash
telegraf config check --strict-env-handling --config /run/muninn/telegraf.conf
```

This subcommand loads the configuration files and **initialises the plugins
without starting them**. Syntax and semantic errors detectable without running
are reported; nothing binds a port and nothing collects.

`--test` remains available to operators as a diagnostic. It is not part of
startup.

## Consequences

- The listener problem disappears rather than being handled. `outputs.prometheus_client`
  is a service plugin: under `--test` it would bind `:9273` — the port the real
  process is about to need. A validation step that races the thing it validates is
  worse than none.
- Validation is fast and side-effect free. No collection cycle runs, so nothing
  touches the Docker socket, nothing shells out to the update helper, and startup
  is not delayed by the slowest module.
- A failure here exits with `TELEGRAF_CONFIG` (20), which is documented as a
  muninn bug or a version mismatch — never operator error, because the operator
  never writes TOML.
- `--strict-env-handling` is passed explicitly. Strict handling became the
  default in Telegraf 1.38, and running without an explicit choice prints a
  warning on every start. muninn generates no `${...}` references at all — secrets
  are resolved into the file — so strict handling costs nothing and silences the
  noise.
- **What this does not catch.** `config check` initialises; it does not run. A
  Docker endpoint that does not exist, a port already occupied by something else
  on the host, a mount that is missing — none of these are visible here. That is
  why `muninn check-runtime` exists as a separate step (startup step 5) and why
  readiness is only reported after Telegraf is confirmed running.

## Alternatives considered

**`telegraf --test`**, as the brief proposed. It validates by running one
collection cycle. Rejected: service inputs and outputs behave differently or bind
ports, the cycle costs as long as the slowest module, and it produces metric
output on stdout that startup would have to discard. Confirmed empirically —
`--test` prints `W! Outputs are not used in testing mode!`, so it does not even
validate the output path it appears to.

**`telegraf --once`.** Runs a full gather *and flush*, which means it writes real
metrics to the real InfluxDB before muninn has decided the config is acceptable.
Rejected outright.

**Parse the TOML in muninn and check it against a plugin schema.** Rejected: it
would mean maintaining a copy of Telegraf's option surface for 249 input plugins
and drifting from it on every release. Asking the binary that will run the config
whether it accepts the config is both simpler and correct by construction.
