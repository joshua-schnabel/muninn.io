# Configuration reference

Every key muninn understands. The annotated file this describes is
[`config/muninn.example.yaml`](../config/muninn.example.yaml); the smallest
working file is [`config/muninn.minimal.yaml`](../config/muninn.minimal.yaml).

## Rules that apply everywhere

**Unknown keys are fatal.** Every section rejects keys it does not know. A
misspelled `exclude_mountpoint` stops startup with a message naming the key path,
rather than being silently ignored — which would leave you believing an exclusion
is in effect while it is not.

**Secrets are paths, never values.** Any key ending in `_file` takes a path to a
file containing the secret. There is no key anywhere that accepts a token,
password or private key inline. See [Secret files](#secret-files).

**Durations are strings.** `30s`, `5m`, `1h`. Zero, negative and unparseable
values are rejected with a reason. Bare numbers are rejected too — `interval: 30`
is an error, not thirty of something.

**Precedence** for anything settable in more than one place: CLI argument →
environment variable → YAML → default.

## Two metrics endpoints

The most common setup mistake, so it is stated before the reference rather than
inside it.

| Endpoint | Serves | Configured by |
|---|---|---|
| `outputs.prometheus.listen`, default `:9273` | **Host metrics** — CPU, memory, disk, network | `outputs.prometheus` |
| `health.listen`, default `:8080`, path `/metrics` | **muninn's own metrics** — is Telegraf running, how long generation took | `health` |

Both are needed. Scraping only the health port gives you nine agent metrics and
no host data; scraping only `:9273` means you cannot tell a dead agent from a
dead host. Scrape both:

```yaml
scrape_configs:
  - job_name: muninn-hosts
    static_configs: [{ targets: ["web-01:9273"] }]
  - job_name: muninn-agents
    static_configs: [{ targets: ["web-01:8080"] }]
```

Why they are separate: `muninn_telegraf_running 0` is only useful if you can read
it while Telegraf is down, which is exactly when Telegraf's endpoint is gone.
[ADR-0012](adr/0012-self-metrics-on-health-server.md).

---

## `version`

| | |
|---|---|
| Type | integer |
| Required | **yes** |
| Default | none |
| Example | `version: 1` |

The schema version. muninn refuses to start on a version it does not know, and
checks this before anything else — an unknown version has to be reported as such,
not as forty complaints about keys that moved.

There is currently one version. When a version 2 exists, version 1 files keep
loading; the migration path is part of the design (`ConfigV1` → normalised
model).

---

## `agent`

Global collection behaviour. Maps to Telegraf's `[agent]` section.

### `agent.interval`

| | |
|---|---|
| Type | duration |
| Required | no |
| Default | `30s` |

How often every module collects. Lower gives finer resolution and proportionally
more write volume and storage. 10s is reasonable for a host you are actively
troubleshooting; 60s is reasonable for a fleet.

### `agent.flush_interval`

| | |
|---|---|
| Type | duration |
| Required | no |
| Default | `30s` |

How often collected metrics are written to the outputs. Keeping it equal to
`interval` means a metric is written in the cycle it was collected. Setting it
much higher batches more efficiently at the cost of delaying every metric by up
to that long.

**Interacts with `runtime.shutdown_grace_period`**: the grace period should
exceed this, or a shutdown discards the cycle in progress.

### `agent.hostname`

| | |
|---|---|
| Type | string |
| Required | no |
| Default | `""` (ask the operating system) |
| Example | `hostname: "web-01.example.internal"` |

The `host` tag on every metric.

**In a container, leaving this empty is almost always wrong.** The operating
system answers with the container ID, which changes on every recreate. Every
deploy therefore starts a fresh time series, and your dashboards lose their
history without anything appearing to fail.

Mounting the host's `/etc` does not help: the hostname comes from the UTS
namespace, not from a file. Either set this key, or give the container the host's
name (`hostname:` in compose, or `network_mode: host`).

muninn warns at startup when this is empty and it detects a container.

### `agent.omit_hostname`

| | |
|---|---|
| Type | boolean |
| Required | no |
| Default | `false` |

Emit metrics with no `host` tag at all. Only sensible when something downstream
adds the identity — a Prometheus `honor_labels` setup, or a relabelling rule.

---

## `runtime`

How muninn manages itself and the Telegraf child.

### `runtime.shutdown_grace_period`

| | |
|---|---|
| Type | duration |
| Required | no |
| Default | `20s` |

On SIGTERM or SIGINT, how long Telegraf is given to flush and exit before
SIGKILL.

What this has to cover is a *write*, not a collection cycle. Telegraf does not
wait for the next flush tick on shutdown — it flushes immediately — so the bound
that matters is `outputs.influxdb.timeout`, not `agent.flush_interval`. muninn
warns if the grace period does not exceed it, because then not even one write
attempt can complete.

It should stay below the orchestrator's own stop timeout — **Docker's default is
10 seconds**, so the 20s default here needs `stop_grace_period: 30s` in compose,
or Docker kills the container mid-flush and the grace period never applies.

### `runtime.telegraf_start_timeout`

| | |
|---|---|
| Type | duration |
| Required | no |
| Default | `15s` |

How long Telegraf may take to come up before muninn gives up and exits with code
21. Generous enough for a loaded host, short enough that a broken deploy fails
fast rather than hanging in "starting" indefinitely.

### `runtime.generated_config_path`

| | |
|---|---|
| Type | path |
| Required | no |
| Default | `/run/muninn/telegraf.conf` |

Where the generated Telegraf configuration is written.

**Security:** this file contains resolved secret values in plaintext. It must
live on a tmpfs and must never be mounted out or persisted. Use
`muninn render-config` to inspect the configuration; that redacts secrets. See
[ADR-0003](adr/0003-ephemeral-generated-config.md).

### `runtime.host_mount_prefix`

| | |
|---|---|
| Type | path or `""` |
| Required | no |
| Default | `/hostfs` |

Where the host filesystem is mounted inside the container. muninn derives
Telegraf's `HOST_PROC`, `HOST_SYS`, `HOST_ETC`, `HOST_VAR`, `HOST_RUN` and
`HOST_MOUNT_PREFIX` from this one value, so a single `-v /:/hostfs:ro` is all the
container needs.

`""` means "running directly on the host, no prefix applies".

**Security and correctness:** setting this to `""` inside a container produces the
failure mode muninn exists to prevent — Telegraf reports the *container's* CPU,
memory and disks as the host's, with numbers that look entirely plausible.
muninn warns when the prefix is empty and it detects a container. See
[ADR-0005](adr/0005-hostfs-mount.md) and [`host-mounts.md`](host-mounts.md).

---

## `logging`

### `logging.format`

| | |
|---|---|
| Type | `human` \| `json` |
| Required | no |
| Default | `human` |
| Environment | `MUNINN_LOG_FORMAT` |

`json` emits one complete object per line. Secrets are redacted in both formats.

### `logging.level`

| | |
|---|---|
| Type | `trace` \| `debug` \| `info` \| `warn` \| `error` |
| Required | no |
| Default | `info` |
| Environment | `MUNINN_LOG_LEVEL` |

Applies to muninn. Telegraf has only two verbosity settings, so the mapping is
coarse: `trace`/`debug` set Telegraf's `debug = true`, `warn`/`error` set
`quiet = true`, `info` sets neither.

---

## `health`

### `health.listen`

| | |
|---|---|
| Type | `address:port` |
| Required | no |
| Default | `0.0.0.0:8080` |

muninn's own HTTP server: `/health/live`, `/health/ready`, `/status`, `/metrics`.

Must be `0.0.0.0` in a container — a published port reaches the container's bridge
address, never its loopback. Publishing a port that is bound to `127.0.0.1`
inside the container reaches nothing, which looks like a networking problem and
is not.

Must not collide with `outputs.prometheus.listen`. muninn checks, including the
case where one address is a wildcard and the other is not: `0.0.0.0:8080` and
`127.0.0.1:8080` cannot both bind.

**Security:** `/status` carries versions, uptime, enabled modules and the last
Telegraf exit — no secrets and no configuration dump. It is still information
about your infrastructure; put the health port on a trusted network.

---

## `modules`

Every module is enabled explicitly. There are no profiles, and a module you did
not name is off. Per-module options are documented in
[`modules.md`](modules.md); this section covers the shape.

```yaml
modules:
  cpu:
    enabled: true
  disks:
    enabled: true
    exclude_filesystems: [tmpfs, devtmpfs]
```

| Module | Default | Options | Needs |
|---|---|---|---|
| `cpu` | off | — | host `/proc` |
| `memory` | off | — | host `/proc` |
| `load` | off | — | host `/proc` |
| `system` | off | — | host `/proc` |
| `swap` | off | — | host `/proc` |
| `processes` | off | — | host `/proc` |
| `disks` | off | `exclude_filesystems`, `exclude_mountpoints`, `include_mountpoints` | host `/proc`, `/hostfs` |
| `disk_io` | off | `include_devices`, `exclude_devices` | host `/proc`, `/sys` |
| `network` | off | `include_interfaces`, `exclude_interfaces` | host `/proc` |
| `docker` | off | `endpoint`, `container_include`, `container_exclude`, `container_states`, `timeout` | Docker socket |
| `updates` | off | `interval`, `security_only_metric` | host `/hostfs` (same mount as the rest) |
| `image_updates` | off | `endpoint`, `timeout`, `interval`, `container_include`, `container_exclude` | Docker socket |

### Include and exclude

They are different mechanisms, and it matters.

**Include lists** are plugin options: Telegraf collects only what matches, so an
empty list means "everything". Setting one turns the module into an allow-list —
a filesystem or interface that appears next month will *not* be monitored until
someone adds it.

**Exclude lists** are metric filters. Telegraf collects everything and then
discards matches. Where both are set, the include narrows collection first and
the exclude drops from what remains.

Excludes are filters rather than options because the plugins have no exclusion
options at all — see [ADR-0007](adr/0007-tagdrop-and-render-order.md).

Both accept glob patterns: `veth*`, `/var/lib/docker/*`.

### `modules.updates`

Off by default. Reads the host's package state through the same read-only host
mount the other modules use; verified against Debian 12/13 and Ubuntu 22.04/24.04
to reproduce each host's own answer exactly. See
[`updates-evidence.md`](updates-evidence.md).

It is off by default because it is the one module that requires `apt` and `dpkg`
in the image, which makes the runtime base debian-slim rather than distroless —
see [`hardening.md`](hardening.md) for what that costs.

What it will never do: report `0` updates when it could not read the host's
package data. A failed check reports the failure and omits the counts.

Unlike `modules.docker`, a failed check here does **not** stop muninn: it moves
to `degraded` and keeps collecting everything else. Its preconditions still do —
an absent host mount or a non-Debian host is exit `12`, as for every module. The difference is that this
failure is visible in the metrics as `check_success=0` with a reason, where an
unreachable Docker endpoint would be indistinguishable from a host with no
containers. `muninn update-check --hostfs /hostfs` runs exactly what Telegraf
runs, which is the fastest way to see the reason. Per-reason causes and fixes are
in [`modules.md`](modules.md#what-a-reason-means).

### `modules.docker`

Off by default, and that is a security decision rather than a convenience one.
Enabling this module with an endpoint that does not answer is a **startup
failure** (exit `12`), not an empty metric set. muninn issues one `GET /_ping`
against `endpoint` before it starts and requires a `200`. The reason is that a
Docker module collecting nothing is indistinguishable from a host running no
containers, and a monitoring system must not leave that ambiguous.

`container_states` selects which containers are collected (default `[running]`).
Add `exited` to keep reporting containers that stopped — see
[`modules.md`](modules.md#docker) for what that costs.

Access to the Docker socket is equivalent to root on the host, and mounting it
`:ro` does not change that — it protects the socket file, not the API. See
[ADR-0010](adr/0010-docker-socket.md) and [`modules.md`](modules.md#docker) for a
socket-proxy configuration.

### `modules.image_updates`

Off by default, for the same security reason as `modules.docker` — it needs the
same Docker socket, and shares its startup reachability check (exit `12` if
`endpoint` does not answer `GET /_ping`).

For each running container, matched against `container_include`/
`container_exclude`, this asks the Docker daemon to resolve the container's
image reference against its registry (`GET /distribution/{name}/json`) and
compares the digest it gets back to the one the daemon recorded when the
running image was pulled. muninn never speaks HTTPS to a registry itself — the
daemon does, with whatever credentials the host already has. See
[ADR-0013](adr/0013-image-updates-via-docker-api.md).

Like `modules.updates`, a failed check degrades rather than stops muninn: a
container whose image cannot be judged reports `check_success=0` with a reason
on its own series, and every other container's verdict is unaffected.
`muninn image-check --endpoint unix:///var/run/docker.sock` runs exactly what
Telegraf runs. Per-reason causes and fixes are in
[`modules.md`](modules.md#image_updates).

Registry lookups are rate-limited, so `interval` defaults to `1h` and, like
`modules.updates.interval`, is rejected below one minute.

---

## `outputs`

**At least one output must be enabled.** An agent that collects metrics and sends
them nowhere is a misconfiguration, so muninn refuses to start. Both may run at
once.

### `outputs.influxdb`

```yaml
outputs:
  influxdb:
    enabled: true
    url: "https://influxdb.example.internal:8086"
    organization: "infrastructure"
    bucket: "servers"
    token_file: "/run/secrets/influxdb_token"
    timeout: 5s
    tls:
      ca_file: null
      cert_file: null
      key_file: null
      insecure_skip_verify: false
```

| Key | Type | Required | Default | Notes |
|---|---|---|---|---|
| `enabled` | boolean | no | `false` | |
| `url` | URL | **yes** when enabled | — | Scheme and port included |
| `organization` | string | **yes** when enabled | — | |
| `bucket` | string | **yes** when enabled | — | |
| `token_file` | path | **yes** when enabled | — | See [Secret files](#secret-files) |
| `timeout` | duration | no | `5s` | Failed writes are retried by Telegraf |
| `tls.ca_file` | path | no | `null` | Custom CA bundle; unset uses the system trust store |
| `tls.cert_file` | path | no | `null` | Client certificate for mutual TLS |
| `tls.key_file` | path | no | `null` | Must be set together with `cert_file` |
| `tls.insecure_skip_verify` | boolean | no | `false` | **See below** |

**`insecure_skip_verify`** disables certificate verification entirely. Anyone able
to intercept the connection can then read your metrics and feed you fabricated
ones. If a certificate does not validate, fix the certificate or set
`tls.ca_file` — this key is not the answer. muninn logs a prominent warning for
as long as it is true.

### `outputs.prometheus`

```yaml
outputs:
  prometheus:
    enabled: true
    listen: "0.0.0.0:9273"
    path: "/metrics"
    expiration_interval: 60s
    basic_auth:
      username: null
      password_file: null
```

| Key | Type | Required | Default | Notes |
|---|---|---|---|---|
| `enabled` | boolean | no | `false` | |
| `listen` | `address:port` | no | `0.0.0.0:9273` | `0.0.0.0` in a container |
| `path` | path | no | `/metrics` | |
| `expiration_interval` | duration | no | `60s` | See below |
| `basic_auth.username` | string | no | `null` | Both keys or neither |
| `basic_auth.password_file` | path | no | `null` | See [Secret files](#secret-files) |

**`expiration_interval`** is how long a metric stays served after it was last
collected. Shorter than your scrape interval and Prometheus sees gaps; much
longer and a disappeared host keeps serving its last known value as though it
were current. Two to three collection intervals is a reasonable band.

**Security:** the endpoint is unauthenticated unless `basic_auth` is set. Host
metrics reveal a fair amount about a machine — mounted filesystems, network
interfaces, running process counts. Put it on a trusted network, or set basic
auth, or both.

---

## Secret files

Any key ending in `_file` takes a path. There is no key anywhere that accepts a
secret value inline, and that is deliberate: a token written into this file ends
up in your configuration management, your backups and every `docker inspect`. A
path does not.

muninn requires the file to exist, be readable, and be non-empty. A trailing
newline is stripped. Any of those failing stops startup with exit code 11.

**Error messages name the path and never the contents.** The value is wrapped in
a type whose `Debug` and `Display` both render `***`, so no log line, error or
diagnostic dump can print it — that is a property of the type, not a convention
someone has to remember.

`muninn render-config` redacts by default. Its output is safe to paste into an
issue.

With Docker:

```yaml
services:
  muninn:
    volumes:
      - ./influxdb-token:/run/secrets/influxdb_token:ro
```

or a proper Docker/Swarm secret, which lands under `/run/secrets/` on a tmpfs.
muninn accepts any path — `/run/secrets/` is a convention, not a requirement.

---

## Environment variables

| Variable | Overrides |
|---|---|
| `MUNINN_CONFIG` | The config file path (`--config`) |
| `MUNINN_LOG_LEVEL` | `logging.level` |
| `MUNINN_LOG_FORMAT` | `logging.format` |

Only these. Module and output settings are not environment-overridable: they
belong in the file that is meant to be the single readable description of what
this agent does.

An environment variable with an unusable value **warns and keeps the previous
setting** rather than silently falling back to a default — a typo in a deployment
should not be indistinguishable from a deliberate choice.

---

## Related

- [`modules.md`](modules.md) — per-module options, produced metrics and requirements
- [`host-mounts.md`](host-mounts.md) — what to mount for which module
- [`supervision.md`](supervision.md) — exit codes and error classification
- [`architecture.md`](architecture.md) — startup sequence and state machine
