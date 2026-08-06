# Architecture

muninn is one process supervising one child process. Everything below follows
from that.

## The shape

```text
                          ┌──────────────────────────────────────┐
   muninn.yaml  ─────────►│  muninn  (PID 1)                     │
   /run/secrets/* ───────►│                                      │
                          │  config ─► modules ─► renderer       │
                          │                          │           │
                          │                          ▼           │
                          │            /run/muninn/telegraf.conf │
                          │                          │           │
                          │              telegraf config check   │
                          │                          │           │
                          │                          ▼           │
                          │  supervisor ◄──────► telegraf (child)│──► InfluxDB
                          │      │                               │──► :9273/metrics
                          │      ▼                               │
                          │  health server  :8080                │
                          └──────────────────────────────────────┘
                                   /health/live  /health/ready
                                   /status       /metrics
```

Two things are worth noticing in that picture.

**The generated config is a dead end, not a document.** It is written to a tmpfs,
consumed by Telegraf, and never read by anything else. It contains resolved
secret values, which is why it is memory-backed and root-only, and why
`muninn render-config` — the way you are meant to inspect it — redacts them.

**There are two metrics endpoints, on purpose.** Telegraf's `:9273` carries the
host metrics. muninn's `:8080/metrics` carries muninn's own operational metrics.
They have different lifetimes: `muninn_telegraf_running` is worth reading
precisely when Telegraf is not running, which is exactly when Telegraf's endpoint
is gone. See [ADR-0012](adr/0012-self-metrics-on-health-server.md).

## Crates

| Crate | Responsibility |
|---|---|
| `muninn` | CLI, logging setup, startup sequence, supervisor wiring |
| `muninn-core` | Config model, loading, validation, secrets, durations, errors, exit codes |
| `muninn-telegraf` | Typed Telegraf model, TOML renderer, `config check` validator, child process, version check |
| `muninn-modules` | The `MonitoringModule` trait, eleven modules, two outputs |
| `muninn-health` | Liveness, readiness, status, self-metrics |

Dependencies point one way: `muninn` → everything; `muninn-modules` →
`muninn-telegraf` → `muninn-core`; `muninn-health` → `muninn-core`. No cycles,
and `muninn-core` knows nothing about Telegraf.

## Startup sequence

The order is the design. Every step that can fail does so before anything
irreversible happens — steps 1 through 9 touch nothing outside the container's
own tmpfs, so a bad config costs an exit code and a log line.

| # | Step | Failure exits with |
|---|---|---|
| 1 | Parse CLI arguments and environment | `2` CLI |
| 2 | Read the YAML file | `10` CONFIG |
| 3 | Validate schema version, structure, then semantics | `10` CONFIG |
| 4 | Read and check every referenced secret file | `11` SECRET |
| 5 | Check runtime preconditions for enabled modules | `12` RUNTIME |
| 6 | Initialise the enabled modules | `10` CONFIG |
| 7 | Render the Telegraf configuration | `30` INTERNAL |
| 8 | Write it to the runtime directory | `30` INTERNAL |
| 9 | Verify it with `telegraf config check` | `20` TELEGRAF_CONFIG |
| 10 | Start Telegraf as a child process | `21` TELEGRAF_START |
| 11 | Report readiness | — |
| 12 | Supervise until signalled | `22` if Telegraf dies |

Step 3 checks the version *first*. An unknown schema version has to be reported
as an unknown schema version, not as forty complaints about keys that moved.

Step 9 uses `config check`, which initialises plugins without starting them, so
validation never competes with the real process for a port. See
[ADR-0006](adr/0006-validate-with-config-check.md).

## State machine

```text
        Starting
           │
           ▼
   LoadingConfiguration ──────┐
           │                  │
           ▼                  │
  ValidatingConfiguration ────┤
           │                  │
           ▼                  │
     CheckingRuntime ─────────┤
           │                  │
           ▼                  ├──► Failed ──► (exit)
 GeneratingTelegrafConfig ────┤
           │                  │
           ▼                  │
 ValidatingTelegrafConfig ────┤
           │                  │
           ▼                  │
    StartingTelegraf ─────────┘
           │
           ▼
         Ready ◄────────► Degraded
           │                  │
           ▼                  ▼
        Stopping ◄────────────┘
           │
           ▼
        Stopped
```

| State | Meaning | Ready? |
|---|---|---|
| `Starting` | Process is up, nothing read yet | no |
| `LoadingConfiguration` | Reading and deserialising the YAML | no |
| `ValidatingConfiguration` | Schema and semantic rules, secrets | no |
| `CheckingRuntime` | Mounts, permissions, ports, host OS | no |
| `GeneratingTelegrafConfiguration` | Rendering TOML | no |
| `ValidatingTelegrafConfiguration` | Running `telegraf config check` | no |
| `StartingTelegraf` | Child spawned, not yet confirmed running | no |
| `Ready` | Telegraf running, listeners up, everything collecting | **yes** |
| `Degraded` | Telegraf running and collecting, but one non-critical module is failing | **yes** |
| `Stopping` | Stop signal received, waiting for Telegraf to exit | no |
| `Failed` | Unrecoverable; the process is about to exit non-zero | no |
| `Stopped` | Clean exit | no |

### Why `Degraded` is ready

`Degraded` reports ready because the alternative is worse. If a failing updates
module made muninn unready, an orchestrator would pull the container out of
service — and stop collecting CPU, memory, disk and network metrics that were
working perfectly — because it could not count pending packages.

So the rule is narrow: `Degraded` is only reachable while Telegraf is running and
collecting. Anything that stops collection is `Failed`, not `Degraded`. The
failing module is visible in the logs, in `/status`, and in its own
`*_check_success` metric, so the degradation is never silent.

**What reaches it today.** The updates module runs its check once immediately
after readiness — a full apt resolution takes seconds, so holding readiness for it
would delay an orchestrator over something unrelated to collecting metrics — and a
failure moves muninn to `Degraded`.

That is deliberately the opposite of the Docker module, which refuses to start at
all when its endpoint does not answer: Docker's failure mode is *silence* that
reads as "no containers", while a failed update check says so in the metric. A
failure that names itself does not justify taking a working agent out of service.

The updates module's *preconditions* are a separate matter and still refuse the
start with exit 12 (step 5 above). A deployment that cannot support the module —
no host mount, a host that is not Debian-family — is not a degradation; it is a
deployment to fix, and every module is treated the same way there.

### Why there is no restart loop

A dead Telegraf sends muninn to `Failed` and out with exit code 22. muninn does
not restart it internally.

The failure this avoids is the expensive one: a container that reports healthy
from the outside while Telegraf crash-loops invisibly inside it. Handing the
restart decision to Docker or the orchestrator means the crash is counted,
back-off is applied by something built for it, and the restart count is visible
where operators already look. See [ADR-0002](adr/0002-supervisor-no-restart-loop.md).

## Shutdown

On SIGTERM or SIGINT:

1. Readiness goes false immediately, so load balancers and orchestrators stop
   counting on this instance before anything is torn down.
2. The signal is forwarded to Telegraf, which flushes its buffers.
3. muninn waits up to `runtime.shutdown_grace_period`.
4. If Telegraf is still alive, SIGKILL.
5. muninn exits `0`.

What the grace period has to cover is a *write*, not a collection cycle.
Telegraf does not wait for the next flush tick on shutdown — it logs `Hang on,
flushing any cached metrics before shutdown` and flushes immediately. So the
bound that matters is how long one write attempt may take, i.e. the output
timeout, not `agent.flush_interval`.

It should stay below the orchestrator's own stop timeout — Docker's default is
10 seconds, so the 20-second default needs a matching `stop_grace_period` in
compose or Docker kills the container mid-flush.

There is no configuration reload. Change the YAML, restart the container. That
is the whole model, and it is why the generated config can be ephemeral.

## Where to read next

- [`configuration.md`](configuration.md) — every key, its default and its effect
- [`modules.md`](modules.md) — what each module produces and requires
- [`telegraf-rendering.md`](telegraf-rendering.md) — how the TOML is produced
- [`supervision.md`](supervision.md) — signals, exit codes, error classification
- [`host-mounts.md`](host-mounts.md) — what to mount and why
- [`roadmap.md`](roadmap.md) — what is still open

### Architecture decisions

| ADR | Subject |
|---|---|
| [0001](adr/0001-embed-telegraf.md) | Embedding Telegraf rather than writing a metrics engine |
| [0002](adr/0002-supervisor-no-restart-loop.md) | muninn as PID 1, with no internal restart loop |
| [0003](adr/0003-ephemeral-generated-config.md) | The generated configuration is ephemeral |
| [0004](adr/0004-no-raw-toml.md) | No raw Telegraf TOML in the YAML |
| [0005](adr/0005-hostfs-mount.md) | One `/hostfs` mount rather than individual paths |
| [0006](adr/0006-validate-with-config-check.md) | Validating with `config check`, not `--test` |
| [0007](adr/0007-tagdrop-and-render-order.md) | Exclusions via `tagdrop`, and what that forces on the renderer |
| [0008](adr/0008-system-and-load-merge.md) | `load` and `system` render into one plugin instance |
| [0009](adr/0009-updates-module-approach.md) | How the updates module reads host package state |
| [0010](adr/0010-docker-socket.md) | The Docker socket security model |
| [0011](adr/0011-telegraf-pinning.md) | Pinning Telegraf by tarball and checksum |
| [0012](adr/0012-self-metrics-on-health-server.md) | muninn's own metrics live on the health server |
| [0013](adr/0013-image-updates-via-docker-api.md) | Detecting image updates via the Docker Engine API, not a registry client |
