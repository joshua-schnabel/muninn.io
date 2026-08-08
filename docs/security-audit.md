# Security audit — 2026-08-08

A review of muninn.io's own code and configuration, prompted by huginn.io's
audit of 2026-08-02: the two projects share conventions, and one of that audit's
findings turned out to apply here and had never been looked for.

## Scope and method

**Read, not run.** This is a source review. Every claim below was checked against
the code and, where a guard exists, against the test that holds it. Nothing here
was measured against a running container — the sibling audit's headline finding
came with numbers from the shipped image, and this one has none. That is a real
difference in strength and is stated up front rather than buried.

| | |
|---|---|
| **Reviewed** | secret loading and redaction, the rendered Telegraf configuration, the generated config's lifetime and permissions, process invocation (`apt-get`, `telegraf`), the Docker Engine API client, the health listener, what leaves the process in logs |
| **Not reviewed** | Telegraf itself, the base image's packages, the CI pipeline (covered in [`ci-cd.md`](ci-cd.md)), anything requiring a running daemon or host |
| **Commit** | `dev` at the time of writing |

Findings are numbered `M-nn` so they cannot be confused with huginn's `F-nn`.

## Findings

| | Severity | Area | Summary | Status |
|---|---|---|---|---|
| [M-01](#m-01) | Low | `muninn-core` | Secret file permissions are neither checked nor reported | **Fixed** — warns when a secret is readable beyond its owner |

One finding. That is not a claim that muninn is secure; it is what a source
review of these surfaces produced, and the section below on what was checked and
found sound is the more useful half of the document.

### M-01 — Secret file permissions are neither checked nor reported {#m-01}

**Severity:** Low · **Status:** Fixed in this pass

`Secret::from_file` reported what went wrong when it could not read a file —
missing, unreadable, empty — but never looked at the mode.
[`configuration.md`](configuration.md) and [`hardening.md`](hardening.md) both
prescribe `0600`; nothing checked it, and nothing said when it was not so.

**Why it is sharper here than in the sibling project.** huginn carries the same
gap and its image is distroless: no shell, no package manager, one process. The
set of things that could read a world-readable token is nearly empty. muninn's
runtime is **debian-slim**, because the updates module needs real `apt` and
`dpkg` — a shell and 88 packages, deliberately
([ADR-0009](adr/0009-updates-module-approach.md)). A token file left `0644` in a
mount is readable by anything that achieves execution in that container, and
here there is something to execute.

**Not exploitable on its own.** It needs a second failure: an operator mounting
a secret with loose permissions *and* an attacker already running code in the
container. It is a defence-in-depth gap, not a way in.

**Fixed** by stating the file after opening and warning when it is group- or
world-readable. A warning rather than a refusal: a read-only bind mount can
carry permissions the operator does not control, and refusing to start over a
mode bit would take down a deployment whose token works perfectly. The check
sits in the one place every secret already passes through, and names the path,
never the contents.

Unix only — mode bits are the check, and there is nothing equivalent to look at
elsewhere. Which means the code path and its two tests are **compiled out on the
maintainer's Windows machine and first exercised by CI on Linux**; that is a
weaker verification than the rest of this document and is worth knowing.

## Checked, and found sound

The parts worth recording, because "we looked and it holds" is what makes the
next review cheaper.

**A configuration value cannot inject a Telegraf plugin.** The renderer escapes
what it writes, and `an_operator_value_cannot_inject_a_plugin_into_the_file`
proves it with a value carrying `"`, a newline and a `[[inputs.exec]]` block.
This is the attack [ADR-0004](adr/0004-no-raw-toml.md) exists to make impossible,
and the test is the thing that keeps it impossible.

**The Docker API client cannot be made to split a request.** Paths are built
with `format!` from daemon-supplied values, which would be a request-smuggling
candidate — except `get()` refuses any path containing a control character or a
space before the request is sent. CRLF cannot reach the socket. The references
are deliberately not percent-encoded, which is documented at the call site: the
daemon must receive the reference exactly as `docker pull` would take it, and
the daemon validated it when the container was created.

**`apt-get` is invoked without a shell.** Arguments are built as `OsString` and
passed individually; the code comments record that `format!` was avoided because
it would replace bytes it cannot render and silently point apt at a different
file. There is no string that a shell ever sees.

**The generated configuration is 0600 at creation.** Not `set_permissions` after
the fact — the mode is on the `OpenOptions`, so the file is never briefly
world-readable, and it is re-asserted for the case where the file already
existed. Two tests cover both paths. It holds resolved secrets by design, lives
on a tmpfs, and is never persisted
([ADR-0003](adr/0003-ephemeral-generated-config.md)).

**Telegraf's output is redacted before muninn re-emits it.** Both stdout and
stderr pass through `Redactor`. This is the gap that type-level redaction cannot
close: `Secret`'s `Debug` and `Display` protect everything muninn formats
itself, and nothing at all about what a child process writes.
`every_resolved_secret_is_redactable` walks a configuration with every
credential set and fails if one is missing from the redactor, which is what stops
a newly added credential from quietly falling outside it.

**Secrets are file paths, never values and never environment.** One `expose()`
on the path that builds the redactor, one where a module needs the value. Both
are greppable, which is the design.

**The health listener now bounds what a peer can hold.** 256 connections and a
ten-second header deadline, added in response to huginn's F-03 — the finding
that prompted this review. Port 8080 is meant to be published, so the unbounded
accept loop was the normal deployment rather than an unusual one.

## Accepted risk

Recorded in [`risks.md`](risks.md) and unchanged by this review — these are
decisions, not oversights:

1. **`/:/hostfs:ro` includes `/etc/shadow`.** The updates module needs the
   host's package state. Discussed plainly in
   [`host-mounts.md`](host-mounts.md).
2. **The Docker socket is root-equivalent**, and `:ro` protects the socket file,
   not the API. Off by default, proxy recommended
   ([ADR-0010](adr/0010-docker-socket.md)).
3. **The runtime image carries a shell and a package manager** — the measured
   trade behind [R7](risks.md), and what makes M-01 worth fixing.
4. **Six suppressed image findings**, all in Go modules vendored into the
   Telegraf binary, each with an expiry and a reachability argument
   ([`hardening.md`](hardening.md)).

## Recommendations

In priority order, and none of them urgent:

1. **Give huginn.io the same check.** M-01 is fixed here; the identical gap is
   its [R5](https://github.com/joshua-schnabel/huginn.io/blob/dev/docs/risks.md),
   still open. The projects are kept aligned deliberately, and a fix that lands
   in one of them is half a fix.
2. Re-run this review **against a running container**, with the sibling audit's
   method: measure rather than read. A connection flood, a crafted image
   reference through a real daemon, and a secret mounted `0644` would each turn
   a paragraph above from reasoning into evidence.
3. Re-run it after any change to the renderer, the Docker client, or the secret
   path — those are the three surfaces where a regression would be silent.

## Related

- [`hardening.md`](hardening.md) — the posture this reviewed
- [`risks.md`](risks.md) — the open risks, including those accepted above
- [`SECURITY.md`](SECURITY.md) — how to report a vulnerability
- huginn.io's `docs/security-audit.md` — the 2026-08-02 review that prompted this
