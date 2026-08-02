# ADR-0010 — Docker socket security model

**Status:** accepted · **Date:** 2026-08-02

## Context

The Docker module reads per-container metrics through the Docker Engine API,
normally over `/var/run/docker.sock`.

Access to that socket is equivalent to root on the host. Anyone who can write to
it can start a container with the host filesystem mounted and `--privileged`.
There is no read-only mode: mounting the socket `:ro` prevents modifying the
socket *file*, not the API calls made through it. The `:ro` in every example
compose file — including muninn's — is defence in depth, not a permission
boundary.

muninn only ever issues read calls. The socket has no way of knowing that.

## Decision

1. **The module is disabled by default.** Enabling it is a deliberate act.
2. **muninn issues no write operations.** No container start, stop, exec or
   create. It reads container lists, info and stats, nothing else.
3. **A socket proxy is the recommended deployment**, restricting the API surface
   to the endpoints the module needs (`/containers/json`, `/containers/*/stats`,
   `/info`, `/version`). `docs/modules.md` carries a working configuration.
4. **A timeout is always set**, defaulting to 5s, so a wedged daemon slows
   collection rather than stopping it.
5. **Enabling the module without a reachable endpoint is a startup failure**, not
   an empty metric set. A Docker module that silently reports nothing looks
   exactly like a host with no containers.
6. **The security implications are documented where the decision is made** — in
   the annotated example config, next to `enabled: false` — not in an appendix
   nobody reaches.

## Consequences

- The common case costs nothing: a host with no containers, or an operator who
  does not want the exposure, never touches the socket.
- The proxy adds a container to the deployment. That is the price of turning
  "root on the host" into "four read-only endpoints", and it is worth it.
- If the socket is mounted directly, the exposure is real and is stated plainly
  rather than softened. An operator who accepts it should do so knowingly.
- muninn cannot offer container *control* features later without revisiting this
  ADR. That is intended: observation and actuation are different trust levels,
  and the roadmap keeps them apart.
- Group membership matters as much as the mount. A container in the `docker`
  group has the same access as one running as root with the socket mounted;
  hardening guidance covers both.

## Alternatives considered

**On by default, since it is useful and most hosts run Docker.** Rejected
outright. A monitoring agent that grants itself root-equivalent access unless
told otherwise inverts the default that matters.

**Read the container filesystem directly from `/hostfs/var/lib/docker` instead of
using the API.** Rejected: the on-disk layout is a private implementation detail
that changes between storage drivers and versions, live stats are not there at
all, and it would need root to read.

**Require the proxy — refuse a direct socket mount.** Considered seriously.
Rejected because muninn cannot reliably tell a proxy endpoint from the real
daemon, so the check would be advisory anyway, and a hard refusal would push
people toward disabling the safety rather than adopting the proxy. Strong
recommendation plus honest documentation is the better trade.
