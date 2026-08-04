# Risks and open questions

Live document. Risks are removed when they are resolved, not when they stop being
mentioned.

## R7 — The runtime image carries a shell and a package manager

**Severity: medium · Status: accepted trade, monitor**

The updates module needs real `apt` and `dpkg` in the image, so the base is
`debian:12-slim` rather than distroless: 88 packages instead of 10, and 5
CRITICAL / 17 HIGH CVEs instead of none. All are currently unfixable, and four of
the five CRITICAL are in `perl-base`, which muninn never invokes.

The CVE count is the lesser problem. A shell and a package manager inside a
container that mounts the host filesystem is a real convenience for anyone who
achieves code execution there.

**Mitigation.** Non-root, all capabilities dropped, read-only root filesystem,
`no-new-privileges`, read-only host mount — all verified working with the apt
invocation. Documented with the measurements in [`hardening.md`](hardening.md).

**Residual.** Unfixable today means blocking tomorrow: once Debian ships fixes,
the Trivy gate starts failing until the image is rebuilt. That is the intended
behaviour, and it means image currency becomes an operational duty rather than a
nicety.

**Revisit if** the CVE surface starts producing regular build failures unrelated
to muninn's own code. The two-variant scheme — distroless by default, debian-slim
for the updates module — was set aside for CI simplicity, not because it was
unworkable.

## R8 — The security subset under-reports on Ubuntu

**Severity: medium · Status: known limit, documented**

`muninn_updates_pending{severity="security"}` classifies an update as security
when the origin apt prints for the **candidate version** names a `-security`
suite. That is the rule [ADR-0009](adr/0009-updates-module-approach.md) fixed, and
on Debian it is accurate.

On Ubuntu it is a lower bound. Ubuntu publishes security updates to
`<release>-security` *and* copies them into `<release>-updates`; when apt resolves
the candidate through the latter, the `Inst` line reads `Ubuntu:24.04/noble-updates`
and muninn does not count it as security. This is measurable rather than
theoretical: the Ubuntu 24.04 fixture reported 66 pending / 34 security, and the
same fixture rebuilt against today's archive reports **66 pending / 0 security**
— the packages are the same, the pocket holding the candidate has moved. The
total is unaffected.

The host's own `apt-get -s dist-upgrade` says exactly the same thing, so muninn is
not diverging from its host — which is why this is a limit rather than a bug, and
why every system-test cell still passes.

**Mitigation.** Documented at the metric, in [`modules.md`](modules.md#updates):
alert on the total, and treat the security series as "at least this many". A
security count of zero on an Ubuntu host is not evidence that nothing security-
relevant is pending.

**Fix, if it is worth the cost.** Ubuntu's own `apt-check` classifies by asking
whether the candidate *version* is available from any security origin, rather than
reading the one origin apt happens to print — `apt-cache policy` exposes that.
It is a second apt invocation and a second parser, and it would change numbers
that were measured, so it needs its own ADR amendment and its own ground truth
rather than a quiet change here.

## R9 — image_updates is verified against public registries only

**Severity: medium · Status: known limit, documented**

The `image_updates` module never speaks to a registry itself: it asks the
Docker daemon to resolve each container's tag, and the daemon uses whatever
registry credentials the host is already configured with
([ADR-0013](adr/0013-image-updates-via-docker-api.md)). That should mean a
private registry the host can already pull from works with no credential
handling in muninn at all.

**"Should" is the risk.** That path has not been measured. The unit tests
script the daemon's answers, and `scripts/image-updates-test.sh` exercises the
real path against Docker Hub — both public. Nothing in the repository records
what happens when the daemon holds credentials, when they have expired, or when
the registry answers `401` rather than failing to connect.

All of those land in `distribution_query_failed`, and so does a **fourth** case
that is not about private registries at all: an image built locally on a daemon
using the containerd image store. That store records a locally computed digest,
so `no_repo_digest` — the reason written for exactly this case — no longer
fires, and the check goes on to ask a registry about a repository that was
never pushed. Measured, on Docker 29.3.1, by cell I5 of the system suite. So
one token now covers "unreachable", "unauthorised", "no such repository" and
"never left this host".

The invariant is not at risk: every one of those cases reports
`check_success=0` with a reason and no verdict, never "up to date". What is at
risk is usefulness — an operator on a private registry may get a module that
reports nothing but failures, and a reason token that does not tell them which
of the three it is.

**Mitigation.** Stated plainly at the metric, in
[`modules.md`](modules.md#image_updates), and in the ADR's consequences. An
operator can tell the difference by hand today: `muninn image-check` prints the
daemon's own message on stderr, including the `401`.

**Fix, if it is worth the cost.** Ground truth first — a local registry with
authentication in the system suite — and then, if the distinction proves worth
carrying, splitting `distribution_query_failed` by cause. The locally built
case is the cheapest of the four to separate, because the daemon's own error
distinguishes it; the rest need evidence. Splitting the token without that
would be inventing a distinction rather than measuring one.

## Mitigated

Real failure modes, each closed by a decision that is documented where the
decision lives. Kept by ID because other pages cite them.

- **R2 — two Prometheus endpoints invite scraping the wrong one.** `:9273` alone
  cannot tell a dead agent from a dead host; the health port alone has no host
  data. Both are needed, and the split is load-bearing
  ([ADR-0012](adr/0012-self-metrics-on-health-server.md)). Stated in the README,
  in the annotated example config at both keys, and in
  [`configuration.md`](configuration.md#two-metrics-endpoints).
- **R3 — a container hostname silently fragments time series.** Telegraf uses
  `os.Hostname()`, which in a container changes on every recreate; nothing
  errors, and dashboards lose their history. muninn warns at startup when
  `agent.hostname` is empty and it detects a container. Deliberately a warning,
  not a refusal — running muninn directly on a host is legitimate, and there the
  OS hostname is exactly right.
- **R4 — `config check` does not catch everything.** It initialises plugins
  without starting them, so it cannot see a missing mount, an occupied port or an
  absent Docker endpoint. `muninn check-runtime` is a separate startup step for
  exactly those, and readiness follows Telegraf running rather than validation
  passing.
- **R5 — the Telegraf plugin surface drifts between minor versions.** Options and
  defaults move, so a config generated for one version can be rejected or quietly
  reinterpreted by another. Telegraf is pinned by checksum
  ([ADR-0011](adr/0011-telegraf-pinning.md)), muninn refuses to start when the
  runtime binary disagrees with the build-time pin, and
  `scripts/verify-design-package.sh` checks every documented option against that
  version's `sample.conf`.
- **R6 — snapshot tests decay if accepted without review.** `cargo insta accept`
  is one keystroke, and this is the main way the determinism guarantee could rot.
  `cargo snap-review` is the documented workflow, the rule is in `AGENTS.md` and
  [`testing.md`](testing.md), and the reference config is verified against real
  Telegraf independently of any snapshot.

## Open questions

**O3 — Bounded restart mechanism?**
[ADR-0002](adr/0002-supervisor-no-restart-loop.md) leaves room for an optional
bounded restart — off by default, at most three attempts, exponential backoff.
Whether it is worth the complexity should be decided from operational experience,
not in advance.

## Decided

Kept as one line each, because other pages cite them.

- **R1 — can the host's package state be read from a container at all?** Yes,
  exactly, and a failure is always distinguishable from a zero. Accepted in
  [ADR-0009](adr/0009-updates-module-approach.md); measured in
  [`updates-evidence.md`](updates-evidence.md). What remains of it is R7.
- **O1 — should `muninn validate` invoke `telegraf config check`?** Static
  validation by default, `--with-telegraf` as opt-in, because the check needs the
  Telegraf binary and therefore only works inside the image.
- **O2 — Docker Hub in addition to ghcr?** Both, Docker Hub first, ghcr mirrored
  from the finished manifest with `skopeo copy --all` — one build, byte-identical
  images, no second push path.
  [`ci-cd.md`](ci-cd.md#repository-settings--maintainer-by-hand).

## Related

- [`roadmap.md`](roadmap.md) — what is still open
- [`updates-evidence.md`](updates-evidence.md) — the measurements behind R1 and R8
