<div align="center">

<img src="docs/logo.png" alt="Muninn — a low-poly raven" width="200">

# muninn.io

**Uniform server monitoring without learning Telegraf's configuration format.**

[![CI](https://github.com/joshua-schnabel/muninn.io/actions/workflows/ci.yml/badge.svg)](https://github.com/joshua-schnabel/muninn.io/actions/workflows/ci.yml)
[![Security](https://github.com/joshua-schnabel/muninn.io/actions/workflows/security.yml/badge.svg)](https://github.com/joshua-schnabel/muninn.io/actions/workflows/security.yml)

[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-b7410e?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Telegraf 1.39.2](https://img.shields.io/badge/telegraf-1.39.2%20pinned-22adf6?logo=influxdb&logoColor=white)](docs/adr/0011-telegraf-pinning.md)
![Platforms](https://img.shields.io/badge/platforms-amd64%20%C2%B7%20arm64-4c566a)
[![Coverage gate](https://img.shields.io/badge/coverage%20gate-%E2%89%A5%2080%25-4c9a2a)](docs/testing.md)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

</div>

> *Muninn* (Old Norse: *Memory*) is the second of Odin's two ravens. Huginn flies
> out and observes; Muninn is the one who remembers. **muninn.io** does the
> remembering for your fleet.

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

muninn turns it into a complete Telegraf configuration, has **Telegraf itself**
verify that configuration, starts Telegraf, supervises it, and tells you honestly
whether it is working. Telegraf remains the telemetry engine — muninn never
touches a metric.

Hand-written Telegraf TOML per host is where fleet monitoring usually goes wrong,
and the ways it goes wrong are quiet: the container's CPU collected instead of
the host's, with plausible numbers and no error; a hostname that changes on every
recreate and starts a fresh time series; exclusion options that do not exist on
the plugin you need them for; credentials sitting in a config file that ends up
in configuration management. muninn handles all of it — or refuses to start and
names the key that is wrong.

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

No version is published yet; until the first release, build the image with
`docker build -t muninn:dev .` and use that tag.

## Two metrics endpoints

The most common setup mistake, so it is up front.

| Port | Serves | Served by |
|---|---|---|
| `9273/metrics` | **Host metrics** — CPU, memory, disk, network | Telegraf |
| `8080/metrics` | **Agent metrics** — is Telegraf running, how long generation took | muninn |

Both are needed, and they are separate for a reason: `muninn_telegraf_running 0`
is only useful if you can read it while Telegraf is down, which is exactly when
Telegraf's endpoint is gone. A two-job scrape configuration is in
[`docs/configuration.md`](docs/configuration.md#two-metrics-endpoints);
[ADR-0012](docs/adr/0012-self-metrics-on-health-server.md) has the reasoning.

## What you get

| | |
|---|---|
| **Modules** | `cpu` · `memory` · `load` · `system` · `swap` · `processes` · `disks` · `disk_io` · `network` · `docker` · `updates` |
| **Outputs** | InfluxDB v2 · Prometheus — separately or together, at least one required |
| **Config** | One YAML file. Unknown keys are errors, not warnings |
| **Secrets** | File paths only, redacted by type — never inline, never from the environment |
| **Health** | Liveness, readiness, status and muninn's own operational metrics |
| **Hosts** | Debian and Ubuntu (and compatible derivatives), `linux/amd64` and `linux/arm64` |
| **Container** | Non-root · read-only root filesystem · all capabilities dropped · `no-new-privileges` |

Every module is **off by default** — you enable what you want, and what the YAML
says is what is collected. Per-module options, metrics and host requirements:
[`docs/modules.md`](docs/modules.md).

**Never report a healthy value for a failed check.** If a module cannot read what
it needs, it reports failure; it does not report zero. This is the sharpest rule
in the project — `0 updates` when the check failed is worse than no metric at
all, because an alert rule cannot tell them apart afterwards.

## Security

muninn mounts your host filesystem read-only and can be given the Docker socket.
Both are stated plainly rather than softened:

- **`/:/hostfs:ro` includes `/etc/shadow`.** The trade is discussed honestly in
  [`docs/host-mounts.md`](docs/host-mounts.md).
- **The Docker socket is root-equivalent**, and mounting it `:ro` does not change
  that — it protects the socket file, not the API. The module is off by default
  and a socket proxy is the recommended deployment.
  [`docs/modules.md#docker`](docs/modules.md#docker)
- **Every secret is a file path.** No key anywhere takes a token inline. The
  value is wrapped in a type whose `Debug` and `Display` render `***`, so no log
  line, error or diagnostic dump can print it; errors name the path, never the
  contents.
- **Telegraf is pinned by SHA-256** and verified at build time.
  [ADR-0011](docs/adr/0011-telegraf-pinning.md)

Full posture and the measured CVE trade: [`docs/hardening.md`](docs/hardening.md).
To report a vulnerability: [`docs/SECURITY.md`](docs/SECURITY.md).

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
  in the image. Measured and traded deliberately —
  [`docs/hardening.md`](docs/hardening.md) has the numbers and the mitigations.
- **Windows and macOS hosts** are out of scope.

## Development

```bash
cargo t-all         # every test in the workspace
cargo lint          # clippy --all-targets --all-features -- -D warnings
cargo fmt-check     # formatting, as CI checks it
cargo audit-all     # cargo-deny: advisories, licences, bans, sources
cargo cov-ci        # coverage gate, >= 80 % workspace lines
```

The system suites need the image (`docker build -t muninn:dev .`):

```bash
bash scripts/container-test.sh muninn:dev     # the image, hardened
bash scripts/updates-test.sh muninn:dev       # the updates module, real hosts
bash scripts/integration-test.sh muninn:dev   # the whole stack, with a database
```

Start at [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md); if you are an AI coding
agent, read [`AGENTS.md`](AGENTS.md) first.

## Documentation

| | |
|---|---|
| [Architecture](docs/architecture.md) | Components, startup sequence, state machine |
| [Configuration](docs/configuration.md) | Every key: type, default, effect, security |
| [Modules](docs/modules.md) | Per-module metrics, options and requirements |
| [Host mounts](docs/host-mounts.md) | What to mount and why |
| [Supervision](docs/supervision.md) | Signals, exit codes, diagnosis |
| [Rendering](docs/telegraf-rendering.md) | How the Telegraf config is produced |
| [Hardening](docs/hardening.md) | Container security posture |
| [Testing](docs/testing.md) | Test pyramid, coverage, the no-sleep rule |
| [CI/CD](docs/ci-cd.md) | Pipeline, release path, repository setup |
| [Versioning](docs/versioning.md) | SemVer policy and the stable surface |
| [Roadmap](docs/roadmap.md) | What is still open |
| [Risks](docs/risks.md) | Open risks and questions |
| [Decisions](docs/adr/) | Twelve ADRs |

## Related

[huginn.io](https://github.com/joshua-schnabel/huginn.io) — the sibling project,
an uptime and latency monitor by the same maintainer. muninn inherits its
conventions.

## Licence

MIT. See [LICENSE](LICENSE).
