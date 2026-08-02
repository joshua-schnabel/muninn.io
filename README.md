# muninn.io

Uniform server monitoring without learning Telegraf's configuration format.

You write this:

```yaml
version: 1

modules:
  cpu: { enabled: true }
  memory: { enabled: true }
  disks:
    enabled: true
    exclude_mountpoints: ["/snap*", "/var/lib/docker/*"]

outputs:
  prometheus:
    enabled: true
```

muninn turns it into a complete Telegraf configuration, has Telegraf verify that
configuration, starts Telegraf, supervises it, and tells you honestly whether it
is working.

---

> ## Status: runnable, not yet deployable
>
> **WP0–WP6 are complete.** muninn reads its YAML, generates a Telegraf
> configuration, has Telegraf verify it, starts Telegraf as a supervised child
> and shuts it down cleanly on SIGTERM. `run`, `validate`, `render-config` and
> `version` all work.
>
> What is missing before you can deploy it: the **health server** (WP7) and the
> **container image** (WP8). `check-runtime` and `healthcheck` fail with a
> pointer to the work package that delivers them, and there is no published
> image — the compose example below describes the target, not something you can
> pull today.
>
> Progress and what is next: [`docs/roadmap.md`](docs/roadmap.md).

---

## The problem

Telegraf is an excellent metrics agent with a large and detailed configuration
surface. Setting up uniform monitoring across a fleet means learning it, then
maintaining hand-written TOML per host, then discovering the parts that only bite
in a container:

- collecting the **container's** CPU and memory instead of the host's, with
  plausible numbers and no error;
- a hostname that changes on every recreate, starting a fresh time series each
  time;
- exclusion options that do not exist on the plugins you need them for;
- credentials in a config file that ends up in your configuration management.

muninn takes a small YAML file and handles all of it — or refuses to start and
tells you which key is wrong.

## What muninn does

1. Loads and validates the YAML. Unknown keys are errors, not warnings.
2. Reads secrets from files. Never from the YAML, never from the environment.
3. Checks that enabled modules have what they need — mounts, sockets, host OS.
4. Renders a deterministic Telegraf configuration.
5. Has **Telegraf itself** verify it, before starting anything.
6. Starts Telegraf as a child process and supervises it.
7. Serves liveness, readiness, status and its own operational metrics.
8. Forwards signals and shuts down cleanly.

Telegraf remains the telemetry engine. muninn never touches a metric.

## Design principles

**Opinionated.** Each module exposes the handful of options server operators
actually need, not everything the plugin supports.

**Explicit.** No profiles, no implicit defaults. A module you did not enable is
off. What the YAML says is what is collected.

**Fail before you start.** Everything decidable is decided before Telegraf runs.
A bad config costs an exit code and a log line, not a half-started agent.

**Never report a healthy value for a failed check.** If a module cannot read what
it needs, it reports failure. It does not report zero. This is the sharpest rule
in the project — `0 updates` when the check failed is worse than no metric at
all, because an alert rule cannot tell them apart afterwards.

## Quick start

**1. Write `muninn.yaml`.** Start from
[`config/muninn.minimal.yaml`](config/muninn.minimal.yaml), or copy the annotated
[`config/muninn.example.yaml`](config/muninn.example.yaml) and delete what you do
not need.

**2. Run it.**

```yaml
services:
  muninn:
    image: ghcr.io/joshua-schnabel/muninn.io:0.1.0
    restart: unless-stopped
    stop_grace_period: 30s
    hostname: web-01.example.internal

    volumes:
      - ./muninn.yaml:/etc/muninn/muninn.yaml:ro
      - /:/hostfs:ro

    tmpfs:
      - /run/muninn:mode=0700

    read_only: true
    security_opt: [no-new-privileges:true]
    cap_drop: [ALL]

    ports:
      - "9273:9273"   # host metrics
      - "8080:8080"   # health + agent metrics

    healthcheck:
      test: ["CMD", "/usr/local/bin/muninn", "healthcheck"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 15s
```

**3. Check it.**

```bash
curl -s localhost:8080/health/ready | jq
curl -s localhost:9273/metrics | head
```

Four details in that compose file are easy to get wrong and slow to debug —
`stop_grace_period`, `hostname`, the tmpfs and the two ports. Each is explained
in [`docs/host-mounts.md`](docs/host-mounts.md).

## Two metrics endpoints

The most common setup mistake, so it is up front.

| Port | Serves | Served by |
|---|---|---|
| `9273/metrics` | **Host metrics** — CPU, memory, disk, network | Telegraf |
| `8080/metrics` | **Agent metrics** — is Telegraf running, how long generation took | muninn |

Both are needed. `:9273` alone cannot distinguish a dead agent from a dead host;
`:8080` alone gives you nine agent metrics and no host data.

```yaml
scrape_configs:
  - job_name: muninn-hosts
    static_configs: [{ targets: ["web-01:9273"] }]
  - job_name: muninn-agents
    static_configs: [{ targets: ["web-01:8080"] }]
```

Why they are separate: `muninn_telegraf_running 0` is only useful if you can read
it while Telegraf is down — which is exactly when Telegraf's endpoint is gone.
[ADR-0012](docs/adr/0012-self-metrics-on-health-server.md).

## Modules

| Module | Collects | Default |
|---|---|---|
| `cpu` | Per-core and total CPU time | off |
| `memory` | RAM usage | off |
| `load` | Load averages | off |
| `system` | Uptime, logged-in users | off |
| `swap` | Swap usage and activity | off |
| `processes` | Process counts by state | off |
| `disks` | Filesystem usage | off |
| `disk_io` | Block device I/O | off |
| `network` | Interface counters | off |
| `docker` | Per-container metrics | off — **needs the Docker socket** |
| `updates` | Pending package updates on the host | off |

Everything is off by default; you enable what you want. Per-module options,
metrics and requirements: [`docs/modules.md`](docs/modules.md).

## Outputs

**InfluxDB v2** and **Prometheus**, separately or together. At least one must be
enabled — an agent that collects and sends nowhere is a misconfiguration, so
muninn refuses to start.

## Secrets

Every secret is a file path. There is no key anywhere that takes a token inline.

```yaml
outputs:
  influxdb:
    token_file: /run/secrets/influxdb_token
```

The file must exist, be readable and be non-empty; a trailing newline is
stripped. Errors name the path and never the contents. The value is wrapped in a
type whose `Debug` and `Display` render `***`, so no log line, error or
diagnostic dump can print it.

## Security

muninn mounts your host filesystem read-only and can be given the Docker socket.
Both are stated plainly rather than softened:

- **`/:/hostfs:ro` includes `/etc/shadow`.** The trade is discussed honestly in
  [`docs/host-mounts.md`](docs/host-mounts.md).
- **The Docker socket is root-equivalent**, and mounting it `:ro` does not change
  that — it protects the socket file, not the API. The module is off by default
  and a socket proxy is the recommended deployment.
  [`docs/modules.md#docker`](docs/modules.md#docker)
- The container runs non-root, read-only, with all capabilities dropped and
  `no-new-privileges`. Never `--privileged`.
- Telegraf is pinned by SHA-256 and verified at build time.
  [ADR-0011](docs/adr/0011-telegraf-pinning.md)

Full posture: [`docs/hardening.md`](docs/hardening.md).

## Supported platforms

**Hosts:** Debian and Ubuntu (and compatible derivatives). Other distributions
are not part of the MVP — the architecture allows adding them without rewriting
the Debian path.

**Architectures:** `linux/amd64` and `linux/arm64`.

## Known limitations

- **No raw Telegraf TOML.** A plugin muninn does not model cannot be used. That
  is deliberate — it is what makes validation, determinism and useful error
  messages possible. [ADR-0004](docs/adr/0004-no-raw-toml.md)
- **No configuration reload.** Change the YAML, restart the container.
- **No internal restart loop.** If Telegraf dies, muninn exits and the
  orchestrator restarts the container — so a crash is never invisible inside a
  seemingly-healthy container. [ADR-0002](docs/adr/0002-supervisor-no-restart-loop.md)
- **The image is debian-slim, not distroless.** Reading the host's package state
  needs real `apt` and `dpkg`, which costs 88 packages instead of 10 and a shell
  in the image. Measured, and traded deliberately —
  [`docs/hardening.md`](docs/hardening.md) has the numbers and the mitigations.
- **Windows and macOS hosts** are out of scope.

## Documentation

| | |
|---|---|
| [Roadmap](docs/roadmap.md) | Work packages, status, what is next |
| [Architecture](docs/architecture.md) | Components, startup sequence, state machine |
| [Configuration](docs/configuration.md) | Every key: type, default, effect, security |
| [Modules](docs/modules.md) | Per-module metrics, options and requirements |
| [Host mounts](docs/host-mounts.md) | What to mount and why |
| [Supervision](docs/supervision.md) | Signals, exit codes, diagnosis |
| [Rendering](docs/telegraf-rendering.md) | How the Telegraf config is produced |
| [Hardening](docs/hardening.md) | Container security posture |
| [Risks](docs/risks.md) | Open risks and questions |
| [Decisions](docs/adr/) | Twelve ADRs |

## Related

[huginn.io](https://github.com/joshua-schnabel/huginn.io) — the sibling project,
an uptime and latency monitor by the same maintainer. muninn inherits its
conventions; [`docs/analysis/huginn-review.md`](docs/analysis/huginn-review.md)
records what carried over and what did not.

## Licence

MIT. See [LICENSE](LICENSE).
