# Module reference

One section per module: what it produces, which Telegraf plugin it renders to,
what it needs from the host, and where it stops being trustworthy.

All modules are **off by default**. `enabled: true` is the only thing every
module has in common.

Verified against **Telegraf 1.39.2**. Every plugin option named here exists in
that version's `sample.conf`; WP0's verification suite checks that mechanically.

## At a glance

| Module | Plugin | Default | Needs |
|---|---|---|---|
| [cpu](#cpu) | `inputs.cpu` | off | host `/proc` |
| [memory](#memory) | `inputs.mem` | off | host `/proc` |
| [load](#load) | `inputs.system` | off | host `/proc` |
| [system](#system) | `inputs.system` | off | host `/proc` |
| [swap](#swap) | `inputs.swap` | off | host `/proc` |
| [processes](#processes) | `inputs.processes` | off | host `/proc` |
| [disks](#disks) | `inputs.disk` | off | host `/proc`, mount prefix |
| [disk_io](#disk_io) | `inputs.diskio` | off | host `/proc`, `/sys` |
| [network](#network) | `inputs.net` | off | host `/proc` |
| [docker](#docker) | `inputs.docker` | off | **Docker socket** |
| [updates](#updates) | `inputs.exec` | off | see section — experimental |

Everything in the "host `/proc`" column is satisfied by one `-v /:/hostfs:ro`
mount plus `runtime.host_mount_prefix`. See [`host-mounts.md`](host-mounts.md).

---

## cpu

Per-core and aggregate CPU time distribution.

```yaml
modules:
  cpu:
    enabled: true
```

**Renders to**

```toml
[[inputs.cpu]]
  percpu = true
  totalcpu = true
  collect_cpu_time = false
  report_active = false
```

**Metrics** — measurement `cpu`, tag `cpu` (`cpu0`, `cpu1`, …, `cpu-total`):
`usage_user`, `usage_system`, `usage_idle`, `usage_iowait`, `usage_nice`,
`usage_irq`, `usage_softirq`, `usage_steal`, `usage_guest`.

**Notes.** Per-core series are emitted, so a 64-core host produces 65 series per
field. That is usually what you want — a single saturated core is invisible in
the average — but it is worth knowing before pointing this at a large fleet.

`report_active` is off because the resulting `usage_active` includes `iowait`,
which makes it read as CPU saturation when the machine is actually waiting on
disk.

**Limits.** `usage_steal` is only meaningful on virtualised hosts. Container CPU
limits are not reflected here; that is what the [docker](#docker) module is for.

---

## memory

```yaml
modules:
  memory:
    enabled: true
```

**Renders to** `[[inputs.mem]]` — no options.

**Metrics** — measurement `mem`: `total`, `available`, `used`, `free`, `cached`,
`buffered`, `active`, `inactive`, `used_percent`, `available_percent`.

**Notes.** Use `available` and `available_percent` for alerting, not `free`. On
Linux `free` excludes the page cache, which the kernel will hand back on demand;
alerting on it means paging someone about memory the machine is using
productively.

---

## load

System load averages.

```yaml
modules:
  load:
    enabled: true
```

**Renders to** `[[inputs.system]]` with the `load` group. If [system](#system) is
also enabled, both contribute to **one** plugin instance:

```toml
[[inputs.system]]
  include = ["load", "uptime", "users"]
```

There is no `inputs.load` in Telegraf; two separate instances would collect every
metric twice. See [ADR-0008](adr/0008-system-and-load-merge.md).

**Metrics** — measurement `system`: `load1`, `load5`, `load15`, `n_cpus`.

**Notes.** Load is only interpretable relative to core count, which is why
`n_cpus` comes with it. Load 8 is idle on a 32-core box and a crisis on a 2-core
one.

In a container, load average is read from the host's `/proc/loadavg` when the
mount prefix is set — and it is genuinely host-wide. There is no per-container
load average in Linux.

---

## system

Uptime and logged-in users.

```yaml
modules:
  system:
    enabled: true
```

**Renders to** `[[inputs.system]]` with the `uptime` and `users` groups — merged
with [load](#load) when both are on.

**Metrics** — measurement `system`: `uptime`, `uptime_format`, `n_users`.

**Notes.** Enabling `system` alone gives uptime and users but **not** load, even
though Telegraf's own default (`include = ["legacy"]`) would include it. muninn
is explicit rather than convenient here: a module you did not enable does not
collect.

`n_users` counts entries in utmp. In a container that is the container's utmp
unless the host's `/var/run` is visible through the mount prefix.

---

## swap

```yaml
modules:
  swap:
    enabled: true
```

**Renders to** `[[inputs.swap]]` — no options.

**Metrics** — measurement `swap`: `total`, `used`, `free`, `used_percent`,
`in`, `out`.

**Notes.** Worth enabling even on hosts with no swap configured: the module costs
nothing there, and "this host started swapping" is exactly the signal you cannot
reconstruct after the fact.

`in` and `out` are the interesting fields. Steady swap *usage* on a long-running
host is normal; sustained swap *activity* means the machine is thrashing.

---

## processes

Process counts by state.

```yaml
modules:
  processes:
    enabled: true
```

**Renders to** `[[inputs.processes]]` — no options.

**Metrics** — measurement `processes`: `total`, `running`, `sleeping`,
`blocked`, `zombies`, `stopped`, `idle`, `total_threads`.

**Requires** the host's `/proc`. Without `runtime.host_mount_prefix` set and the
host filesystem mounted, this counts the container's handful of processes —
plausible small numbers about the wrong thing.

**Notes.** `zombies` climbing steadily means a parent process is not reaping
children. `blocked` is processes in uninterruptible sleep, usually waiting on
I/O; a persistently non-zero value points at storage rather than CPU.

---

## disks

Filesystem usage per mount point.

```yaml
modules:
  disks:
    enabled: true
    exclude_filesystems: [tmpfs, devtmpfs, squashfs, overlay]
    exclude_mountpoints: ["/snap*", "/var/lib/docker/*"]
    include_mountpoints: []
```

| Option | Type | Default | Renders to |
|---|---|---|---|
| `exclude_filesystems` | list of strings | `[]` | `ignore_fs` |
| `exclude_mountpoints` | list of globs | `[]` | `[inputs.disk.tagdrop] path` |
| `include_mountpoints` | list of paths | `[]` | `mount_points` |

**Renders to**

```toml
[[inputs.disk]]
  ignore_fs = ["tmpfs", "devtmpfs", "squashfs", "overlay"]
  [inputs.disk.tagdrop]
    path = ["/snap*", "/var/lib/docker/*"]
```

The `tagdrop` table comes **last**. `inputs.disk` has no mount-point exclusion
option, so path exclusions have to be metric filters — and Telegraf requires
sub-tables at the end of a plugin block. Putting them earlier produces a config
that passes validation and silently ignores every option after the header. See
[ADR-0007](adr/0007-tagdrop-and-render-order.md).

**Metrics** — measurement `disk`, tags `path`, `device`, `fstype`, `mode`:
`total`, `used`, `free`, `used_percent`, `inodes_total`, `inodes_used`,
`inodes_free`, `inodes_used_percent`.

**Notes.** `used_percent` is `used / (used + free)`, which is **not** what `df`
reports. `df` uses `used / total`, and the difference is the reserved blocks —
typically 5 % on ext4. Expect muninn's number to sit a few points above `df`'s on
a full disk. This is Telegraf's definition and muninn does not change it, because
silently disagreeing with upstream is worse than disagreeing with `df`.

Watch inodes as well as bytes. A filesystem full of small files runs out of
inodes while `used_percent` still looks healthy.

**Requires** `runtime.host_mount_prefix`, which does double duty: it lets
gopsutil read the host's mount table, and it strips the prefix from the reported
`path` tag so `/var` is tagged `/var` and not `/hostfs/var`.

---

## disk_io

Block device I/O counters.

```yaml
modules:
  disk_io:
    enabled: true
    include_devices: []
    exclude_devices: ["loop*", "ram*"]
```

| Option | Type | Default | Renders to |
|---|---|---|---|
| `include_devices` | list of globs | `[]` | `devices` |
| `exclude_devices` | list of globs | `[]` | `[inputs.diskio.tagdrop] name` |

**Renders to**

```toml
[[inputs.diskio]]
  [inputs.diskio.tagdrop]
    name = ["loop*", "ram*"]
```

**Metrics** — measurement `diskio`, tag `name`: `reads`, `writes`,
`read_bytes`, `write_bytes`, `read_time`, `write_time`, `io_time`,
`weighted_io_time`, `iops_in_progress`, `merged_reads`, `merged_writes`.

**Notes.** These are cumulative counters, not rates. Take a derivative when
graphing; a raw `read_bytes` graph is a line going up.

Excluding `loop*` matters on Ubuntu, where every installed snap is a loop device.
A default install has a dozen before you add anything.

Partitions and their parent device are both reported (`sda`, `sda1`, `sda2`), so
summing across all devices double-counts.

**Requires** the host's `/proc` and `/sys`.

---

## network

Network interface counters.

```yaml
modules:
  network:
    enabled: true
    include_interfaces: []
    exclude_interfaces: [lo, "veth*", "br-*", docker0]
```

| Option | Type | Default | Renders to |
|---|---|---|---|
| `include_interfaces` | list of globs | `[]` | `interfaces` |
| `exclude_interfaces` | list of globs | `[]` | `[inputs.net.tagdrop] interface` |

**Renders to**

```toml
[[inputs.net]]
  [inputs.net.tagdrop]
    interface = ["lo", "veth*", "br-*", "docker0"]
```

**Metrics** — measurement `net`, tag `interface`: `bytes_sent`, `bytes_recv`,
`packets_sent`, `packets_recv`, `err_in`, `err_out`, `drop_in`, `drop_out`.

**Notes.** Cumulative counters again — take a derivative.

The `veth*` exclusion is not cosmetic on a Docker host. Each container gets a
veth interface named after an ephemeral ID; every container that starts and stops
leaves a dead time series behind, and a host cycling through containers
accumulates them indefinitely. This is the highest-cardinality trap in the module
set.

`err_in`/`err_out` and `drop_in`/`drop_out` are worth alerting on. They are
usually zero, and a non-zero value means a real cable, driver or buffer problem.

---

## docker

Per-container metrics from the Docker Engine API.

```yaml
modules:
  docker:
    enabled: false
    endpoint: "unix:///var/run/docker.sock"
    container_include: []
    container_exclude: []
    timeout: 5s
```

| Option | Type | Default | Renders to |
|---|---|---|---|
| `endpoint` | URL | `unix:///var/run/docker.sock` | `endpoint` |
| `container_include` | list of globs | `[]` | `container_name_include` |
| `container_exclude` | list of globs | `[]` | `container_name_exclude` |
| `timeout` | duration | `5s` | `timeout` |

**Renders to**

```toml
[[inputs.docker]]
  endpoint = "unix:///var/run/docker.sock"
  timeout = "5s"
  container_state_include = ["running"]
  perdevice_include = ["cpu"]
  total_include = ["cpu", "blkio", "network"]
```

**Metrics** — measurements `docker`, `docker_container_cpu`,
`docker_container_mem`, `docker_container_net`, `docker_container_blkio`,
`docker_container_status`, tagged with `container_name` and `container_image`.

### Security — read this before enabling

**Access to the Docker socket is equivalent to root on the host.** Anyone who can
write to it can start a container with the host filesystem mounted and
`--privileged`.

Mounting it `:ro` does **not** change this. That makes the socket *file*
read-only; it does not restrict the API calls made through it. The `:ro` in the
examples below is defence in depth, not a permission boundary.

muninn only ever issues read calls. The socket has no way of knowing that.

**Recommended: a socket proxy.** Restrict the API surface to what the module
needs:

```yaml
services:
  docker-socket-proxy:
    image: tecnativa/docker-socket-proxy
    environment:
      CONTAINERS: 1      # /containers/json, /containers/*/stats
      INFO: 1            # /info
      VERSION: 1         # /version
      POST: 0            # no write operations at all
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
    # deliberately not published to the host
    networks: [monitoring]

  muninn:
    # ...
    networks: [monitoring]
    # and in muninn.yaml:
    #   modules.docker.endpoint: "tcp://docker-socket-proxy:2375"
```

This turns "root on the host" into four read-only endpoints. It costs one
container. See [ADR-0010](adr/0010-docker-socket.md).

**Notes.** `container_state_include = ["running"]` means stopped containers are
not reported. A container that exits disappears from the metrics rather than
reporting zeros.

Enabling the module with an unreachable endpoint is a **startup failure**, not an
empty metric set — a silent Docker module looks exactly like a host with no
containers.

**Limits.** Container-level metrics only. What runs *inside* a container is not
visible here.

---

## updates

Pending package updates on the host. Off by default; implementation lands in
WP10.

```yaml
modules:
  updates:
    enabled: false
    interval: 1h
    security_only_metric: true
```

| Option | Type | Default | Effect |
|---|---|---|---|
| `interval` | duration | `1h` | Own schedule — package state changes slowly and the check is comparatively expensive |
| `security_only_metric` | boolean | `true` | Report the security subset alongside the total |

**Renders to** `[[inputs.exec]]` running muninn's update helper with
`data_format = "influx"`. Telegraf has no package input plugin — all 249 in
1.39.2 were checked — so this is the only route available.

### How it reads the host, and why that took a spike

`apt` inside a container reads the *container's* package database. A naive
implementation reports the updates pending for debian-slim: not a crash, a
number, and a believable one. The [WP1 spike](spikes/updates-spike.md) settled
the approach with measurements before any of it was written.

What it does: mounts the host's apt and dpkg state read-only, points apt's
directory options at it, and runs `apt-get -s dist-upgrade`. Real apt does the
resolution — which is the point, because it honours holds, pins and phased
updates that a hand-rolled version comparison would not.

Measured against each host's own answer:

| Host | Host says | muninn says |
|---|---|---|
| debian:12 | 41 / 3 | **41 / 3** |
| debian:13 | 39 / 2 | **39 / 2** |
| ubuntu:22.04 | 50 / 40 | **50 / 40** |
| ubuntu:24.04 | 66 / 34 | **66 / 34** |

Including from a container running a *different* distribution than the host,
which is the normal case rather than the exotic one.

**Requires** the host mount (`/:/hostfs:ro` plus `runtime.host_mount_prefix`) —
the same mount CPU, memory and disk already need. No extra capabilities, no root,
no writes: the host tree is byte-identical after a check.

**Consequence for the image.** This needs real `apt` and `dpkg` in the runtime
image, so the base is debian-slim rather than distroless. That trade, with its
measurements, is in [`hardening.md`](hardening.md) and
[ADR-0009](adr/0009-updates-module-approach.md).

### The invariant

```text
muninn_updates_pending{severity="all"}        gauge
muninn_updates_pending{severity="security"}   gauge
muninn_updates_check_success                  gauge  0|1
muninn_updates_check_timestamp_seconds        gauge
muninn_updates_lists_age_seconds              gauge
```

**A failed check emits `check_success=0` and omits the pending counts.** It never
emits zero. "No updates pending" and "I could not look" are opposite conclusions,
and an alert rule cannot tell them apart afterwards if they share a
representation.

This is demonstrated rather than asserted: a missing mount, an empty dpkg status
and a structurally corrupt one each produce `check_success=0` with a specific
`reason` tag and no counts. Those are the spike's T8, T9 and T9b cells.

A failure here degrades muninn rather than stopping it: the configuration is
valid and everything else keeps collecting. The failure stays visible in the
logs, in `/status` and in `check_success`.

---

## Related

- [`configuration.md`](configuration.md) — the full key reference
- [`host-mounts.md`](host-mounts.md) — what to mount, and what each module needs
- [`telegraf-rendering.md`](telegraf-rendering.md) — how these fragments are produced
