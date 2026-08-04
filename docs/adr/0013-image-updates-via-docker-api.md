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
Reading to EOF is capped at four megabytes: the socket is root-equivalent so
this is not a trust boundary, but an unbounded `read_to_end` does not belong in
a client that already refuses chunked encoding for the same class of reason.

**Transport and parsing are separate functions.** Each response shape is parsed
by a free function over `&[u8]`, and the three calls sit behind a `DockerApi`
trait. Both exist for testing: without them the only way to reach the verdict
logic — which is the part of this module worth testing hardest — is a live
daemon holding containers in states that are awkward to arrange on purpose. The
trait is the seam a scripted fake plugs into.

**Every request path is checked for a control character or a space before it
is sent.** Two of the three calls build their path from strings the daemon
reported — an image reference, an image ID — and this is the one place in
muninn that writes a raw request line (`GET {path} HTTP/1.1\r\n...`) from
values it did not choose itself. Docker's own reference grammar cannot produce
`\r`, `\n` or a space in either field, so this guard should never fire against
a real daemon — but the guard does not rely on that holding. It is one check
at the single choke point every call passes through (`docker_api::get`),
catching request-line injection before it can reach whatever is listening on
the other end of the socket, rather than resting on an upstream guarantee this
module has no way to verify.

The same argument applies a second time, in the other direction: the container
name and image reference the daemon reports are also written into influx line
protocol, where a newline ends the line and everything after it parses as
another measurement. Control characters there are **replaced**, not escaped —
line protocol has no escape for them — so a value muninn did not choose cannot
fabricate a metric series. One guard without the other would have been half an
argument.

Both were found in a security review of this module, before its first release
and before this ADR was merged. Reviewing code that hand-builds a wire format
from strings it did not choose, *while* writing it, is the better order; that
is what this ADR now records for the next module that does anything similar.

**The comparison is `RepoDigests` against `Descriptor.digest`, keyed by
`ImageID`, not by the tag string.** Looking the image up by its currently
running `ImageID` — rather than re-resolving the tag through `/images/{name}`
— matters because the local tag can have moved since the container started
(someone re-pulled `myapp:latest` without recreating the container); comparing
against the *running* image's own recorded digest is what makes the verdict
about this container rather than about whatever the tag happens to point to on
the host right now.

**Repository names are normalised before they are compared.** The daemon
spells one repository two ways: a container created as
`docker.io/library/nginx:latest` reports exactly that under `Image`, while the
image it runs records the *familiar* `nginx@sha256:...` under `RepoDigests`.
Compared literally those do not match, and an entirely ordinary container
reports `no_matching_repo_digest` — the invariant holds, but the module says
"cannot judge" about something it can judge perfectly well. Both sides go
through Docker's own normalisation rule: a first component containing `.` or
`:`, or equal to `localhost`, is a registry host; anything else is Docker Hub,
where a single-component name lives under `library/`.

**A digest-pinned reference (`repo@sha256:...`) has no verdict**, and neither
does an image *ID* where a reference belongs — which is what `docker ps` shows
once the last tag for a running container's image is removed. Splitting that as
a reference yields a repository of `sha256`, which then matches no digest and
reports `no_matching_repo_digest`: a true answer to a nonsense question. It
gets its own reason, `image_id_reference`.

**Two timeouts, because the calls are not alike.** `/containers/json` and
`/images/{id}/json` are answered by the daemon out of its own state;
`/distribution/{ref}/json` makes it perform a TLS handshake, a token exchange
and a manifest fetch against a possibly distant registry. One number for both
has to be either too loose for the local calls or too tight for the remote one,
and too tight there reports `distribution_query_failed` for a registry that was
merely slow — a failure the operator cannot reproduce by hand, because by hand
it succeeds. So `timeout` stays at the `docker` module's 5s and
`registry_timeout` defaults to 30s.

**The run carries a budget, derived rather than configured.** Cost scales with
the container count, and Telegraf kills an `inputs.exec` helper that overruns
its timeout — a killed helper reports *nothing*, not even the verdicts it had
already established. So the check is given a budget of half its own interval,
capped at five minutes, and the containers it does not reach report
`budget_exceeded` rather than being silently absent. The rendered `inputs.exec`
`timeout` is that budget plus a fixed margin, so Telegraf's patience always
outlasts the check's own **by construction**, not by a comment asking someone
to keep two numbers in step. Neither is a config key: the relationship has one
right answer, and exposing both would be exposing two ways to get it wrong.

The same bound applies to the startup one-shot in `supervisor.rs`, which runs
*before* the supervise loop begins multiplexing signals. An unbounded wait
there is a SIGTERM the container does not answer; the wait is capped at the
same number Telegraf uses, and a check that overruns it is abandoned rather
than waited on.

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
  or two Docker API round trips each, sequentially. Rather than assume a
  container count muninn cannot know at render time, the run is bounded: what
  does not fit reports `budget_exceeded`, and a host that sees that regularly
  should narrow `container_include`/`container_exclude` or raise `interval`.
  See `docs/modules.md#image_updates`.
- **The verdict logic is unit-tested against a scripted `DockerApi`**, and the
  whole path is exercised against a real daemon by
  `scripts/image-updates-test.sh` — including a container deliberately made
  stale by re-tagging, so `update_available=1` is asserted against a known
  answer rather than against whatever a registry happens to serve that day.
  The counterpart to `updates-test.sh`, for the same reason ADR-0009 wanted
  one.
- **Registries rate-limit anonymous callers.** Docker Hub allows 100 anonymous
  pulls per 6 hours per IP, and a manifest lookup counts against it. The
  module's default `interval` is 1 hour, matching `updates`, and validation
  refuses anything under a minute for the same reason `updates.interval` does.
- **Only public images are verified.** A private registry the *host* can
  already reach works through the daemon's own stored credentials with no
  change to muninn, but that path has not been measured the way ADR-0009's
  evidence measured the updates module against real hosts. Carried as
  [R9](../risks.md) and listed under "Next" in `docs/roadmap.md`.
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
