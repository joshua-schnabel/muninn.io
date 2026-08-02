# Hardening

muninn mounts your host filesystem and may be given the Docker socket. That is
a larger blast radius than most containers, so the posture is stated explicitly
rather than assumed.

## Baseline

```yaml
services:
  muninn:
    image: ghcr.io/joshua-schnabel/muninn.io:0.1.0

    read_only: true                    # no writable layer
    security_opt: [no-new-privileges:true]
    cap_drop: [ALL]                    # no capabilities at all

    tmpfs:
      - /run/muninn:mode=0700          # generated config, memory-backed

    volumes:
      - ./muninn.yaml:/etc/muninn/muninn.yaml:ro
      - /:/hostfs:ro
      - ./influxdb-token:/run/secrets/influxdb_token:ro
```

Never `--privileged`. Never `--pid=host`. Neither is required by any module in
the MVP, and if a future module needs one it will declare it per-module rather
than raising the baseline for everyone.

## The image

- **Multi-stage build.** The runtime stage carries the muninn binary, the pinned
  Telegraf binary, and nothing else.
- **Non-root**, with a read-only root filesystem.
- **Telegraf pinned by SHA-256** and verified during the build, per architecture.
  A mismatch fails the build. [ADR-0011](adr/0011-telegraf-pinning.md)
- Only the Telegraf **binary** is taken, not the upstream image's entrypoint
  scripts, default configuration or user setup — muninn manages all of that.
- **SBOM generated** per release; **Trivy** blocks on fixable CRITICAL/HIGH.

> **Open:** the runtime base image is decided by the WP1 spike. If reading host
> package state needs `apt` and `dpkg` in the image, the base is debian-slim
> rather than distroless — a real increase in attack surface, which is precisely
> why that decision is made before the Dockerfile is written rather than after.
> See [`risks.md#r1`](risks.md) and [`spikes/updates-spike.md`](spikes/updates-spike.md).

## Secrets

- **File paths only.** No key anywhere accepts a secret value inline, and none
  reads one from the environment. A token in a config file ends up in your
  configuration management, your backups and every `docker inspect`; a path does
  not.
- **Redaction is a property of the type.** Secrets are wrapped in a type whose
  `Debug` and `Display` render `***`, with the value reachable only through an
  explicit accessor called in one place. `tracing::debug!(?config)` cannot leak a
  token — not by convention, by construction.
- **Errors name the path, never the contents.**
- **`muninn render-config` redacts by default.** Its output is safe to paste into
  an issue.
- **The generated config holds resolved values** and therefore lives on a tmpfs,
  root-only, never persisted, never mounted out.
  [ADR-0003](adr/0003-ephemeral-generated-config.md)

## The host mount

`/:/hostfs:ro` gives the container read access to the entire host filesystem,
including `/etc`, and therefore `/etc/shadow`.

This is real and is not softened. Weigh it honestly, though: `/proc` and `/sys`
alone — the paths that actually carry the metrics — already expose command lines,
environment variables and network state for every process on the host. The extra
reach of the full root mount is mostly `/home` and `/var/lib`.

Reasoning for the single mount rather than several:
[ADR-0005](adr/0005-hostfs-mount.md). It is not laziness — `HOST_MOUNT_PREFIX`
has to strip a common prefix from reported paths, and separate mounts have none,
which leaves every disk metric tagged with a path nobody recognises.

**The honest framing:** a monitoring agent for the host is inside the host's
trust boundary. It is not going to be less trusted than the host it reports on.
Keep the image minimal and pinned, run it non-root with no capabilities, and
treat it accordingly.

## The Docker socket

**Access to the Docker socket is equivalent to root on the host.** Anyone who can
write to it can start a container with the host filesystem mounted and
`--privileged`.

**Mounting it `:ro` does not change this.** That makes the socket *file*
read-only; it does not restrict the API calls made through it. The `:ro` in every
example — including muninn's — is defence in depth, not a permission boundary.

Therefore:

- the Docker module is **off by default**;
- muninn issues **only read calls** — no start, stop, exec or create;
- a **socket proxy is the recommended deployment**, restricting the API to
  `/containers/json`, `/containers/*/stats`, `/info` and `/version`. Working
  configuration in [`modules.md#docker`](modules.md#docker).

Group membership matters as much as the mount: a container in the `docker` group
has the same access as one running as root with the socket mounted.

[ADR-0010](adr/0010-docker-socket.md)

## Network exposure

| Port | Serves | Authentication |
|---|---|---|
| `9273` | Host metrics (Telegraf) | Optional basic auth |
| `8080` | Health, status, agent metrics (muninn) | None |

Neither is authenticated by default. Host metrics reveal a fair amount about a
machine — mounted filesystems, network interfaces, running process counts — and
`/status` reveals versions and enabled modules. Put both on a trusted network,
set basic auth on the Prometheus output, or both.

`/status` deliberately carries no secrets and no configuration dump.

**`insecure_skip_verify`** on the InfluxDB output disables certificate
verification entirely, so anyone able to intercept the connection can read your
metrics and inject fabricated ones. If a certificate does not validate, fix the
certificate or set `tls.ca_file`. muninn logs a prominent warning for as long as
it is true.

## Supply chain

- `Cargo.lock` committed; dependencies resolved reproducibly.
- **cargo-deny** gates advisories, licences, banned crates and registry sources.
- **OpenSSL is banned outright** and removed from the licence allow-list. muninn
  adds no TLS stack of its own, so an OpenSSL dependency appearing would be an
  accident worth failing the build over.
- **Semgrep** (`p/rust`, `p/secrets`) on every push and PR; ERROR blocks.
- **Trivy** on the built image; fixable CRITICAL/HIGH block.
- Telegraf pinned by checksum, verified at build time, and checked again at
  startup against the runtime binary's reported version.
- GitHub Actions pinned by commit SHA; least-privilege `permissions:` per job.

## Verifying a deployment

```bash
docker compose exec muninn muninn check-runtime
```

Checks mounts, permissions, secrets, port conflicts, the Docker socket, the host
OS and module support. Non-zero exit on any problem, with every problem listed
rather than stopping at the first.

## Reporting a vulnerability

See [`SECURITY.md`](SECURITY.md).
