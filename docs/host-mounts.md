# Host mounts

What a container needs in order to report the host, and why.

## The problem

A container sees its own namespace. Ask it about CPU, memory, disks or processes
and it answers about itself — confidently, with plausible numbers, about the
wrong machine.

This is worth stating sharply because it does not look like a failure. There is
no error. A container reports 2 GB of memory and one filesystem, and someone
builds a dashboard on it.

## The mount

One read-only mount of the host root:

```yaml
volumes:
  - /:/hostfs:ro
```

plus, in `muninn.yaml`:

```yaml
runtime:
  host_mount_prefix: /hostfs
```

muninn derives every gopsutil variable Telegraf needs from that one value:

```text
HOST_MOUNT_PREFIX=/hostfs
HOST_PROC=/hostfs/proc   HOST_SYS=/hostfs/sys   HOST_ETC=/hostfs/etc
HOST_VAR=/hostfs/var     HOST_RUN=/hostfs/run
```

You set one volume and one key. muninn does the rest.

### Why one mount and not several

`HOST_MOUNT_PREFIX` is stripped from reported paths, so a filesystem mounted at
`/var` is tagged `path=/var` rather than `path=/hostfs/var`. Mounting `/proc`,
`/sys` and `/etc` at separate targets has no common prefix to strip, so every
disk metric would carry a path that matches nothing an operator recognises.

The set of paths Telegraf needs also is not stable across plugins or versions —
mounting "enough" today is how a deployment breaks quietly next year. And it is
the configuration InfluxData documents and tests, which is a good place to be
unoriginal.

Full reasoning: [ADR-0005](adr/0005-hostfs-mount.md).

### What this exposes

The container can read the entire host filesystem, read-only. That includes
`/etc`, and therefore `/etc/shadow`.

This is a real exposure and it is stated here rather than buried. Weigh it
honestly: mounting `/proc` and `/sys` alone — the paths that carry the metrics —
already exposes command lines, environment variables and network state for every
process on the host. The extra reach of the full root mount is mostly `/home` and
`/var/lib`.

If the trade is not acceptable, the mitigations are the usual ones: keep the
image minimal and pinned, run non-root with no capabilities, and treat the
monitoring container as part of the host's trust boundary rather than as an
isolated workload. It is a monitoring agent for the host; it is not going to be
less trusted than the host.

## What each module needs

| Module | Needs | Without it |
|---|---|---|
| cpu, memory, load, system, swap | host `/proc` | Container figures — plausible, wrong |
| processes | host `/proc` | Counts the container's few processes |
| disks | host `/proc` **and** the prefix | Container layers only; paths carry `/hostfs` if the prefix is unset |
| disk_io | host `/proc`, `/sys` | No devices at all |
| network | host `/proc` | The container's `eth0` only |
| docker | **Docker socket** — separate, see below | — |
| updates | host `/var`, `/etc` **and** `/usr` | `check_success=0` with a reason — never a count |

All of these are satisfied by the single `/:/hostfs:ro` mount — and the updates
row is the clearest argument for mounting the root rather than a hand-picked list:
`/etc/os-release` is a symlink into `/usr/lib`, so a mount set carrying `/etc` but
not `/usr` leaves it dangling and the module reports "not a Debian host" for a
machine that plainly is.

The updates module also needs somewhere writable for apt's cache and temp files —
the `/run/muninn` tmpfs the deployment already has. It writes nothing to the host
tree, which is why that mount can be, and is, read-only.

muninn checks at startup that the paths its **enabled** modules need are present
and plausible. Nothing is demanded on behalf of a module you did not enable, and
`muninn check-runtime` reports every unmet precondition rather than stopping at
the first.

## The Docker socket is separate

The Docker module needs the Docker Engine API, not a filesystem path. It is not
covered by the host mount and it is not off-by-default by accident: socket access
is equivalent to root on the host, and `:ro` does not change that — it makes the
socket *file* read-only, not the API.

A socket proxy is the recommended deployment. See
[`modules.md`](modules.md#docker) for a working configuration and
[ADR-0010](adr/0010-docker-socket.md) for the reasoning.

## Compose example

```yaml
services:
  muninn:
    image: ghcr.io/joshua-schnabel/muninn.io:0.1.0
    restart: unless-stopped

    # Docker's default stop timeout is 10s, which would kill the container
    # before runtime.shutdown_grace_period (20s) expires.
    stop_grace_period: 30s

    # The container's own hostname would be its ID, which changes on every
    # recreate and starts a new time series each time. Either this, or set
    # agent.hostname in muninn.yaml.
    hostname: web-01.example.internal

    volumes:
      - ./muninn.yaml:/etc/muninn/muninn.yaml:ro
      - /:/hostfs:ro
      - ./influxdb-token:/run/secrets/influxdb_token:ro

    # The generated Telegraf config lives here and holds resolved secrets.
    # Memory-backed, so it never reaches disk.
    tmpfs:
      - /run/muninn:mode=0700

    read_only: true

    security_opt:
      - no-new-privileges:true

    cap_drop:
      - ALL

    environment:
      MUNINN_CONFIG: /etc/muninn/muninn.yaml

    ports:
      - "9273:9273"   # host metrics   (Telegraf)
      - "8080:8080"   # agent metrics + health (muninn)

    healthcheck:
      test: ["CMD", "/usr/local/bin/muninn", "healthcheck"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 15s
```

Four details in there are easy to get wrong and expensive to debug:

- **`stop_grace_period: 30s`** — without it Docker kills the container at 10s and
  the shutdown flush never runs.
- **`hostname:`** — without it (or `agent.hostname`), every recreate starts a new
  time series.
- **`tmpfs: /run/muninn`** — required, because `read_only: true` leaves nowhere to
  write the generated config.
- **two published ports** — `9273` is host metrics, `8080` is agent metrics and
  health. Both are needed; see
  [`configuration.md`](configuration.md#two-metrics-endpoints).

## Verifying

```bash
docker compose exec muninn muninn check-runtime
```

Checks mounts, permissions, secrets, port conflicts, the Docker socket, the host
OS and module support. Non-zero exit on any problem, with every problem listed.

## Related

- [ADR-0005](adr/0005-hostfs-mount.md) — the single-mount decision
- [`modules.md`](modules.md) — per-module requirements
- [`hardening.md`](hardening.md) — the full container security posture
