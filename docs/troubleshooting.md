# Troubleshooting

Symptom, cause, fix. Exit codes are a public contract and are listed in full in
[`supervision.md`](supervision.md); this page covers what you are most likely to
hit and what it actually means.

Start here, every time:

```bash
docker logs muninn                       # muninn names the key that is wrong
curl -s localhost:8080/status | jq       # state, and which module is failing
curl -s localhost:8080/health/ready      # ready or not, and why
docker inspect muninn --format '{{.State.ExitCode}}'
```

## The container exits immediately with code 10 (CONFIG)

The YAML did not load or did not validate. muninn names the key.

- **`unknown field ...`** — every config struct carries
  `#[serde(deny_unknown_fields)]`, so a typo is an error rather than a silently
  ignored key. Check spelling and nesting against
  [`configuration.md`](configuration.md).
- **`invalid type: integer`** on a duration — durations are `30s`, `5m`, `1h`,
  never bare numbers.
- **`unsupported schema version`** — the version is checked *first*, on purpose:
  an unknown schema has to be reported as an unknown schema, not as forty
  complaints about keys that moved.
- **no output at all** — the file is not where muninn is looking. Default is
  `/etc/muninn/muninn.yaml`; the mount is `./muninn.yaml:/etc/muninn/muninn.yaml:ro`.

Validate without starting anything:

```bash
docker run --rm -v ./muninn.yaml:/etc/muninn/muninn.yaml:ro \
  jschnabel/muninn:<version> validate
```

## Exit code 11 (SECRET)

A secret file could not be read, or was empty. Secrets are **file paths only** —
never inline values, never environment variables — and the handling is
fail-closed: missing, unreadable or empty stops startup rather than proceeding
without credentials.

The error names the **path**, never the contents. Check that the file is mounted,
that it is readable by the non-root user muninn runs as, and that it is not
zero bytes.

## Exit code 12 (RUNTIME)

A module's preconditions are not met — this is a deployment to fix, not a
degradation.

- **updates module:** needs the host filesystem mounted (`/:/hostfs:ro`) and a
  Debian-family host. [`host-mounts.md`](host-mounts.md)
- **docker / image_updates:** needs the Docker socket, and it must answer.

## Exit code 20 (TELEGRAF_CONFIG)

Telegraf itself rejected the configuration muninn generated. This is a bug in
muninn or an unmodelled combination — the whole point of running
`telegraf config check` before starting is that you learn it here rather than
from missing metrics.

See what was generated (secrets are redacted):

```bash
docker run --rm -v ./muninn.yaml:/etc/muninn/muninn.yaml:ro \
  jschnabel/muninn:<version> render-config
```

The generated file itself lives on a tmpfs, is root-only and is never persisted,
because it holds resolved secret values —
[ADR-0003](adr/0003-ephemeral-generated-config.md). `render-config` is the
supported way to inspect it.

## Exit code 22 (TELEGRAF_EXITED) — the container keeps restarting

Telegraf died, and muninn exited rather than restarting it internally. That is
deliberate: a container reporting healthy while Telegraf crash-loops invisibly
inside it is the worst failure this project can have.
[ADR-0002](adr/0002-supervisor-no-restart-loop.md)

The restart count is the signal, and Telegraf's own error is in the logs above
muninn's exit line. Fix that; the orchestrator's back-off is doing its job in the
meantime.

## Host metrics show the container, not the host

Plausible numbers, no error, and completely wrong — the failure this project
exists to prevent. Almost always a missing or partial host mount.

`/:/hostfs:ro` and nothing narrower. [`host-mounts.md`](host-mounts.md) explains
why one mount rather than individual paths
([ADR-0005](adr/0005-hostfs-mount.md)), and what accepting it means — it includes
`/etc/shadow`.

## A fresh time series after every recreate

The hostname changed. Docker assigns a new container ID as the hostname on every
recreate, and every metric is tagged with it.

Set `hostname:` in the compose file, or `agent.hostname` in the YAML. It is one
of the four details [`host-mounts.md`](host-mounts.md) singles out as easy to get
wrong and slow to debug.

## `muninn_telegraf_running 0`, or nothing on `:9273`

Two endpoints, two purposes — and this is why they are separate.

| Port | Serves | Served by |
|---|---|---|
| `9273/metrics` | host metrics | Telegraf |
| `8080/metrics` | agent metrics: is Telegraf running, how long generation took | muninn |

If `:9273` answers nothing, read `:8080/metrics`. `muninn_telegraf_running 0` is
only useful while Telegraf is down, which is exactly when Telegraf's own endpoint
is gone. [ADR-0012](adr/0012-self-metrics-on-health-server.md)

If **neither** answers from outside the container, the ports are not published —
`9273:9273` and `8080:8080`.

## `docker_unreachable`

The Docker module cannot talk to the daemon.

- The socket is not mounted, or the daemon is not running.
- A socket proxy is in front of it and does not expose the endpoints the module
  needs.

Mounting the socket `:ro` does **not** make it read-only in any meaningful sense:
it protects the socket file, not the API, and the Docker socket is
root-equivalent. The module is off by default and a proxy is the recommended
deployment. [ADR-0010](adr/0010-docker-socket.md),
[`modules.md#docker`](modules.md#docker)

## `distribution_query_failed` on image_updates

The module asks the daemon to query the registry, using the daemon's own stored
credentials. This reason token covers everything that can go wrong there: rate
limits, a registry that is down, and an expired or absent credential for a
private image. They are not split apart today — [R9](risks.md).

**It also takes the module down, and the container with it.** One container
without a verdict now holds `muninn_module_check_success{module="image_updates"}`
at 0 and muninn at `degraded`, because the aggregate means "every selected
container was answered for" rather than "the daemon replied". That is intended
and is the honest report — but if a single unreachable registry is a thing you
have looked at and decided not to care about, say so with
`modules.image_updates.container_exclude`. An excluded container is not
selected, so it cannot hold the aggregate down.
[`modules.md#image_updates`](modules.md#image_updates), F-11.

Telegraf's own `muninn_image_updates_check_success` is unaffected: it answers
whether the daemon could be reached, and it still does.

The module is verified against public images only.
[ADR-0013](adr/0013-image-updates-via-docker-api.md)

## `updates` reports 0 security updates on Ubuntu

Now good news, and it did not always mean that. Classification asks whether the
candidate version is available from **any** security origin, so a security
update Ubuntu published to `<release>-security` and also copied into
`<release>-updates` is counted either way. Zero means zero.

Until that changed, muninn read only the one origin apt prints for the candidate
— whichever pocket it happened to resolve through — so an Ubuntu host with
security updates pending could report zero. Worth knowing if you are reading a
dashboard whose history crosses the change: the security series can step up
without anything on the host having moved. [R8](risks.md),
[ADR-0009](adr/0009-updates-module-approach.md)

## `updates` stops reporting a total it used to report

Deliberate. The security subset is a second `apt-cache policy` pass, and if that
pass fails the **whole check** fails — `muninn_updates_check_success` goes to 0
with apt's own `reason`, rather than a total appearing beside a security count
that might be wrong. A missing series reads as zero on most dashboards, which is
the failure this rule exists to prevent.

So a check that went from answering to `apt_failed` or `apt_timed_out` after
this change is usually the second pass, not the first. `muninn update-check`
prints the detail on stderr. Setting `modules.updates.security_only_metric` to
`false` skips the pass entirely — the total comes back, and the security series
legitimately goes away with it.

## A module reports failure but the container stays ready

Intended. `Degraded` is ready, narrowly: it is only reachable while Telegraf is
running and collecting. Taking a container out of service — and stopping CPU,
memory, disk and network collection that works perfectly — because it could not
count pending packages is worse than the degradation.

The failing module is visible in the logs, in `/status`, and in its own
`*_check_success` metric. Anything that stops collection is `Failed`, not
`Degraded`. [`architecture.md`](architecture.md#why-degraded-is-ready)

## Metrics stop during a deploy

`stop_grace_period`. Docker's default is 10 seconds and muninn's default
`runtime.shutdown_grace_period` is 20, so Docker kills the container mid-flush.
Set `stop_grace_period: 30s` in compose.

What the grace period covers is a *write*, not a collection cycle: Telegraf
flushes immediately on shutdown rather than waiting for the next tick, so the
bound that matters is one write attempt, not `agent.flush_interval`.

## Verbose logging

```yaml
logging:
  level: debug
  format: json    # structured, for a log pipeline
```

`RUST_LOG` works too and takes precedence for finer control:

```bash
docker run -e RUST_LOG=muninn=trace,muninn_telegraf=debug ...
```

## Related

- [`supervision.md`](supervision.md) — every exit code and what it means
- [`configuration.md`](configuration.md) — every key and its default
- [`host-mounts.md`](host-mounts.md) — the four compose details that are easy to get wrong
- [`risks.md`](risks.md) — known limits that look like bugs
