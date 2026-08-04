# Module reference

One section per module: what it produces, which Telegraf plugin it renders to,
what it needs from the host, and where it stops being trustworthy.

All modules are **off by default**. `enabled: true` is the only thing every
module has in common.

Verified against **Telegraf 1.39.2**. Every plugin option named here exists in
that version's `sample.conf`, and `scripts/verify-design-package.sh` checks that
mechanically on every pipeline run.

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
| [updates](#updates) | `inputs.exec` | off | host `/var`, `/etc`, `/usr`; Debian or Ubuntu |
| [image_updates](#image_updates) | `inputs.exec` | off | **Docker socket** |

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
    container_states: [running]
    timeout: 5s
```

| Option | Type | Default | Renders to |
|---|---|---|---|
| `endpoint` | URL | `unix:///var/run/docker.sock` | `endpoint` |
| `container_include` | list of globs | `[]` | `container_name_include` |
| `container_exclude` | list of globs | `[]` | `container_name_exclude` |
| `container_states` | list | `[running]` | `container_state_include` |
| `timeout` | duration | `5s` | `timeout` |

`timeout` is also how long muninn waits when it probes the endpoint at startup —
see below.

`container_states` accepts Telegraf's own vocabulary: `created`, `restarting`,
`running`, `removing`, `paused`, `exited`, `dead`. An unknown value is rejected
by muninn rather than passed through, because Telegraf accepts it silently and it
then matches no container — a typo would produce a module that runs and reports
nothing.

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

**If you mount the socket directly anyway**, mounting it is not enough. muninn
runs as uid 10001 and the socket is owned by `root:docker`, so the process has to
be in the socket's group:

```yaml
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
    group_add:
      - "999"            # the docker group's GID: `stat -c '%g' /var/run/docker.sock`
```

The GID is host-specific, which makes the compose file host-specific — one more
reason the proxy below is the recommendation. Without it muninn refuses to start
with a permission error on the socket, which is at least the honest failure.

**Recommended: a socket proxy.** Restrict the API surface to what the module
needs, and skip the group entirely:

```yaml
services:
  docker-socket-proxy:
    image: tecnativa/docker-socket-proxy
    environment:
      CONTAINERS: 1      # /containers/json, /containers/*/stats
      INFO: 1            # /info
      VERSION: 1         # /version
      PING: 1            # /_ping — muninn's startup reachability check
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
container.

A complete, working version of this is shipped as
[`docker-compose.docker-module.yml`](../docker-compose.docker-module.yml), and
`scripts/container-test.sh` runs muninn against a real proxy and asserts that
container metrics arrive — the recommended deployment is tested, not just
recommended.

`POST: 0` is what makes it a boundary rather than a suggestion: without it the
proxy forwards container creation, and the whole exercise buys nothing. See
[ADR-0010](adr/0010-docker-socket.md).

### Running only, by default

`container_states: [running]` means a container that exits disappears from the
metrics rather than reporting zeros. That is deliberate: zeros are
indistinguishable from an idle container, absence is not.

If the event you want to alert on is *a container that stopped*, it has to still
be reported — add `exited`:

```yaml
    container_states: [running, exited]
```

Watch the cost. Every container that ever exited stays in the metrics until it is
removed, so on a host that runs short-lived containers this grows without bound.
Pair it with `container_exclude`, or prune.

### An unreachable endpoint refuses the start

Enabling the module with an endpoint that does not answer is a **startup
failure** (exit `12`), not an empty metric set. This is the module's most
important behaviour, and the reason is worth stating plainly: a Docker module
collecting nothing looks exactly like a host running no containers. Nothing in a
dashboard distinguishes them, so the difference is settled before start.

At startup — and in `muninn check-runtime` — muninn issues one `GET /_ping`
against the configured endpoint and requires a `200`. That is stricter than
opening a connection, and the difference is the proxy case: a proxy that is
running with `PING: 0` accepts the connection and denies the call. Only the
request sees it.

What each failure means:

| Symptom | Cause | Fix |
|---|---|---|
| `cannot connect to '/var/run/docker.sock'` | not mounted, or the daemon is not running | mount the live socket |
| `cannot resolve 'docker-socket-proxy:2375'` | the two containers do not share a network | put them on one |
| `answered 'HTTP/1.1 403 Forbidden'` | the proxy denies the call | allow `PING`, `CONTAINERS`, `INFO` |
| `accepted the connection and closed it` | something else holds that port | check the port |

**Limits.** Container-level metrics only. What runs *inside* a container is not
visible here.

---

## updates

Pending package updates on the host. Off by default, because it needs the host
mount and because most operators want to decide for themselves whether their
monitoring agent reads their package state.

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

**Renders to** `[[inputs.exec]]` with `data_format = "influx"`, running

```toml
commands = [["/usr/local/bin/muninn", "update-check"]]
environment = ["HOSTFS=/hostfs", "TMPDIR=/run/muninn"]
```

Telegraf has no package input plugin — all 249 in 1.39.2 were checked — so
`exec` is the only route available, and what it executes is muninn itself. There
is no separate helper binary to keep in step, and the same command is available
to an operator:

```bash
docker exec muninn muninn update-check --hostfs /hostfs
muninn_updates,status=ok,reason=none check_success=1i,check_timestamp_seconds=1754225000i,lists_age_seconds=4210i
muninn_updates,severity=all pending=41i
muninn_updates,severity=security pending=3i
```

Running it by hand is the fastest way to diagnose a count that looks wrong: it
prints the same line Telegraf parses, and puts the detail behind the `reason` tag
— the path, or apt's own error — on stderr.

`TMPDIR` points at the runtime directory rather than `/tmp`, because apt writes
its cache even when it is only simulating, and in the documented deployment the
root filesystem is read-only with exactly one writable tmpfs.

### How it reads the host, and why that took a spike

`apt` inside a container reads the *container's* package database. A naive
implementation reports the updates pending for debian-slim: not a crash, a
number, and a believable one. The [measured evidence](updates-evidence.md) settled
the approach before any of it was written.

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
| ubuntu:24.04 | 66 / 0 | **66 / 0** |

Including from a container running a *different* distribution than the host,
which is the normal case rather than the exotic one.

The Ubuntu 24.04 zero is not an error and not a disagreement: the host's own apt
says zero too, because the candidate versions now resolve through
`noble-updates`. [The security subset is a lower bound on Ubuntu](#the-security-subset-is-a-lower-bound-on-ubuntu)
explains why, and why the total is unaffected.

**Requires** the host mount (`/:/hostfs:ro` plus `runtime.host_mount_prefix`) —
the same mount CPU, memory and disk already need. No extra capabilities, no root,
no writes: the host tree is byte-identical after a check.

**Consequence for the image.** This needs real `apt` and `dpkg` in the runtime
image, so the base is debian-slim rather than distroless. That trade, with its
measurements, is in [`hardening.md`](hardening.md) and
[ADR-0009](adr/0009-updates-module-approach.md).

### The invariant

```text
muninn_updates_pending{severity="all"}          gauge   only on success
muninn_updates_pending{severity="security"}     gauge   only on success, unless switched off
muninn_updates_check_success{status,reason}     gauge   0|1
muninn_updates_check_timestamp_seconds{...}     gauge
muninn_updates_lists_age_seconds{...}           gauge   only on success
```

**A failed check emits `check_success=0` and omits the pending counts.** It never
emits zero. "No updates pending" and "I could not look" are opposite conclusions,
and an alert rule cannot tell them apart afterwards if they share a
representation.

This is demonstrated rather than asserted: a missing mount, an empty dpkg status
and a structurally corrupt one each produce `check_success=0` with a specific
`reason` tag and no counts — cells S8, S9 and S9b of
`scripts/updates-test.sh`, which runs the shipped image against real host trees.

The one alert worth writing from this:

```promql
muninn_updates_check_success == 0
```

Not `absent(muninn_updates_pending)`, which also fires while the agent is
starting.

### The security subset is a lower bound on Ubuntu

An update counts as security when the origin apt prints for the candidate version
names a `-security` suite — `Debian-Security:12/stable-security`,
`Ubuntu:22.04/jammy-security`.

Ubuntu publishes security updates to `<release>-security` **and** copies them into
`<release>-updates`. When apt resolves the candidate through the latter, the line
reads `Ubuntu:24.04/noble-updates` and muninn does not count it as security. The
same fixture measured a year apart shows it plainly: Ubuntu 24.04 reported 66
pending / 34 security when first measured, and 66 pending / **0** security when
rebuilt against today's archive. Same packages, different pocket.

The host's own `apt-get -s dist-upgrade` says exactly the same thing, so muninn is
not diverging from the machine it describes. But it does mean:

- **Alert on the total.** `muninn_updates_pending{severity="all"}` is exact.
- **Read the security series as "at least this many".** On an Ubuntu host, zero is
  not evidence that nothing security-relevant is pending.

Tracked as [R8](risks.md), with what a more thorough classification would cost.

### What a `reason` means

`reason` is a closed set of tokens, so it is safe as a label. The detail — the
path, or apt's own message — is on stderr, which Telegraf logs.

| `reason` | Cause | Fix |
|---|---|---|
| `hostfs_not_mounted` | no host mount | `-v /:/hostfs:ro`, and `runtime.host_mount_prefix: /hostfs` |
| `dpkg_status_unreadable` | `/hostfs/var/lib/dpkg/status` missing or unopenable | mount the whole root, not a subset |
| `dpkg_status_empty` | the package database is empty | check what is actually mounted at `/hostfs` |
| `apt_etc_missing`, `apt_lists_missing` | `/etc/apt` or `/var/lib/apt/lists` absent | same |
| `apt_lists_empty` | no package index — `apt-get update` has never run on the host | run it on the host |
| `os_release_unreadable` | neither `/etc/os-release` nor `/usr/lib/os-release` can be read — the first is normally a symlink into the second | mount the whole root |
| `host_not_debian_family` | the host is not Debian or Ubuntu | disable the module |
| `scratch_unavailable` | nowhere writable for apt's cache | give the container its tmpfs |
| `apt_failed` | apt refused — usually a host index format the image's apt does not understand | see stderr; report it |
| `parse_inconsistent` | more security updates than updates in total | a bug; please report it |

### A failed check degrades muninn — it does not stop it

The Docker module refuses the start when its endpoint does not answer. This one
does the opposite, and the difference is the point: an unreachable Docker
endpoint produces *silence*, which reads as "no containers", while a failed
update check produces `check_success=0` with a reason. Nothing is being
misrepresented, so taking a working agent out of service — and losing CPU,
memory, disk and network collection with it — would cost far more than it
protects.

muninn runs the check once at startup, so the answer is in the logs, in `/status`
and in `muninn_module_check_success{module="updates"}` within seconds instead of
after the first hourly interval:

```text
muninn_module_check_success{module="updates"} 0
```

and `/status` reports `degraded` — ready, serving, one module down.

**Preconditions are the exception, and they are checked earlier.** A host tree
that is not mounted at all, or a host that is not Debian-family, is not a failed
check — it is a deployment that cannot support the module, and muninn refuses to
start with exit `12` naming the module, exactly as it does for every other
module's requirements. `muninn check-runtime` reports the same thing without
starting anything. The rule in one line:

| | |
|---|---|
| The deployment cannot support the module | exit `12` before anything starts |
| The deployment is right and the check still failed | `degraded`, `check_success=0`, keep collecting |

The second is the interesting case, and it is why the metric exists: apt refusing,
an index format the image does not understand, a package database that is present
but unreadable. Those cannot be known before start, and none of them is a reason
to stop reporting CPU.

---

## image_updates

Whether a newer image is available, under the tag it is running, for each
running container. Off by default, for the same reason `docker` is: it needs
the Docker socket, which is root-equivalent access to the host, and most
operators want to decide that for themselves.

```yaml
modules:
  image_updates:
    enabled: false
    endpoint: "unix:///var/run/docker.sock"
    timeout: 5s
    interval: 1h
    container_include: []
    container_exclude: []
```

| Option | Type | Default | Effect |
|---|---|---|---|
| `endpoint` | URL | `unix:///var/run/docker.sock` | Same meaning as `modules.docker.endpoint` — see [docker](#docker) |
| `timeout` | duration | `5s` | Per Docker API call, and for the startup reachability probe |
| `interval` | duration | `1h` | Own schedule — see below |
| `container_include` | list of globs | `[]` | Container names to check (allow-list) |
| `container_exclude` | list of globs | `[]` | Container names to skip |

**Renders to** `[[inputs.exec]]` with `data_format = "influx"`, running

```toml
commands = [["/usr/local/bin/muninn", "image-check", "--endpoint", "unix:///var/run/docker.sock", "--timeout-secs", "5"]]
```

Telegraf has no plugin for this either — see [updates](#updates) for the same
fact about package updates — so `exec` runs muninn again, the same pattern:

```bash
docker exec muninn muninn image-check --endpoint unix:///var/run/docker.sock
muninn_image_updates,status=ok,reason=none check_success=1i,check_timestamp_seconds=1754225000i,containers_checked=3i
muninn_container_image_updates,container_name=web,image=nginx:latest,status=ok,reason=none check_success=1i,check_timestamp_seconds=1754225000i
muninn_container_image_updates,container_name=web,image=nginx:latest update_available=0i
```

### Why the daemon answers, not muninn

Every registry speaks HTTPS, and muninn has never been a TLS client — see the
note next to `openssl` in `deny.toml`. Rather than adding a TLS stack for one
module, this module asks the *Docker daemon* to resolve the tag against the
registry (`GET /distribution/{name}/json`), the same way `docker pull` would,
without pulling. The daemon does the TLS handshake, in its own process, with
whatever registry credentials the host is already configured with. muninn stays
a plaintext HTTP client talking to the same socket, or the same proxy, the
`docker` module already reaches. The full reasoning, including what was
rejected, is [ADR-0013](adr/0013-image-updates-via-docker-api.md).

**This is the one place muninn is a Docker client for more than a reachability
check.** Three read-only calls per check — list containers, inspect one image,
resolve one tag — nothing else. The security posture is unchanged from
[docker](#docker): the same socket, the same root-equivalent exposure, the same
recommendation to use a proxy, whose allowlist grows by two endpoints:

```yaml
services:
  docker-socket-proxy:
    environment:
      CONTAINERS: 1
      IMAGES: 1          # /images/*/json
      DISTRIBUTION: 1    # /distribution/*/json
      PING: 1
      POST: 0
```

### The invariant

```text
muninn_image_updates_check_success{status,reason}          gauge  0|1
muninn_image_updates_check_timestamp_seconds{...}          gauge
muninn_image_updates_containers_checked{...}               gauge  only on success
muninn_container_image_updates_check_success{
  container_name,image,status,reason}                      gauge  0|1
muninn_container_image_updates_check_timestamp_seconds{...} gauge
muninn_container_image_updates_update_available{
  container_name,image}                                    gauge  0|1, only on success
```

**A failed check emits `check_success=0` and omits the verdict.** It never
reports `update_available` for a container it could not judge — the same rule
[updates](#updates) stands on, applied per container instead of once per host.
A container whose image was built locally, whose registry is unreachable, or
whose reference is already pinned to a digest does not report "up to date"; it
reports why it could not say, and that does not affect any other container's
verdict.

Two alerts worth writing from this:

```promql
muninn_image_updates_check_success == 0
muninn_container_image_updates_update_available == 1
```

Not `absent(muninn_container_image_updates_update_available{container_name="x"})`
for "container x has no verdict" — that is also true while the container is
starting, or is not enabled to be checked at all.

### What a `reason` means

The daemon-level check (`muninn_image_updates`):

| `reason` | Cause | Fix |
|---|---|---|
| `invalid_endpoint` | `--endpoint` was not `unix://...` or `tcp://...` | only reachable running `image-check` by hand; the rendered form is always valid |
| `docker_unreachable` | the daemon (or proxy) at `endpoint` did not answer | same fixes as an unreachable [docker](#docker) endpoint |

Per container (`muninn_container_image_updates`):

| `reason` | Cause | Fix |
|---|---|---|
| `digest_pinned_reference` | the container's image is already `repo@sha256:...` | nothing to fix — there is no tag for a newer image to appear under |
| `no_repo_digest` | the image was never pulled from, or pushed to, a registry | expected for a locally built image; nothing to fix |
| `no_matching_repo_digest` | the daemon recorded digests, but none for this repository | usually a `docker tag` onto an image pulled under a different name |
| `image_inspect_failed` | `GET /images/{id}/json` failed | see stderr; the image may have been removed since the container started |
| `distribution_query_failed` | the daemon could not resolve the tag against the registry | unreachable registry, a tag that no longer exists, or authentication the daemon does not have |

### Cost scales with the container count

Each container costs one or two Docker API round trips, run sequentially
rather than in parallel — the container counts this module is verified
against are small enough that the added complexity of parallelising them was
not worth it. The rendered `inputs.exec` `timeout` is a fixed `120s` rather
than derived from a count muninn cannot know at render time. A host with more
containers than that comfortably covers in `modules.image_updates.timeout` ×
2 × container-count should narrow `container_include`/`container_exclude`
rather than assume the timeout will grow to match.

**Registries rate-limit anonymous callers.** Docker Hub allows 100 anonymous
pulls per 6 hours per IP, and a manifest lookup counts against it — one reason
`interval` defaults to `1h`, like [updates](#updates), and why validation
refuses anything under a minute.

**Only public images are verified.** A private registry the *host* is already
configured to pull from should work through the daemon's own credentials with
no change to muninn, but that path has not been measured the way
[the updates module's evidence](updates-evidence.md) measured Debian and
Ubuntu hosts. See [ADR-0013](adr/0013-image-updates-via-docker-api.md) and
[`docs/roadmap.md`](roadmap.md).

**Limits.** Running containers only, matching the question this module
answers — a stopped container's image is not being served by anything.
Multi-architecture manifest lists are compared whole, which is what the
daemon itself compares on a `docker pull`.

---

## Related

- [`configuration.md`](configuration.md) — the full key reference
- [`host-mounts.md`](host-mounts.md) — what to mount, and what each module needs
- [`telegraf-rendering.md`](telegraf-rendering.md) — how these fragments are produced
