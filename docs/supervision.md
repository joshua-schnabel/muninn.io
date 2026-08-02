# Supervision, signals and exit codes

muninn is PID 1 in its container and Telegraf is its child. This page is the
operational contract that follows from that.

## Exit codes

Stable, and part of the public contract — write alerting rules and restart
policies against them. A code may gain meaning in a minor release; it will never
change meaning. The authoritative definition is
[`crates/muninn-core/src/exit.rs`](../crates/muninn-core/src/exit.rs).

| Code | Name | Meaning | Usual fix |
|---|---|---|---|
| `0` | OK | Stop signal received, Telegraf exited cleanly | — |
| `2` | CLI | Unknown flag, missing argument, unusable `--config` | Fix the command line |
| `10` | CONFIG | Unreadable file, invalid YAML, unknown key, unknown schema version, no output enabled, port collision | Fix the YAML; the message names the key |
| `11` | SECRET | A secret file is missing, unreadable or empty | Fix the mount, not the config |
| `12` | RUNTIME | An enabled module's precondition is absent: unmounted host path, unreachable Docker socket, unsupported host OS | Fix the deployment; `muninn check-runtime` reports specifics |
| `20` | TELEGRAF_CONFIG | The generated config was rejected by `telegraf config check` | **A muninn bug or version mismatch** — please report it |
| `21` | TELEGRAF_START | Telegraf did not start within the timeout, or its binary is missing or the wrong version | Usually an image problem |
| `22` | TELEGRAF_EXITED | Telegraf exited on its own while supervised | Read the captured Telegraf output |
| `30` | INTERNAL | An invariant broke | Always a muninn bug — please report it |

`1` is deliberately unassigned. It is what a panic or a shell wrapper produces,
and it must stay distinguishable from every deliberate exit.

Code `20` deserves emphasis: the operator never writes TOML, so a rejected
generated config is never their mistake.

## Error classification

### Fatal before start

Everything in startup steps 1–9. The common property is that nothing
irreversible has happened yet: no child process, no listener, no metric written.
Cost is an exit code and a log line.

Unreadable YAML · invalid YAML · unknown key · missing or unknown schema
version · missing, unreadable or empty secret · no output enabled · port
collision · invalid module configuration · missing required host mount ·
invalid generated config · missing or mismatched Telegraf binary.

### Fatal during operation

muninn exits and lets the orchestrator decide.

Telegraf exits unexpectedly (`22`) · the supervisor can no longer determine child
status (`30`) · the health server fails permanently (`30`).

### Degraded — not fatal

Collection continues; the problem is visible but does not stop the agent.

A failing updates module · InfluxDB temporarily unreachable while Telegraf keeps
buffering · a single non-critical collection failing intermittently.

Each of these appears in the logs, in `/status`, and in a `muninn_module_check_success`
metric. Nothing is swallowed — but nothing that is still collecting gets torn
down either.

### Why `Degraded` still reports ready

If a failing updates module made muninn unready, an orchestrator would pull the
container out of service and stop collecting CPU, memory, disk and network
metrics that were working perfectly — because it could not count pending
packages.

The rule is therefore narrow: `Degraded` is reachable only while Telegraf is
running and collecting. Anything that stops collection is `Failed`.

## Signals

### SIGTERM, SIGINT

What `docker stop`, `systemctl stop` and Ctrl+C send.

1. Readiness → false immediately, so load balancers and orchestrators stop
   counting on this instance before anything is torn down.
2. Forward the signal to Telegraf, which flushes its buffers.
3. Wait up to `runtime.shutdown_grace_period` (default `20s`).
4. If Telegraf is still alive, SIGKILL.
5. Exit `0`.

**Watch the interaction with Docker.** Docker's default stop timeout is 10
seconds. With the default 20-second grace period, Docker kills the container
before muninn's grace period expires and the clean shutdown never happens. Either
lower the grace period or set `stop_grace_period: 30s` in compose — the shipped
compose file does the latter.

The grace period should also exceed `agent.flush_interval`, or shutdown discards
the cycle in progress.

### SIGHUP

Logged and otherwise ignored. There is no configuration reload: change the YAML,
restart the container. That is the whole operating model, and it is what lets the
generated config be ephemeral ([ADR-0003](adr/0003-ephemeral-generated-config.md)).

### PID 1 duties

Being PID 1 means no init process will clean up afterwards. muninn reaps its
child and does not leave zombies, and it forwards signals rather than absorbing
them — the failure mode of a naive PID 1 is a container that ignores `docker stop`
entirely and gets killed ten seconds later, every time.

## Telegraf output

Telegraf's stdout and stderr are captured and re-emitted through muninn's logger,
with the source identifiable, in whichever format `logging.format` selects. One
stream leaves the container, and JSON logging stays parseable end to end rather
than being interleaved with Telegraf's own plain text.

## Health semantics

| Endpoint | True when | Use for |
|---|---|---|
| `/health/live` | muninn's event loop is responsive | Restart policy |
| `/health/ready` | Config loaded and validated, Telegraf running, listeners up | Traffic and service registration |

They answer different questions on purpose. A brief InfluxDB outage must not fail
liveness — muninn is fine, the network is not, and restarting would help nothing.
A dead Telegraf must fail readiness immediately, because at that point nothing is
being collected.

`muninn healthcheck` queries `/health/ready` locally and maps it to an exit code
Docker's `HEALTHCHECK` understands.

## Diagnosing a failure

**Exit 10 or 11** — the message names the key or the path. For `11`, check the
mount rather than the config: the path is usually right and the file is not
there.

**Exit 12** — run `muninn check-runtime`, which reports every unmet precondition
rather than stopping at the first.

**Exit 20** — a muninn bug or a Telegraf version mismatch. `muninn render-config`
prints the generated configuration with secrets redacted; that output is safe to
attach to an issue.

**Exit 21** — Telegraf missing, not executable, or a different version than the
image was built for. Confirm with `muninn version`, which prints both.

**Exit 22** — Telegraf died. The captured output immediately before the exit is
the evidence; the exit code and signal are logged with it.

**Container restarting repeatedly** — the exit code tells you which of the above
it is. `docker inspect --format '{{.State.ExitCode}}' <container>` after it
settles, or read the logs from the start of the last attempt.

**Healthy container, no metrics** — muninn is designed to make this hard, but
check readiness rather than liveness, then `/status` for the module list. If a
module you expected is missing, it is not enabled; there are no implicit
defaults.

## Related

- [`architecture.md`](architecture.md) — startup sequence and state machine
- [`configuration.md`](configuration.md) — `runtime.*` keys
- [ADR-0002](adr/0002-supervisor-no-restart-loop.md) — why there is no restart loop
