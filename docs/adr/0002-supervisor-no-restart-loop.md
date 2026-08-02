# ADR-0002 — muninn is PID 1 and supervises Telegraf, without restarting it

**Status:** accepted · **Date:** 2026-08-02

## Context

Telegraf runs as muninn's child process. Something has to decide what happens
when it exits unexpectedly.

The tempting answer is a restart loop: catch the exit, wait a moment, spawn it
again. It keeps the container "working" through transient failures.

It also produces the worst failure mode this project can have. A container whose
health check passes, whose logs scroll, and inside which Telegraf has been
crash-looping for three weeks, collecting nothing. Nobody is paged, because from
the outside everything is fine.

## Decision

muninn is PID 1. It spawns Telegraf as a direct child, knows its PID, and watches
it.

If Telegraf exits unexpectedly, muninn:

1. sets readiness to false immediately,
2. logs the exit code and signal,
3. exits itself with `TELEGRAF_EXITED` (22).

There is no internal restart loop. The container orchestrator restarts the
container.

An optional bounded restart mechanism may be added later — disabled by default,
at most three attempts, exponential backoff, then a hard exit. Unbounded internal
restarts are excluded permanently.

## Consequences

- A crashing Telegraf is visible where operators already look: container restart
  counts, orchestrator events, `docker ps` status.
- Back-off is handled by software built for it, rather than reimplemented here.
- Restart-on-failure has to be configured on the container (`restart:
  unless-stopped`), which the README and the shipped compose file both do.
- Recovery from a transient failure costs a container restart, roughly a second.
  That is a real cost, and it is the price of the crash never being invisible.
- muninn must handle PID 1 duties properly: forward signals, and not leave a
  zombie behind. Being PID 1 means no init process is going to clean up after it.

## Alternatives considered

**Unbounded internal restart loop.** Rejected — see above. The invisibility is
the problem, not the restarting.

**Restart internally but expose a `muninn_telegraf_restarts_total` counter so the
loop is at least observable.** Rejected as insufficient: it depends on someone
having written an alert rule on a counter they may not know exists. The container
restart count is visible without anyone having planned for it.

**Let Telegraf be PID 1 and run muninn as a helper.** Rejected: muninn has to
outlive the generation step to serve health endpoints and supervise, and inverting
the relationship makes clean shutdown ordering considerably harder — the process
that needs to report "not ready" would be the one being torn down first.
