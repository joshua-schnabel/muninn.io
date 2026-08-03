# Risks and open questions

Live document. Risks are removed when they are resolved, not when they stop being
mentioned.

## R1 — The updates module may have no safe answer

**Severity: was high · Owner: WP1 · Status: RESOLVED 2026-08-02**

Reading the host's package state from a container turned out to be solvable, and
the [spike](spikes/updates-spike.md) measured it rather than argued it: approach A
reproduces the host's own answer exactly across Debian 12/13 and Ubuntu
22.04/24.04, including from a container running a different distribution, under
non-root with `--cap-drop=ALL` and a read-only root filesystem, leaving the host
tree byte-identical.

Failure is detectable, which was the property the module stood on: a missing
mount, an empty dpkg status and a corrupt one each produce `check_success=0` with
the pending counts omitted — never a zero.

Accepted in [ADR-0009](adr/0009-updates-module-approach.md). Superseded by R7.

## R7 — The runtime image carries a shell and a package manager

**Severity: medium · Owner: WP8/WP12 · Status: accepted trade, monitor**

R1's resolution needs real `apt` and `dpkg` in the image, so the base is
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

**Severity: medium · Owner: WP10 · Status: known limit, documented**

`muninn_updates_pending{severity="security"}` classifies an update as security
when the origin apt prints for the **candidate version** names a `-security`
suite. That is the rule [ADR-0009](adr/0009-updates-module-approach.md) fixed, and
on Debian it is accurate.

On Ubuntu it is a lower bound. Ubuntu publishes security updates to
`<release>-security` *and* copies them into `<release>-updates`; when apt resolves
the candidate through the latter, the `Inst` line reads `Ubuntu:24.04/noble-updates`
and muninn does not count it as security. This is measurable rather than
theoretical: the WP1 spike's Ubuntu 24.04 fixture reported 66 pending / 34
security, and the same fixture rebuilt against today's archive reports **66
pending / 0 security** — the packages are the same, the pocket holding the
candidate has moved. The total is unaffected.

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
It is a second apt invocation and a second parser, and it would change the numbers
the spike measured, so it needs its own ADR amendment and its own ground truth
rather than a quiet change here.

## R2 — Two Prometheus endpoints invite scraping the wrong one

**Severity: medium · Owner: WP0/WP7 · Status: mitigated, monitor**

Telegraf serves host metrics on `:9273`. muninn serves its own operational
metrics on the health port. Scraping only one gives a partial picture that looks
complete: `:9273` alone cannot distinguish a dead agent from a dead host, and the
health port alone gives nine metrics and no host data.

The split is deliberate and load-bearing —
[ADR-0012](adr/0012-self-metrics-on-health-server.md) — because
`muninn_telegraf_running 0` has to be readable exactly when Telegraf's endpoint
is gone.

**Mitigation.** Called out in the annotated example config next to both keys, in
`configuration.md` before the reference rather than inside it, and in the README
with a two-job scrape configuration.

## R3 — Container hostname silently fragments time series

**Severity: medium · Owner: WP2/WP7 · Status: mitigated**

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

**Severity: medium · Owner: WP6/WP8 · Status: mitigated by design**

`telegraf config check` initialises plugins without starting them. It therefore
cannot see a Docker endpoint that does not exist, a port already taken on the
host, or a mount that is missing. Treating it as complete validation would mean
those surface as a Telegraf crash after startup reported success.

**Mitigation.** `muninn check-runtime` is a separate startup step (5) that checks
exactly what `config check` cannot. Readiness is reported only after Telegraf is
confirmed running — never after validation alone.

## R5 — Telegraf plugin surface drifts between minor versions

**Severity: low · Owner: WP6 · Status: mitigated**

Option names and defaults move. `inputs.system.include` and
`outputs.prometheus_client.name_sanitization` are both recent additions, and
`skip_processors_after_aggregators` changes its default in 1.40. A config
generated for one version can be rejected — or worse, silently reinterpreted — by
another.

**Mitigation.** Telegraf is pinned by checksum
([ADR-0011](adr/0011-telegraf-pinning.md)); muninn compares the runtime binary's
version against the build-time pin and refuses to start on a mismatch; WP0's
verification suite checks every documented plugin option against the pinned
version's `sample.conf`.

## R6 — Snapshot tests decay if accepted without review

**Severity: low · Owner: WP3+ · Status: process control only**

`cargo insta accept` is one keystroke, and a snapshot suite accepted without
reading is a record of whatever the code happens to do rather than a check on it.
This is the main way the determinism guarantee could rot.

**Mitigation.** `cargo snap-review` is the documented workflow, the rule is in
`AGENTS.md` and `testing.md`, and the reference config is verified against real
Telegraf independently of any snapshot — so a wrongly-accepted snapshot still has
one external check standing.

## Open questions

**O1 — Should `muninn validate` invoke `telegraf config check`?**
Doing so requires the Telegraf binary, which effectively means it only works
inside the image. Proposal: static validation by default, `--with-telegraf` as
opt-in. Decide in WP2.

**O2 — Docker Hub in addition to ghcr?**
CI publishes to `ghcr.io/joshua-schnabel/muninn.io`, which works with the
built-in `GITHUB_TOKEN`. A Docker Hub mirror needs a `DOCKERHUB_TOKEN`
repository secret. Setting repository secrets is deliberately outside what the
agent does, so `docs/ci-cd.md` records the steps for the maintainer. Decide in
WP12.

**O3 — Bounded restart mechanism?**
[ADR-0002](adr/0002-supervisor-no-restart-loop.md) leaves room for an optional
bounded restart — off by default, at most three attempts, exponential backoff.
Whether it is worth the complexity should be decided from operational experience,
not in advance. Revisit after the MVP.

## Related

- [`roadmap.md`](roadmap.md) — which work package owns each risk
- [`spikes/updates-spike.md`](spikes/updates-spike.md) — R1 in detail
