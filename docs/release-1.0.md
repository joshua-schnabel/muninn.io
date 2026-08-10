# Release 1.0 readiness

Review of `dev` for work that should be completed before the public interfaces
are stabilised as 1.0. The review focused on incomplete features, incomplete or
drifting documentation, and security weaknesses.

[`versioning.md`](versioning.md) is what makes this list worth having: at 1.0
every YAML key, every exit code, every `muninn_*` metric name, the two health
endpoints and the container contract stop being changeable without a major
release. Anything wrong or unfinished on those surfaces is cheap to fix now and
expensive afterwards.

## Review baseline

| | |
|---|---|
| **Branch** | `dev` |
| **Commit** | `5339b9f419363b7e184a7cdd4ab18bf297cc72e0` |
| **Working tree** | clean before this document was added |
| **Local gates** | format, clippy, workspace tests, cargo-deny, coverage threshold, and release build passed on the preceding baseline |
| **CI evidence** | the current commit passed the Linux test matrix, coverage gate, cargo-deny, design-package verification, Semgrep, Trivy, both native image builds, both hardened-container integration jobs, and both updates system suites |
| **Limitation** | the local Docker daemon was unavailable, so the connection-saturation and container-shutdown findings below are source-derived rather than locally reproduced |

The green pipeline is a strong release baseline. It does not settle behavioural
contracts that the tests do not exercise, especially error classification,
startup and shutdown races, or the meaning of stable metrics.

Findings are numbered `F-nn` from the first pass and `N-nn` from the second, so
that neither renumbers the other. Both are equally binding.

## The work, and where it lands

Every finding is assigned to one pull request. The grouping is by the surface
being changed, not by severity, because the parts of a lifecycle have to move
together or not at all.

| PR | Branch | Findings |
|---|---|---|
| 0 | `docs/release-1.0-findings` | This document |
| 1 | `fix/secret-leak-paths` | [F-01](#f-01--secret-values-can-still-reach-child-and-validator-diagnostics), [F-02](#f-02--the-secret-file-permission-warning-is-not-observable) |
| 2 | `fix/lifecycle-contract` | [F-03](#f-03--runtimetelegraf_start_timeout-is-not-implemented-as-documented), [F-05](#f-05--health-server-failure-and-startup-state-contracts-do-not-match-code), [F-06](#f-06--the-startup-updates-check-can-block-signal-handling-indefinitely), [N-02](#n-02--degraded-is-one-way-and-the-self-check-metrics-freeze-at-startup) |
| 3 | `fix/shutdown-and-io` | [F-07](#f-07--a-saturated-health-listener-delays-shutdown), [F-10](#f-10--telegraf-output-forwarders-are-detached-and-not-drained), the shutdown-classification and `expect` hygiene items |
| 4 | `fix/filesystem-and-probe-contract` | [F-08](#f-08--muninn-healthcheck-reloads-the-whole-configuration-and-all-secrets), [F-09](#f-09--secret-bearing-files-and-runtime-probes-follow-predictable-paths), [F-18](#f-18--port-zero-is-accepted-but-cannot-be-operated) |
| 5 | `feat/prometheus-tls` | [N-01](#n-01--outputsprometheus-can-only-send-basic-auth-in-the-clear) |
| 6 | `feat/updates-security-classification` | [F-04](#f-04--ubuntu-security-updates-can-be-reported-as-zero-incorrectly) |
| 7 | `feat/image-updates-evidence` | [F-11](#f-11--image_updates-has-two-conflicting-meanings-of-success), [F-14](#f-14--authenticated-registry-behaviour-has-no-system-evidence) |
| 8 | `chore/ci-and-supply-chain` | [F-12](#f-12--the-declared-msrv-is-not-compiled-in-ci), [F-13](#f-13--release-credentials-remain-available-for-too-much-workflow-code), the cargo-deny hygiene items |
| 9 | `docs/1.0-sweep` | [F-15](#f-15--the-roadmap-and-examples-describe-old-release-state), [F-16](#f-16--module-vulnerability-and-link-documentation-has-drifted), [F-17](#f-17--there-is-no-current-canonical-reference-for-stable-self-metrics), [N-03](#n-03--design-package-verification-checks-only-half-the-plugin-surface), [N-04](#n-04--requirementscapabilities-is-declared-and-checked-by-nothing), [N-05](#n-05--dead-public-api-and-stale-comments), remaining hygiene |

## Blockers before 1.0

### F-01 — Secret values can still reach child and validator diagnostics

**Severity:** High · **Area:** Security, secrets · **Status:** Open · **PR 1**

`Redactor` deliberately excludes values shorter than eight bytes, while config
loading accepts such values without a warning or rejection
([`secret.rs`](../crates/muninn-core/src/secret.rs)). A short but valid Basic
Auth password or token printed by Telegraf therefore passes through unchanged.

The separate `telegraf config check` path has no redactor at all. On failure it
copies Telegraf's stdout and stderr directly into `MuninnError`
([`validator.rs`](../crates/muninn-telegraf/src/validator.rs)). The generated
configuration being checked contains resolved secrets, so a plugin diagnostic
that quotes a value can put it in an error. This contradicts the security
contract that secrets must never appear in logs or errors.

**Release acceptance:**

- every accepted credential is protected in child-process and validator output;
- short credentials are either safely redacted or rejected with an actionable
  configuration error;
- a fake validator and a real forwarding-path test emit configured secrets and
  prove they are absent from the resulting error and log event.

### F-02 — The secret-file permission warning is not observable

**Severity:** Medium · **Area:** Security, diagnostics · **Status:** Open;
currently documented as fixed · **PR 1**

The permission check added for security finding M-01 emits its warning through
`tracing::warn!` ([`secret.rs`](../crates/muninn-core/src/secret.rs)). Secrets
are loaded before `run` initialises the tracing subscriber
([`main.rs`](../muninn/src/main.rs)). The other config-reading commands never
initialise that subscriber. The event is therefore discarded in the paths where
an operator is meant to see it.

The two Unix tests only prove that a loosely permissioned secret still loads;
they do not capture or assert the warning. As a result, the changelog and
[`security-audit.md`](security-audit.md) overstate the fix.

**Release acceptance:** return the permission finding through the same warning
channel as semantic configuration warnings, print it on stderr for every
config-reading command, add a Linux test that asserts the actual diagnostic, and
correct the two documents that call M-01 closed.

### F-03 — `runtime.telegraf_start_timeout` is not implemented as documented

**Severity:** High · **Area:** Stable configuration and exit-code contract ·
**Status:** Open · **PR 2**

The configuration reference says the value is how long Telegraf may take to come
up before muninn exits with code 21
([`configuration.md`](configuration.md); the link is to the page rather than to
the key's section, because removing that section is what closed this finding).
The
implementation sleeps for at most half a second and treats a process that is
merely still alive as ready ([`supervisor.rs`](../muninn/src/supervisor.rs)).
Longer values have no effect. A failure after that window becomes an operational
crash with exit 22 rather than a start failure with exit 21.

**Release acceptance:** implement a measurable ready/start criterion governed by
the configured timeout, or remove and replace the setting before the config and
exit-code surfaces become stable. Lifecycle tests must cover a delayed success, a
delayed failure, and the exact timeout boundary.

### F-04 — Ubuntu security updates can be reported as zero incorrectly

**Severity:** High · **Area:** Feature correctness, security metrics ·
**Status:** Known limit, tracked as [R8](risks.md) · **PR 6**

The updates module classifies the security subset from the origin printed for
the selected candidate. Ubuntu may copy the same candidate into its updates
pocket, causing a host with security-relevant updates to produce a security
count of zero. The limitation and measured example are already recorded in
[`risks.md`](risks.md).

This is unsafe to freeze as stable 1.0 metric semantics: a plausible false zero
can suppress an alert, which is the one failure mode
[AGENTS.md §9](../AGENTS.md) singles out as the project's sharpest rule.

**Release acceptance:** classify the candidate version against all applicable
security origins, amend [ADR-0009](adr/0009-updates-module-approach.md), and add
Debian and Ubuntu ground-truth cells that independently establish both the total
and security subset.

### F-05 — Health-server failure and startup-state contracts do not match code

**Severity:** High · **Area:** Health API, lifecycle, documentation ·
**Status:** Open · **PR 2**

The health task discards the result of `serve_on`, and its join result is also
ignored ([`main.rs`](../muninn/src/main.rs)). A task failure or panic can drop
the listener while the supervisor continues. The operational contract instead
says a permanently failed health server exits with code 30
([`supervision.md`](supervision.md)).

Two published states are unreachable in production. `HealthState` is created
only after configuration load and validation, so `LoadingConfiguration` and
`ValidatingConfiguration` can never be served
([`state.rs`](../crates/muninn-health/src/state.rs)).

Startup failures also do not transition to `Failed`. `Failed` is set on exactly
one path — Telegraf exiting after it was confirmed running — while every startup
error returns from the supervisor without a transition: the version check, the
runtime preconditions, writing the generated config, `telegraf config check`,
the spawn, and the start confirmation. The listener is bound and serving before
any of them, so `/health/live` answers 200 and `muninn_state` reports a startup
state right up to the moment the process exits non-zero. The state diagram in
[`architecture.md`](architecture.md) draws an arrow to `Failed` from each of
them.

Meanwhile that same document says the early startup steps have no listener, even
though the health listener is bound and serving before the runtime check,
rendering, and Telegraf validation.

**Release acceptance:** choose and implement one coherent lifecycle. Monitor the
health task as part of the supervisor, make its permanent failure fatal, and
either make every public state reachable with tested transitions or remove the
unreachable states and update the diagrams and endpoint contract.

### N-01 — `outputs.prometheus` can only send Basic Auth in the clear

**Severity:** High · **Area:** Security, stable config surface ·
**Status:** Open · **PR 5**

`outputs.influxdb` carries the full TLS surface — CA, client certificate, key
and `insecure_skip_verify` — and warns when its URL is plaintext HTTP, because
the API token goes out with every write
([`validation.rs`](../crates/muninn-core/src/config/validation.rs)).

`outputs.prometheus` carries `basic_auth` and **no TLS keys at all**, although
the pinned Telegraf's `prometheus_client` exposes them. Nothing warns. So the
one place muninn sends a credential rather than receiving one is also the only
one with no confidentiality option, and [`configuration.md`](configuration.md)
actively recommends setting basic auth without saying the password then crosses
the network in the clear on every scrape.

The `basic_auth` rendering branch also has **no tests**. The output tests cover
URLs, redaction, TLS omission, the listener and ordering, but never set
`basic_auth`; the shipped example leaves both keys null, so the block is absent
from [`telegraf.reference.conf`](reference/telegraf.reference.conf) as well. The
first execution of that code is an operator's.

**Release acceptance:** model `outputs.prometheus.tls` after the InfluxDB one and
render it under the option names the pinned Telegraf's `sample.conf` actually
uses; warn when `basic_auth` is set without TLS, symmetrically to the existing
plaintext-HTTP warning; cover both the existing `basic_auth` branch and the new
TLS branch with rendering tests. A new optional key is additive, but the warning
changes behaviour, so it belongs before the freeze.

### N-02 — `Degraded` is one-way, and the self-check metrics freeze at startup

**Severity:** High · **Area:** Stable metric semantics · **Status:** Open ·
**PR 2**

`record_module_check` is called only from the two startup self-checks, and those
run exactly once, immediately after readiness
([`supervisor.rs`](../muninn/src/supervisor.rs)). Telegraf re-runs the same
checks on the module interval through `inputs.exec`, but those results go to the
outputs; nothing feeds back into `HealthState`.

Two consequences, both on surfaces [`versioning.md`](versioning.md) declares
stable:

- `muninn_module_check_success` and `muninn_module_check_timestamp_seconds`
  ([`metrics.rs`](../crates/muninn-health/src/metrics.rs)) hold their startup
  values for the life of the container. The timestamp's own help text says *when
  a module last completed a self-check*; on a container up for a week it reports
  a week-old timestamp while the check has run every hour since.
- `Degraded` is never left. A transient registry failure during startup marks
  muninn degraded permanently, with nothing able to clear it.

Related to [F-11](#f-11--image_updates-has-two-conflicting-meanings-of-success) and [F-17](#f-17--there-is-no-current-canonical-reference-for-stable-self-metrics), but covered by neither.

**Release acceptance:** either feed the periodic results back into `HealthState`
and give `Degraded` a route back to `Ready`, or redefine both metric families so
their names and help text say "at startup" and mean it. Tests must cover a
recovery, not only a degradation.

## High-priority work

### F-06 — The startup updates check can block signal handling indefinitely

**Severity:** High · **Area:** Shutdown reliability · **Status:** Open · **PR 2**

After reporting readiness, muninn waits for the initial updates check before it
enters the `select!` that supervises Telegraf and signals
([`supervisor.rs`](../muninn/src/supervisor.rs)). The check executes `apt-get`
and waits for its output without a timeout
([`debian.rs`](../crates/muninn-modules/src/updates/debian.rs)). A signal is
buffered but not acted on until apt returns. A stuck apt process can therefore
make `docker stop` exceed the container grace period while readiness had already
reported success.

**Release acceptance:** bound and terminate the subprocess, begin signal and
child supervision before optional self-checks, and add a lifecycle test with a
hung helper and SIGTERM.

### F-07 — A saturated health listener delays shutdown

**Severity:** Medium · **Area:** Availability, container shutdown ·
**Status:** Open; source-derived, live reproduction pending · **PR 3**

The server awaits a semaphore permit before entering the `select!` that observes
shutdown ([`serve.rs`](../crates/muninn-health/src/serve.rs)). At the connection
limit, shutdown is invisible until a permit is released. With the configured
Telegraf grace period, header timeout, and connection drain, the total can exceed
the shipped Compose stop timeout.

Persistent HTTP/1.1 connections can also keep all permits occupied with a small
request below every header deadline, bounding memory but still starving genuine
probes.

**Release acceptance:** include permit acquisition in shutdown selection, define
the keep-alive policy, and add saturated-capacity tests for shutdown and probe
availability. Reproduce the connection flood against the hardened image.

### F-08 — `muninn healthcheck` reloads the whole configuration and all secrets

**Severity:** Medium · **Area:** Health semantics · **Status:** Open · **PR 4**

The comment says the command reads only the health address and deliberately does
not validate the configuration. It actually calls `load_and_resolve`
([`main.rs`](../muninn/src/main.rs)), which validates and reads every secret and
TLS file. The container `HEALTHCHECK` runs it on a short interval with retries,
so a config edit or a temporarily missing secret after startup can mark a healthy
running process unhealthy. An orchestrator may then restart it into the broken
configuration, turning a diagnostic mismatch into an outage — precisely the
failure the comment claims to avoid.

**Release acceptance:** make the probe depend only on the address used by the
running instance, document how that address is obtained, and test that unrelated
config and secret changes cannot alter the health verdict.

### F-09 — Secret-bearing files and runtime probes follow predictable paths

**Severity:** Medium · **Area:** Filesystem security · **Status:** Open ·
**PR 4**

The generated-config writer uses create-and-truncate on the final path and
follows an existing symlink
([`generated_config.rs`](../muninn/src/generated_config.rs)). The default
container directory is protected, but the path is configurable and direct-host
use is supported.

`validate --with-telegraf` uses a predictable per-process name when the
configured parent exists, then ignores deletion failure
([`main.rs`](../muninn/src/main.rs)). `check-runtime` similarly writes and
deletes a fixed write probe, which can truncate an existing file or follow a
symlink ([`runtime_check.rs`](../muninn/src/runtime_check.rs)).

**Release acceptance:** create secret-bearing files exclusively under random
names, keep mode `0600` from creation, replace the final file atomically without
following a symlink, and treat failed secret-file cleanup as an error. `tempfile`
is already a runtime dependency of the binary, so this needs no new crate.

### F-10 — Telegraf output forwarders are detached and not drained

**Severity:** Medium · **Area:** Diagnostics, supervision · **Status:** Open ·
**PR 3**

The stdout and stderr tasks are spawned without retaining their join handles
([`process.rs`](../crates/muninn-telegraf/src/process.rs)). Child exit can be
observed before those tasks have drained the final buffered lines, and the Tokio
runtime is then dropped. The diagnostic immediately preceding a crash can be lost
even though [`troubleshooting.md`](troubleshooting.md) tells operators to rely on
it.

The existing test named `a_secret_in_child_output_is_masked_before_it_is_logged`
only calls `Redactor::apply`; it never exercises the forwarding path or a logging
subscriber.

**Release acceptance:** retain and join both reader tasks on every exit path,
bound a pathological drain, and test a real child that emits final stdout and
stderr containing a secret immediately before exit.

### F-11 — `image_updates` has two conflicting meanings of success

**Severity:** Medium · **Area:** Stable metric and status semantics ·
**Status:** Open design decision · **PR 7**

The supervisor records the module as successful whenever container enumeration
succeeded ([`supervisor.rs`](../muninn/src/supervisor.rs)), and
`daemon_succeeded()` ignores every per-container outcome
([`check.rs`](../crates/muninn-modules/src/image_updates/check.rs)). `/status`
and `muninn_module_check_success{module="image_updates"}` can therefore say
success when every selected container has `image_inspect_failed`,
`distribution_query_failed`, or `budget_exceeded`.

The per-container metrics remain honest, but the aggregate name reads as module
success rather than daemon enumeration success.

**Release acceptance:** define aggregate versus partial success before metric
names become stable. Either require every selected container to receive a
verdict, or expose daemon reachability separately and document partial success
explicitly. Add all-failed and mixed-result tests for `/status` and both metric
surfaces.

### F-12 — The declared MSRV is not compiled in CI

**Severity:** Medium · **Area:** Compatibility, supply chain · **Status:** Open ·
**PR 8**

The supported floor is `rust-version` in [`Cargo.toml`](../Cargo.toml). The
Docker builder is newer than that floor ([`Dockerfile`](../Dockerfile)), while CI
runs only floating stable and beta ([`ci.yml`](../.github/workflows/ci.yml)).
Resolver 3 can choose dependencies compatible with the declared floor, but
neither the project source nor its tests are compiled by the floor compiler.

**Release acceptance:** add a required MSRV build or check job that reads the
authoritative manifest value rather than repeating it, and keep stable and beta
as the current and forward compatibility jobs.

### F-13 — Release credentials remain available for too much workflow code

**Severity:** Medium · **Area:** CI/CD security · **Status:** Open hardening ·
**PR 8**

The publish checkout persists the release token before repository scripts and
several third-party actions execute ([`ci.yml`](../.github/workflows/ci.yml)).
The comment that nothing between checkout and push executes third-party code is
not true for the current job.

The release workflows import the GPG private key and may write its passphrase
file before running the repository-controlled version and changelog transforms
([`release.yml`](../.github/workflows/release.yml)). Both automation paths also
force-push their reusable branch names.

**Release acceptance:** use `persist-credentials: false`, make the token
available only to the final tag or branch push, import and remove signing
material immediately around the commit, and replace unconditional force pushes
with a verified `--force-with-lease` flow. Anything requiring a repository
setting or secret change is named in the pull request rather than done there
([AGENTS.md §3](../AGENTS.md)).

### F-14 — Authenticated registry behaviour has no system evidence

**Severity:** Medium · **Area:** Incomplete feature verification ·
**Status:** Already on the roadmap as [R9](risks.md) · **PR 7**

`image_updates` is tested against public registries. There is no repository
evidence for a registry the Docker daemon accesses with stored credentials, an
expired credential, or a `401` response. All currently collapse into
`distribution_query_failed`; locally built images can reach the same reason.

**Release acceptance:** extend `scripts/image-updates-test.sh` with a local
authenticated registry, valid and expired credentials, and recorded expected
reasons before deciding whether the public reason vocabulary needs to change.

## Documentation work before 1.0

### F-15 — The roadmap and examples describe old release state

**Severity:** Medium · **Area:** Release documentation · **Status:** Open ·
**PR 9**

[`roadmap.md`](roadmap.md) still names the first release and its old development
tag as current. Several README, hardening, host-mount, release, and
troubleshooting examples similarly pin historical image versions even though
[AGENTS.md §7](../AGENTS.md) says the authoritative version should not be
repeated in prose.

The roadmap also says staging-tag deletion currently receives HTTP 403. The CI
step is deliberately best-effort and exits successfully regardless of the DELETE
status, so a green run does not prove that this item is resolved.

**Release acceptance:** make examples use an explicitly documented placeholder or
a moving supported tag where appropriate, verify the staging tags against Docker
Hub, and ensure the roadmap reflects the actual open work.

### F-16 — Module, vulnerability, and link documentation has drifted

**Severity:** Medium · **Area:** Documentation correctness · **Status:** Open ·
**PR 9**

- [`AGENTS.md`](../AGENTS.md) and [`architecture.md`](architecture.md) say the
  module crate has eleven modules; the config and module reference expose twelve.
- [`.trivyignore.yaml`](../.trivyignore.yaml) and [`ci-cd.md`](ci-cd.md) say
  there are two suppressions; the file contains six.
- `ci-cd.md` links to a hardening heading that no longer exists.
- `scripts/verify-design-package.sh` validates only the path portion of a local
  link and skips fragments entirely. It therefore reports success for broken
  heading links.
- The security-audit heading syntax uses an explicit `{#id}`, which
  GitHub-flavoured Markdown does not interpret as a custom heading ID, while the
  local link checker skips same-document fragments.

**Release acceptance:** correct the counts and links, remove current-version
duplication, and extend the existing link check to validate local heading
fragments with GitHub-compatible slug rules.

### F-17 — There is no current canonical reference for stable self-metrics

**Severity:** Medium · **Area:** Public metrics documentation · **Status:** Open ·
**PR 9**

[`versioning.md`](versioning.md) declares `muninn_*` names, labels, and units
stable, but the only family list is
[ADR-0012](adr/0012-self-metrics-on-health-server.md). It omits `muninn_state`,
while retaining `muninn_telegraf_restarts_total`. The latter is always zero
because muninn deliberately has no internal restart loop
([ADR-0002](adr/0002-supervisor-no-restart-loop.md)) and
`record_telegraf_restart` has no production caller
([`state.rs`](../crates/muninn-health/src/state.rs)). A process-local counter
also could not answer the ADR's own example of restarts "today", because a
process restart resets it.

**Release acceptance:** add a canonical current self-metric reference, checked
against the renderer rather than maintained by hand, covering every family and
label; and remove or redefine the restart counter before it becomes a permanent
1.0 surface.

### N-03 — Design-package verification checks only half the plugin surface

**Severity:** Medium · **Area:** Supply chain, [R5](risks.md) · **Status:** Open ·
**PR 9**

The design-package gate that checks every documented option against the pinned
Telegraf's `sample.conf` is the recorded mitigation for R5, plugin-surface drift
between minor versions. It reads
[`telegraf.reference.conf`](reference/telegraf.reference.conf), which is rendered
from the shipped example configuration and therefore contains only the plugin
blocks that example enables.

Never checked against upstream, because no shipped example produces them: the
whole `inputs.docker` block, both `inputs.exec` blocks, the output TLS options,
and the Prometheus basic-auth options. That is exactly the drift the gate exists
to catch, walking past it.

**Release acceptance:** extend the gate to cover every option the renderer can
emit. A second reference configuration that enables everything is cleaner than
distorting the example, which has a different job — being a good default.

## Lower-priority hygiene

Not release blockers on their own, but part of the 1.0 cleanup.

### F-18 — Port zero is accepted but cannot be operated

**Severity:** Low · **Area:** Configuration contract · **Status:** Open ·
**PR 4**

Tests explicitly accept port zero for health and Prometheus listeners
([`tests.rs`](../crates/muninn-core/src/config/tests.rs)). The listener then
receives an ephemeral port, but muninn neither records nor reports the chosen
address. `muninn healthcheck` reconnects to configured port zero and therefore
cannot reach the running server.

**Release acceptance:** reject port zero in operator configuration, or expose and
consistently consume the actual bound address. Test both healthcheck and
Prometheus discovery semantics.

### N-04 — `Requirements::capabilities` is declared and checked by nothing

**Severity:** Low · **Area:** Dead contract · **Status:** Open · **PR 9**

The module requirements type declares a capabilities field
([`lib.rs`](../crates/muninn-modules/src/lib.rs)). The runtime check handles host
paths, absolute paths, endpoints and the Debian-family constraint, and silently
ignores capabilities ([`runtime_check.rs`](../muninn/src/runtime_check.rs)). The
only other reference is a test asserting it is always empty. A future module
declaring a capability would be accepted with no check at all.

**Release acceptance:** check it, or remove the field. A declared requirement
nothing enforces is worse than no field, because it reads as a guarantee.

### N-05 — Dead public API and stale comments

**Severity:** Low · **Area:** Crate surface, comments · **Status:** Open ·
**PR 9**

- `muninn_health::serve` is re-exported and used only by its own test; production
  uses `bind` plus `serve_on`.
- `Telegraf::binary()` is never called.
- `exit::OK` and `exit::CLI` are never referenced — success returns
  `ExitCode::SUCCESS` directly and clap produces its own usage code.

Stale comments, in addition to the counts in [F-16](#f-16--module-vulnerability-and-link-documentation-has-drifted): the CLI module
documentation still says some commands are not implemented and names the work
package, though all of them are; and the inputs module header counts its own
files and ranks wrongly.

### Remaining hygiene

Assigned to the pull request whose surface they touch:

- **PR 3** — replace the production `expect` in the health semaphore path with
  ordinary error handling. It is the only violation of the no-panic convention
  ([AGENTS.md §7](../AGENTS.md)) outside tests in the whole tree. Also: define
  shutdown classification when a stop signal and a non-clean Telegraf exit become
  ready at the same time.
- **PR 8** — make `deny.toml`'s `unknown-git` setting deny rather than warn,
  matching its documented crates.io-only source policy; resolve or explicitly
  accept cargo-deny's duplicate `syn` versions; remove unused licence allow-list
  entries. Re-evaluate the six Trivy suppressions against the newest Telegraf
  release rather than extending their expiry.
- **PR 9** — remove the unused direct `anyhow` dependency from the binary and
  `tracing` from the modules crate; clarify that muninn itself emits the updates
  and image-updates line protocol, so the statement that it "never touches a
  metric" is not literally true.

## Deliberate non-goals

The review does not classify the following as incomplete 1.0 work:

- raw Telegraf TOML;
- configuration reload;
- Windows or macOS hosts;
- an internal unbounded restart loop;
- non-Debian-family support for the updates module.

Those are explicit product decisions. A bounded restart remains an optional
future feature only if operational experience justifies it.

## Open decisions

Three findings change the stable surface in a way that is a product decision
rather than an implementation detail. Each is settled in its own pull request,
not in advance:

1. **[F-03](#f-03--runtimetelegraf_start_timeout-is-not-implemented-as-documented)** — repair `runtime.telegraf_start_timeout`, or remove it?
   Removing it is the more honest answer if no measurable ready criterion exists.
2. **[F-17](#f-17--there-is-no-current-canonical-reference-for-stable-self-metrics)** — remove `muninn_telegraf_restarts_total`, or keep it until
   the bounded-restart question ([O3](risks.md)) is settled? Removing it now is
   cheap; after 1.0 it is a major release.
3. **[F-18](#f-18--port-zero-is-accepted-but-cannot-be-operated)** — reject port zero, or thread the bound address through?

## Suggested release order

1. Close F-01 through F-05, N-01 and N-02 before freezing stable interfaces.
2. Fix lifecycle and filesystem issues F-06 through F-10.
3. Decide the stable metric and compatibility surfaces in F-11, F-12, F-17,
   and F-18.
4. Harden the release path and complete authenticated-registry evidence in F-13
   and F-14.
5. Perform the documentation sweep F-15, F-16, N-03, N-04 and N-05, then rerun
   every repository gate and the live-container security review.

## Related

- [`roadmap.md`](roadmap.md) — canonical backlog once these findings are triaged
- [`risks.md`](risks.md) — accepted and open operational risks
- [`security-audit.md`](security-audit.md) — the earlier source review
- [`versioning.md`](versioning.md) — surfaces that become stable at 1.0
- [`testing.md`](testing.md) — required evidence and release gates
