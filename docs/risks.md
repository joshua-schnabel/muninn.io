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

## R2 — Two Prometheus endpoints invite scraping the wrong one

**Severity: medium · Status: mitigated, monitor**

Telegraf serves host metrics on `:9273`. muninn serves its own operational
metrics on the health port. Scraping only one gives a partial picture that looks
complete: `:9273` alone cannot distinguish a dead agent from a dead host, and the
health port alone gives nine metrics and no host data.

The split is deliberate and load-bearing —
[ADR-0012](adr/0012-self-metrics-on-health-server.md) — because
`muninn_telegraf_running 0` has to be readable exactly when Telegraf's endpoint
is gone.

**Mitigation.** Called out in the annotated example config next to both keys, in
the README, and in [`configuration.md`](configuration.md#two-metrics-endpoints)
before the reference rather than inside it, with a two-job scrape configuration.

## R3 — Container hostname silently fragments time series

**Severity: medium · Status: mitigated**

Telegraf uses `os.Hostname()`. In a container that is the container ID, which
changes on every recreate — so every deploy starts a fresh time series and
dashboards lose their history. Nothing errors. Mounting the host's `/etc` does
not help; the hostname comes from the UTS namespace.

**Mitigation.** muninn warns at startup when `agent.hostname` is empty and it
detects a container. The example config carries a READ THIS marker at the key,
and the compose example sets `hostname:`.

**Residual.** A warning can be ignored. Making it fatal was considered and
rejected — it would break the legitimate case of running muninn directly on a
host, where the OS hostname is exactly right.

## R4 — `config check` does not catch everything

**Severity: medium · Status: mitigated by design**

`telegraf config check` initialises plugins without starting them. It therefore
cannot see a Docker endpoint that does not exist, a port already taken on the
host, or a mount that is missing. Treating it as complete validation would mean
those surface as a Telegraf crash after startup reported success.

**Mitigation.** `muninn check-runtime` is a separate startup step (5) that checks
exactly what `config check` cannot. Readiness is reported only after Telegraf is
confirmed running — never after validation alone.

## R5 — Telegraf plugin surface drifts between minor versions

**Severity: low · Status: mitigated**

Option names and defaults move. `inputs.system.include` and
`outputs.prometheus_client.name_sanitization` are both recent additions, and
`skip_processors_after_aggregators` changes its default in 1.40. A config
generated for one version can be rejected — or worse, silently reinterpreted — by
another.

**Mitigation.** Telegraf is pinned by checksum
([ADR-0011](adr/0011-telegraf-pinning.md)); muninn compares the runtime binary's
version against the build-time pin and refuses to start on a mismatch; and
`scripts/verify-design-package.sh` checks every documented plugin option against
the pinned version's `sample.conf`.

## R6 — Snapshot tests decay if accepted without review

**Severity: low · Status: process control only**

`cargo insta accept` is one keystroke, and a snapshot suite accepted without
reading is a record of whatever the code happens to do rather than a check on it.
This is the main way the determinism guarantee could rot.

**Mitigation.** `cargo snap-review` is the documented workflow, the rule is in
`AGENTS.md` and `testing.md`, and the reference config is verified against real
Telegraf independently of any snapshot — so a wrongly-accepted snapshot still has
one external check standing.

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
