# ADR-0013 — Detect image updates through the Docker Engine API, not a registry client

**Status:** accepted · **Date:** 2026-08-04

## Context

The image_updates module reports, per running container, whether the registry
now serves a different image under the tag the container was started with —
the same question `docker pull` answers by comparing digests, asked without
pulling anything.

**Telegraf has no plugin for this**, the same fact ADR-0009 records for OS
package updates: whatever muninn does, the result reaches Telegraf through
`inputs.exec`.

**Answering it needs a network call to a registry**, and every registry speaks
HTTPS. muninn has never been an HTTPS client. `deny.toml` bans `openssl` and
`openssl-sys` outright, with a comment explaining why the licence gate does not
need to list OpenSSL's licence at all: "muninn adds no TLS stack of its own —
the Telegraf binary is a separate process with its own Go TLS stack, and
muninn's own HTTP surface is plaintext on a health port." `muninn/src/probe.rs`
states the same boundary from the other side: "this is deliberately not a
Docker client... muninn never talks to the API again — Telegraf does."

Adding a registry client — Docker Hub's and GHCR's bearer-token exchange, an
OCI Distribution Specification manifest fetch, TLS certificate verification —
would be the first TLS stack in muninn's own process, pulled in as a dependency
for one module.

## Decision

**Ask the Docker daemon to ask the registry.** `GET /distribution/{name}/json`
is a documented Docker Engine API endpoint (since API 1.30 / Docker 17.06) that
has the *daemon* resolve a reference against its registry and return the
manifest digest — the same resolution `docker pull` performs, without pulling.

```text
GET /containers/json?filters={"status":["running"]}
  → running containers: Names, Image (the reference), ImageID (the local
    content ID)
GET /images/{ImageID}/json
  → RepoDigests: what the daemon recorded when this exact image was last
    pulled from, or pushed to, a registry
GET /distribution/{repo}:{tag}/json
  → Descriptor.digest: what the registry serves for that tag right now
```

A different digest under `RepoDigests` for the container's repository than
`Descriptor.digest` means the tag moved since this container started.

muninn stays a plaintext HTTP client talking to the same socket, or the same
proxy, the `docker` module already reaches — `deny.toml`'s note stays true. The
daemon does the TLS handshake, in its own process, with its own already-audited
Go stack, and — the incidental benefit — with whatever registry credentials the
host is already configured with, so a private registry the host can already
pull from works with no credential handling added to muninn at all. An
anonymous lookup, which is what happens for a public image, is the only case
this module is verified against.

## Implementation

**muninn does become a Docker API client for real**, not only the one-shot
`/_ping` `probe.rs` sends at startup. That contradicts the "muninn never talks
to the API again" line quoted above, and does so on purpose: this module makes
three calls, on the module's own interval, for the same reason the updates
module runs `apt-get` on its own interval rather than through a Telegraf plugin
— there is no Telegraf plugin, and this cost is more honest carried by muninn
than by inventing one for a case that would go untested.

**`serde_json` is a new dependency**, in `muninn-modules` only. Parsing three
small, fixed JSON response shapes by hand was considered and rejected: it is
exactly the kind of "plausible until the input isn't" code the hand-rolled
`inputs.exec` line protocol writers get away with only because they are
producing output, not consuming someone else's. `serde` is already load-bearing
throughout the tree; `serde_json` adds a name to `Cargo.toml` and no TLS, no
crypto, nothing `deny.toml`'s licence gate has not already seen elsewhere.

**The transport is hand-rolled, not `serde_json`.** Three calls, three response
shapes, one connection each — no keep-alive, no chunked transfer encoding, no
streaming endpoint. `Connection: close` on every request makes "read until EOF"
always correct, matching how `probe.rs`'s `/_ping` already uses `HTTP/1.0` to
force the same thing for a response with no body at all. A general HTTP client
crate was not needed for three fixed calls, and not adding one keeps this
module's only new dependency the one that parses what the three calls return.

**Every request path is checked for a control character or a space before it
is sent.** Two of the three calls build their path from strings the daemon
reported — an image reference, an image ID — and this is the one place in
muninn that writes a raw request line (`GET {path} HTTP/1.1\r\n...`) from
values it did not choose itself. Docker's own reference grammar cannot produce
`\r`, `\n` or a space in either field, so this guard should never fire against
a real daemon — but the guard does not rely on that holding. It is one check
at the single choke point every call passes through
(`docker_api::get`), catching request-line injection before it can reach
whatever is listening on the other end of the socket, rather than resting on
an upstream guarantee this module has no way to verify. Found during a
security review after the module shipped; a security review before shipping
code that hand-builds HTTP requests from external strings is the better
order, and the next one gets written that way.

**The comparison is `RepoDigests` against `Descriptor.digest`, keyed by
`ImageID`, not by the tag string.** Looking the image up by its currently
running `ImageID` — rather than re-resolving the tag through `/images/{name}`
— matters because the local tag can have moved since the container started
(someone re-pulled `myapp:latest` without recreating the container); comparing
against the *running* image's own recorded digest is what makes the verdict
about this container rather than about whatever the tag happens to point to on
the host right now.

**A digest-pinned reference (`repo@sha256:...`) has no verdict.** There is no
tag for a newer image to appear under, so it reports
`reason=digest_pinned_reference` rather than guessing.

**Reachability is two different questions, checked at two different times.**
Whether the *daemon* answers is a startup precondition — `Requirements` is
built exactly like the `docker` module's (`ADR-0010`), deriving a unix-socket
or TCP endpoint from `modules.image_updates.endpoint`, so `muninn check-runtime`
and the real startup path both refuse to start if it does not answer `GET
/_ping`. Whether the *registry* answers for one container's image is a
per-check outcome, and it degrades that one container's series rather than the
module — the same split `updates` draws between "the host isn't Debian" (a
precondition) and "apt failed this time" (`check_success=0`).

## Consequences

- **The invariant holds per container, not once per host.** `check_success=0`
  and no verdict, never a guess — the property ADR-0009 established, applied to
  N series instead of one. A container whose image was built locally, or whose
  registry cannot be reached, does not report "up to date"; it reports why it
  could not say, and every other container's verdict is unaffected.
- **This module's cost scales with the number of running containers**, at one
  or two Docker API round trips each, sequentially. The rendered `inputs.exec`
  `timeout` is a fixed, generous `120s` rather than derived from a container
  count muninn cannot know at render time; a host with more containers than
  that comfortably covers should narrow `container_include`/
  `container_exclude` rather than assume the timeout will grow to match. See
  `docs/modules.md#image_updates`.
- **Registries rate-limit anonymous callers.** Docker Hub allows 100 anonymous
  pulls per 6 hours per IP, and a manifest lookup counts against it. The
  module's default `interval` is 1 hour, matching `updates`, and validation
  refuses anything under a minute for the same reason `updates.interval` does.
- **Only public images are verified.** A private registry the *host* can
  already reach works through the daemon's own stored credentials with no
  change to muninn, but that path has not been measured the way ADR-0009's
  evidence measured the updates module against real hosts. Tracked in
  `docs/roadmap.md` if operational experience asks for it.
- **The module needs the same Docker socket exposure as `docker`** — root-
  equivalent on the host, `:ro` not a permission boundary — and inherits its
  whole security posture from ADR-0010: off by default, a socket proxy
  recommended, a reachability check before start rather than silent emptiness.
  The proxy allowlist grows by two calls: `CONTAINERS` (already needed for
  `docker`) plus `IMAGES` and `DISTRIBUTION`.

## Alternatives considered

**muninn as a registry client**, speaking the OCI Distribution Specification's
bearer-token flow directly to Docker Hub, GHCR, Quay and anything else an
operator points it at. Rejected: it is the first TLS stack in muninn's own
process, for one module, duplicating a resolution the daemon already performs
correctly and already has credentials for. It would also need its own
multi-registry auth handling — muninn's secrets model is file paths only, and
extending it to per-registry bearer tokens is a design question this module
does not need to force.

**Reading `/var/lib/docker` directly**, the way ADR-0010 already rejected for
container stats. The same reasons apply again, harder: no registry ever enters
that directory, so there is nothing there to compare against.

**Skip the daemon; require an external tool (`docker scout`, `regctl`,
skopeo) on the host, muninn only reads its output.** Rejected for the same
reason ADR-0009 rejected an external host helper for updates: it works, but it
is one more thing to install, version and keep working across the base images
this project does not control, for a call the daemon already exposes for free.
