# Release 1.0 findings

Review of the current `dev` branch for work that should be completed before the
public interfaces are stabilised as 1.0. The review focused on incomplete
features, incomplete or drifting documentation, and security weaknesses.

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

## Blockers before 1.0

### F-01 — Secret values can still reach child and validator diagnostics

**Severity:** High  
**Area:** Security, secrets  
**Status:** Open

`Redactor` deliberately excludes values shorter than eight bytes, while config
loading accepts such values without a warning or rejection
([`secret.rs`](crates/muninn-core/src/secret.rs#L105)). A short but valid Basic
Auth password or token printed by Telegraf therefore passes through unchanged.

The separate `telegraf config check` path has no redactor at all. On failure it
copies Telegraf's stdout and stderr directly into `MuninnError`
([`validator.rs`](crates/muninn-telegraf/src/validator.rs#L50)). The generated
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

**Severity:** Medium  
**Area:** Security, diagnostics  
**Status:** Open; currently documented as fixed

The permission check added for security finding M-01 emits its warning through
`tracing::warn!` ([`secret.rs`](crates/muninn-core/src/secret.rs#L211)). Secrets
are loaded before `run` initialises the tracing subscriber
([`main.rs`](muninn/src/main.rs#L124), [`main.rs`](muninn/src/main.rs#L425)). The
other config-reading commands never initialise that subscriber. The event is
therefore discarded in the paths where an operator is meant to see it.

The two Unix tests only prove that a loosely permissioned secret still loads;
they do not capture or assert the warning. As a result, the changelog and
[`docs/security-audit.md`](docs/security-audit.md) overstate the fix.

**Release acceptance:** return the permission finding through the same warning
channel as semantic configuration warnings, print it on stderr for every
config-reading command, and add a Linux test that asserts the actual diagnostic.

### F-03 — `runtime.telegraf_start_timeout` is not implemented as documented

**Severity:** High  
**Area:** Stable configuration and exit-code contract  
**Status:** Open

The configuration reference says the value is how long Telegraf may take to
come up before muninn exits with code 21
([`configuration.md`](docs/configuration.md#runtime-telegraf_start_timeout)).
The implementation sleeps for at most 500 milliseconds and treats a process
that is merely still alive as ready
([`supervisor.rs`](muninn/src/supervisor.rs#L399)). Values above 500 milliseconds
have no effect. A failure after that window becomes an operational crash with
exit 22 rather than a start failure with exit 21.

**Release acceptance:** implement a measurable ready/start criterion governed
by the configured timeout, or remove and replace the setting before the config
and exit-code surfaces become stable. Lifecycle tests must cover a delayed
success, a delayed failure, and the exact timeout boundary.

### F-04 — Ubuntu security updates can be reported as zero incorrectly

**Severity:** High  
**Area:** Feature correctness, security metrics  
**Status:** Known limit, tracked as R8

The updates module classifies the security subset from the origin printed for
the selected candidate. Ubuntu may copy the same candidate into its updates
pocket, causing a host with security-relevant updates to produce a security
count of zero. The limitation and measured example are already recorded in
[`docs/risks.md`](docs/risks.md#r8--the-security-subset-under-reports-on-ubuntu).

This is unsafe to freeze as stable 1.0 metric semantics: a plausible false zero
can suppress an alert.

**Release acceptance:** classify the candidate version against all applicable
security origins, amend ADR-0009, and add Debian and Ubuntu ground-truth cells
that independently establish both the total and security subset.

### F-05 — Health-server failure and startup-state contracts do not match code

**Severity:** High  
**Area:** Health API, lifecycle, documentation  
**Status:** Open

The health task discards the result of `serve_on`, and its join result is also
ignored ([`main.rs`](muninn/src/main.rs#L459)). A task failure or panic can drop
the listener while the supervisor continues. The operational contract instead
says a permanently failed health server exits with code 30
([`supervision.md`](docs/supervision.md#fatal-during-operation)).

Two published states are unreachable in production. `HealthState` is created
only after configuration load and validation, so `LoadingConfiguration` and
`ValidatingConfiguration` can never be served
([`state.rs`](crates/muninn-health/src/state.rs#L30)). Startup failures also do
not generally transition to `Failed`. Meanwhile the documentation says startup
steps one through nine have no listener, even though the health listener is
bound and serving before the runtime check, rendering, and Telegraf validation.

**Release acceptance:** choose and implement one coherent lifecycle. Monitor the
health task as part of the supervisor, make its permanent failure fatal, and
either make every public state reachable with tested transitions or remove the
unreachable states and update the diagrams and endpoint contract.

## High-priority work

### F-06 — The startup updates check can block signal handling indefinitely

**Severity:** High  
**Area:** Shutdown reliability  
**Status:** Open

After reporting readiness, muninn waits for the initial updates check before it
enters the `select!` that supervises Telegraf and signals
([`supervisor.rs`](muninn/src/supervisor.rs#L163)). The check executes
`apt-get ... .output()` without a timeout
([`debian.rs`](crates/muninn-modules/src/updates/debian.rs#L468)). A signal is
buffered but not acted on until apt returns. A stuck apt process can therefore
make `docker stop` exceed the container grace period while readiness had already
reported success.

**Release acceptance:** bound and terminate the subprocess, begin signal and
child supervision before optional self-checks, and add a lifecycle test with a
hung helper and SIGTERM.

### F-07 — A saturated health listener delays shutdown

**Severity:** Medium  
**Area:** Availability, container shutdown  
**Status:** Open; source-derived, live reproduction pending

The server awaits a semaphore permit before entering the `select!` that observes
shutdown ([`serve.rs`](crates/muninn-health/src/serve.rs#L93)). At the connection
limit, shutdown is invisible until a permit is released. With the configured
Telegraf grace period, header timeout, and connection drain, the total can exceed
the shipped Compose stop timeout.

Persistent HTTP/1.1 connections can also keep all permits occupied with a small
request below every header deadline, bounding memory but still starving genuine
probes.

**Release acceptance:** include permit acquisition in shutdown selection,
define the keep-alive policy, and add saturated-capacity tests for shutdown and
probe availability. Reproduce the connection flood against the hardened image.

### F-08 — `muninn healthcheck` reloads the whole configuration and all secrets

**Severity:** Medium  
**Area:** Health semantics  
**Status:** Open

The comment says the command reads only the health address and deliberately
does not validate the configuration. It actually calls `load_and_resolve`
([`main.rs`](muninn/src/main.rs#L290)). A config edit or temporarily missing
secret after startup can therefore mark a healthy running process unhealthy.
An orchestrator may restart it into the broken configuration, turning a
diagnostic mismatch into an outage.

**Release acceptance:** make the probe depend only on the address used by the
running instance, document how that address is obtained, and test that unrelated
config and secret changes cannot alter the health verdict.

### F-09 — Secret-bearing files and runtime probes follow predictable paths

**Severity:** Medium  
**Area:** Filesystem security  
**Status:** Open

The generated-config writer uses `create + truncate` on the final path and
follows an existing symlink
([`generated_config.rs`](muninn/src/generated_config.rs#L30)). The default
container directory is protected, but the path is configurable and direct-host
use is supported.

`validate --with-telegraf` uses a predictable `telegraf.check.<pid>.conf` name
when the configured parent exists, then ignores deletion failure
([`main.rs`](muninn/src/main.rs#L169)). `check-runtime` similarly writes and
deletes a fixed `.muninn-write-probe`, which can truncate an existing file or
follow a symlink ([`runtime_check.rs`](muninn/src/runtime_check.rs#L306)).

**Release acceptance:** create secret-bearing files exclusively under random
names, keep mode `0600` from creation, replace the final file atomically without
following a symlink, and treat failed secret-file cleanup as an error.

### F-10 — Telegraf output forwarders are detached and not drained

**Severity:** Medium  
**Area:** Diagnostics, supervision  
**Status:** Open

The stdout and stderr tasks are spawned without retaining their join handles
([`process.rs`](crates/muninn-telegraf/src/process.rs#L136)). Child exit can be
observed before those tasks have drained the final buffered lines, and the Tokio
runtime is then dropped. The diagnostic immediately preceding a crash can be
lost even though the troubleshooting guide tells operators to rely on it.

The existing test named `a_secret_in_child_output_is_masked_before_it_is_logged`
only calls `Redactor::apply`; it never exercises `forward` or a logging
subscriber ([`process.rs`](crates/muninn-telegraf/src/process.rs#L355)).

**Release acceptance:** retain and join both reader tasks on every exit path,
bound a pathological drain, and test a real child that emits final stdout and
stderr containing a secret immediately before exit.

### F-11 — `image_updates` has two conflicting meanings of success

**Severity:** Medium  
**Area:** Stable metric and status semantics  
**Status:** Open design decision

The supervisor records the module as successful whenever container enumeration
succeeded ([`supervisor.rs`](muninn/src/supervisor.rs#L315)).
`daemon_succeeded()` ignores every per-container outcome
([`check.rs`](crates/muninn-modules/src/image_updates/check.rs#L177)). `/status`
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

**Severity:** Medium  
**Area:** Compatibility, supply chain  
**Status:** Open

The supported floor is `rust-version` in [`Cargo.toml`](Cargo.toml). The Docker
builder is newer than that floor ([`Dockerfile`](Dockerfile#L52)), while CI runs
only floating stable and beta ([`ci.yml`](.github/workflows/ci.yml#L92)). Resolver
3 can choose dependencies compatible with the declared floor, but neither the
project source nor its tests are compiled by the floor compiler.

**Release acceptance:** add a required MSRV build or check job using the
authoritative manifest value, and keep stable/beta as the current and forward
compatibility jobs.

### F-13 — Release credentials remain available for too much workflow code

**Severity:** Medium  
**Area:** CI/CD security  
**Status:** Open hardening

The publish checkout persists `RELEASE_PAT` before repository scripts and
several third-party actions execute ([`ci.yml`](.github/workflows/ci.yml#L696)).
The comment that nothing between checkout and push executes third-party code is
not true for the current job.

The release workflows import the GPG private key and may write its passphrase
file before running the repository-controlled version and changelog transforms
([`release.yml`](.github/workflows/release.yml#L361)). Both automation paths
also force-push their reusable branch names.

**Release acceptance:** use `persist-credentials: false`, make the PAT available
only to the final tag or branch push, import and remove signing material
immediately around the commit, and replace unconditional force pushes with a
verified `--force-with-lease` flow.

### F-14 — Authenticated registry behaviour has no system evidence

**Severity:** Medium  
**Area:** Incomplete feature verification  
**Status:** Already on the roadmap as R9

`image_updates` is tested against public registries. There is no repository
evidence for a registry the Docker daemon accesses with stored credentials, an
expired credential, or a `401` response. All currently collapse into
`distribution_query_failed`; locally built images can reach the same reason.

**Release acceptance:** extend `scripts/image-updates-test.sh` with a local
authenticated registry, valid and expired credentials, and recorded expected
reasons before deciding whether the public reason vocabulary needs to change.

## Documentation work before 1.0

### F-15 — The roadmap and examples describe old release state

**Severity:** Medium  
**Area:** Release documentation  
**Status:** Open

[`docs/roadmap.md`](docs/roadmap.md) still names the first release and its old
development tag as current. Several README, hardening, host-mount, release, and
troubleshooting examples similarly pin historical image versions even though
the repository rule says the authoritative version should not be repeated in
prose.

The roadmap also says staging-tag deletion currently receives HTTP 403. The CI
step is deliberately best-effort and exits successfully regardless of the
DELETE status, so a green run does not prove that this item is resolved.

**Release acceptance:** make examples use an explicitly documented placeholder
or a moving supported tag where appropriate, verify the staging tags against
Docker Hub, and ensure the roadmap reflects the actual open work.

### F-16 — Module, vulnerability, and link documentation has drifted

**Severity:** Medium  
**Area:** Documentation correctness  
**Status:** Open

- [`AGENTS.md`](AGENTS.md) and
  [`docs/architecture.md`](docs/architecture.md#crates) say the module crate has
  eleven modules; the config and module reference expose twelve.
- [`.trivyignore.yaml`](.trivyignore.yaml) and
  [`docs/ci-cd.md`](docs/ci-cd.md#suppressed-image-findings) say there are two
  suppressions; the file contains six.
- `ci-cd.md` links to an old hardening heading that no longer exists.
- `scripts/verify-design-package.sh` validates only the path portion of a local
  link and skips fragments entirely
  ([`verify-design-package.sh`](scripts/verify-design-package.sh#L175)). It
  therefore reports success for broken heading links.
- The security-audit heading syntax uses `{#m-01}`, which GitHub-flavoured
  Markdown does not interpret as a custom heading ID, while the local link
  checker skips same-document fragments.

**Release acceptance:** correct the counts and links, remove current-version
duplication, and extend the existing Python link check to validate local heading
fragments with GitHub-compatible slug rules.

### F-17 — There is no current canonical reference for stable self-metrics

**Severity:** Medium  
**Area:** Public metrics documentation  
**Status:** Open

The versioning policy declares `muninn_*` names, labels, and units stable, but
the only family list is ADR-0012. It omits `muninn_state`, while retaining
`muninn_telegraf_restarts_total`
([`0012-self-metrics-on-health-server.md`](docs/adr/0012-self-metrics-on-health-server.md#decision)).
The latter is always zero because muninn deliberately has no internal restart
loop; `record_telegraf_restart` has no production caller
([`state.rs`](crates/muninn-health/src/state.rs#L183)). A process-local counter
also could not answer the ADR's example of restarts "today" because the process
restart resets it.

**Release acceptance:** add a canonical current self-metric reference generated
or checked against the renderer, include every family and label, and remove or
redefine the restart counter before it becomes a permanent 1.0 surface.

### F-18 — Port zero is accepted but cannot be operated

**Severity:** Low  
**Area:** Configuration contract  
**Status:** Open

Tests explicitly accept port zero for health and Prometheus listeners
([`config/tests.rs`](crates/muninn-core/src/config/tests.rs#L938)). The listener
then receives an ephemeral port, but muninn neither records nor reports the
chosen address. `muninn healthcheck` reconnects to configured port zero and
therefore cannot reach the running server.

**Release acceptance:** reject port zero in operator configuration, or expose
and consistently consume the actual bound address. Test both healthcheck and
Prometheus discovery semantics.

## Lower-priority hygiene

These are not release blockers on their own, but should be handled during the
1.0 cleanup:

- remove the unused direct `anyhow` dependency from
  [`muninn/Cargo.toml`](muninn/Cargo.toml) and `tracing` from
  [`crates/muninn-modules/Cargo.toml`](crates/muninn-modules/Cargo.toml);
- make `deny.toml`'s `unknown-git` setting deny rather than warn, matching its
  documented crates.io-only source policy;
- resolve or explicitly accept cargo-deny's duplicate `syn` versions and remove
  unused licence allow-list entries;
- remove stale comments that describe implemented CLI commands as future work;
- replace the production `expect` in the health semaphore path with ordinary
  error handling, in line with the repository's no-panic convention;
- clarify that muninn itself emits the updates and image-updates line protocol,
  so the statement that it "never touches a metric" is not literally true;
- define shutdown classification when a stop signal and a non-clean Telegraf
  exit become ready at the same time.

## Deliberate non-goals

The review does not classify the following as incomplete 1.0 work:

- raw Telegraf TOML;
- configuration reload;
- Windows or macOS hosts;
- an internal unbounded restart loop;
- non-Debian-family support for the updates module.

Those are explicit product decisions. A bounded restart remains an optional
future feature only if operational experience justifies it.

## Suggested release order

1. Close F-01 through F-05 before freezing stable interfaces.
2. Fix lifecycle and filesystem issues F-06 through F-10.
3. Decide the stable metric and compatibility surfaces in F-11, F-12, F-17,
   and F-18.
4. Harden the release path and complete authenticated-registry evidence in F-13
   and F-14.
5. Perform the documentation sweep F-15 and F-16, then rerun every repository
   gate and the live-container security review.

## Related

- [`docs/roadmap.md`](docs/roadmap.md) — canonical backlog once these findings
  are triaged
- [`docs/risks.md`](docs/risks.md) — accepted and open operational risks
- [`docs/security-audit.md`](docs/security-audit.md) — the earlier source review
- [`docs/versioning.md`](docs/versioning.md) — surfaces that become stable at 1.0
- [`docs/testing.md`](docs/testing.md) — required evidence and release gates
