# Security audit

muninn.io's own reviews, newest first. Each pass is kept as it was written,
amendments included: a review that turns out to have been wrong is the most
useful thing in this file, and rewriting it would delete the evidence for that.

| Pass | Scope | Findings |
|---|---|---|
| [2026-08-12](#pass-2026-08-12--the-pipeline-and-the-10-surface) | The CI/CD pipeline, and what moved in the code since the last pass | M-02 … M-06 |
| [2026-08-08](#pass-2026-08-08--the-first-code-review) | muninn's own code: secrets, rendering, the Docker client, the health listener | M-01 |
| 2026-08-02 | huginn.io's audit, which prompted the first pass here | — |

Findings are numbered `M-nn` so they cannot be confused with huginn's `F-nn`.
How to report a vulnerability is [`SECURITY.md`](SECURITY.md).

---

## Pass 2026-08-12 — the pipeline, and the 1.0 surface

Prompted by two sentences the previous pass wrote about itself. It excluded
**"the CI pipeline (covered in `ci-cd.md`)"** from its scope, and that exclusion
had never been lifted: `ci-cd.md` describes what the pipeline does, which is not
a review of what it exposes. And it recommended re-running **"after any change
to the renderer, the Docker client, or the secret path"** — since 2026-08-08 all
three moved, and `modules.image_updates.registry_auth` added a credential that
did not exist when the last pass called the secret path sound.

### Scope and method

**Read, and in two places run.** The previous pass named "read, not run" as its
own weakness. This one measures where measuring was possible: two findings below
are demonstrated by a test that fails without the fix, and the pipeline changes
are exercised by the run that carries them.

| | |
|---|---|
| **Reviewed** | all six workflows in `.github/workflows/` — **and not `.github/dependabot.yml`, which was the gap that produced M-04**; — trigger surface, `permissions` scope, which step can reach which secret, expression handling, action pinning, artefact flow, the auto-merge path; and on the code side `registry_auth` end to end, redactor coverage, and the credential-handling claims the workflows make about themselves |
| **Not reviewed** | Telegraf, the base image's packages, the six suppressed Trivy findings (all tracked in [`hardening.md`](hardening.md) with expiries), the renderer and the health listener — unchanged since the last pass and re-checked only where 1.0 touched them |
| **Measured** | M-02 by a test that fails against the previous code; M-03 by the checksum it introduces; `auto-pr.yml`'s handling of a hostile branch name by pushing one; the rest reasoned from source |
| **Not measured** | a real fork pull request attempting to read a secret. That needs a second account, and the finding below is reasoned from configuration and from GitHub's own rule rather than demonstrated. Stated here rather than left to be assumed |
| **Commit** | `dev` at `087d15c`, plus the fixes in this pull request |

### Findings

| | Severity | Area | Summary | Status |
|---|---|---|---|---|
| [M-02](#m-02--a-registry-password-was-outside-the-redactor) | Low | `muninn-core`, `muninn` | Registry passwords never reached the redactor, and the test written to prevent exactly that could not see them | **Fixed** here |
| [M-03](#m-03--nothing-checked-that-the-published-bytes-were-the-built-bytes) | Low | `ci.yml` | The pipeline's central claim — scanned, tested and published are the same bytes — rested on the artefact store, with no checksum recorded or compared | **Fixed** here |
| [M-04](#m-04--a-transitive-version-can-be-younger-than-the-cooldown-that-gated-its-parent) | Low | `dependabot.yml` | A transitive crate can be younger than the cooldown that gated the direct one | **Open**, and narrower than published — the finding as first written was wrong, see it |
| [M-05](#m-05--write-permissions-were-granted-to-a-whole-workflow) | Low | `dependabot-auto-merge.yml` | `contents: write` and `pull-requests: write` at workflow scope, inherited by any job added later | **Fixed** here |
| [M-06](#m-06--the-push-credential-passed-through-an-external-processs-argv) | Low | `ci.yml` | The tag-push credential was handed to `git config` as an argument, in the step whose own comment rejects argv | **Fixed** here |

Five findings, all low once M-04 was corrected, and four of them closed by a few
lines each. That is not a claim that the pipeline is secure — it is what a first
review of a surface nobody had reviewed produced, and the section on what held is
again the more useful half.

**One of the five was published wrong**, and that is the most useful thing in
this pass. M-04 asserted an exposure that a control already closed, because the
review read the six workflow files and not `.github/dependabot.yml`, where the
control lives. The corrected finding is narrower and the original text is kept
under it. This is the same lesson the 2026-08-08 pass recorded about itself, in
the other direction: "we looked and it holds" can be shown wrong later, and so
can "we looked and it does not".

**One candidate was withdrawn.** Scoping suspected the GPG passphrase file was
written at the default umask and would land `0644`. It is not: `release.yml`
sets `umask 077` immediately before writing it, and the wrapper script beside it
inherits the same. Recorded because a withdrawn finding is evidence too — the
grep that suggested it did not show the line above.

#### M-02 — a registry password was outside the redactor

**Severity:** Low · **Status:** Fixed in this pass

`Config::redactor()` builds the set of values scrubbed out of Telegraf's stdout
and stderr before muninn re-emits them. It collected two: the InfluxDB token and
the Prometheus basic-auth password. `modules.image_updates.registry_auth`
passwords were not among them.

**Why adding a line to `redactor()` would not have fixed it**, and why this is
more interesting than an omission. The normalised configuration does not *hold*
a registry password. It holds `password_file` — a path — and the value is read
much later, by `registry_auth::resolve`, in the code that builds the
`X-Registry-Auth` header. There was no value in `redactor()`'s reach to add. The
credential had left the shape every previous secret had.

**The path that matters.** `image_updates` runs through `inputs.exec`, so
`muninn image-check`'s stderr *is* a Telegraf log line, which is exactly the text
this redactor exists to scrub.

**Not a live leak.** `Secret`'s `Debug` and `Display` render `***`, and errors
name paths rather than contents, so nothing prints the value today. It needs a
second failure, which is M-01's shape — and M-01's lesson is that the second
failure is the one nobody plans.

**The test could not have caught it, while reading as though it would.**
`every_resolved_secret_is_redactable` builds a configuration with every
credential set and asserts each disappears. A credential stored as a path never
appears in a configuration that test can build, so no assertion in it would ever
have failed. The doc comment above `redactor()` claimed the test "walks a
configuration with every credential set" — true of what it can see, and read as
a guarantee about everything. That sentence was the finding as much as the gap
was.

**Fixed** by assembling the redactor where the whole credential set is known:
`muninn::supervisor::output_redactor` merges the configuration-held secrets with
the resolved registry passwords through the new `Redactor::extended_with`. The
guarantee it can offer is now stated honestly in both doc comments — one says
what it covers, the other says what it cannot.

**Measured, not argued.** `a_registry_password_is_redacted_out_of_telegraf_output`
fails against `config.redactor()` and passes against `output_redactor`, which
was checked both ways rather than assumed.

#### M-03 — nothing checked that the published bytes were the built bytes

**Severity:** Low · **Status:** Fixed in this pass

`AGENTS.md`, [`ci-cd.md`](ci-cd.md) and `ci.yml`'s own header all state the
property the pipeline is built around: the image is built once per architecture
and every later job consumes that same artefact, so the bytes scanned, tested
and published are byte-identical.

Nothing verified it. `build` uploaded `image.tar`; `scan`, `integration`,
`updates` and `push` downloaded it; `push` handed it to skopeo. No checksum was
recorded on the way out and none was compared on the way in. The guarantee
rested entirely on GitHub's artefact store behaving — a reasonable assumption,
and an assumption, at the one point in this project where a claim of identity is
load-bearing.

**Exploitability is low and beside the point.** Artefact names are immutable
within a run, so substituting one needs a compromised job — and a compromised
`build` would forge the checksum too. What the check actually buys is that the
sentence stops being an assertion: corruption, truncation, a future change in
how artefacts are stored, or a job that quietly rebuilds instead of loading now
fail loudly rather than silently changing what "scanned" refers to.

**Fixed** by recording `sha256sum image.tar` in `build` and verifying it in all
four consumers. Not only in `push`: "scanned, tested and published are
identical" is several claims, and checking once at the end would leave the rest
still assumed.

#### M-04 — a transitive version can be younger than the cooldown that gated its parent

**Severity:** Low · **Status:** Open — narrow, and named rather than closed

**This finding was published wrong, and the original text is kept below.** It
claimed that a version published an hour ago could reach `dev` and therefore the
`:dev` image unattended. It cannot. `.github/dependabot.yml` carries a
`cooldown: default-days: 3` on all three ecosystems, so Dependabot does not
*propose* a release for three days — and that is a stronger control than
anything this pass could have recommended, because a pull request that is never
opened is also a dependency CI never compiles. The reviewer read the six
workflows and not the Dependabot configuration, which is where the control
lives; [`ci-cd.md`](ci-cd.md) states it in plain text, one sentence away from
pages that were open at the time.

**What survives, and it is much smaller.** The cooldown gates the dependency
Dependabot proposes, and this repository deliberately keeps Dependabot's scope
at direct dependencies. A direct crate three days old can still resolve a
**transitive** version published an hour ago, and nothing looks at that: not the
cooldown, which never considered it, and not CI, whose instruments all search
for something already known. Measured on the merged #62 — Dependabot named
`clap`, and `Cargo.lock` also gained `clap_builder`.

**And a cooldown is not a review.** `dependabot.yml` says so itself: an attacker
who waits it out is unaffected. That is accepted, and it is accepted knowingly,
which is the difference this finding is now recording.

**Left open rather than closed**, deliberately. A merge-time check over every new
`Cargo.lock` entry was written and discarded: it duplicated the native cooldown
for the overwhelming majority of cases, could not protect the runner the way the
native one does, and cost a scheduled sweep, a label and a wider token scope for
a residual this narrow. Naming the gap is worth more here than a second
mechanism that has to be kept in step with the first.

<details>
<summary>What this finding said when it was published, 2026-08-12</summary>

**Severity:** Medium · **Status:** Open — a policy decision, deliberately left

`dependabot-auto-merge.yml` queues auto-merge for `semver-patch` **and**
`semver-minor` updates. The reasoning is written down beside it, and it is about
compatibility: "CI green does not prove a major has no breaking runtime
behaviour the tests miss."

That argument is sound and it answers a different question. The risk that makes
unattended dependency merges dangerous is not breakage, it is a compromised
release of a package that was fine yesterday — the shape of the npm and xz
incidents. Against that, everything the pipeline checks is the wrong instrument:
`cargo deny` refuses *known* advisories, Trivy scans for *known* CVEs, and the
tests confirm the code still does what it did. A fresh backdoor in a minor
release passes all three.

The blast radius is what raises it above Low. Auto-merge targets `dev`, and a
push to `dev` publishes `:dev` and `:x.y.z-dev` to Docker Hub and ghcr. So the
path runs from an unreviewed dependency diff to an image an operator can pull,
with no human in it.

**Not fixed, because the fix is a judgement about how much review capacity you
want to spend.** Three options, in the order I would consider them:

1. **Restrict auto-merge to `semver-patch`.** One condition removed. Minors then
   wait for a glance, which for a solo project is a few pull requests a month.
2. **Keep minors, add a cooling-off period.** A compromised release is usually
   yanked within days; merging nothing younger than, say, seven days converts
   most of this risk into a delay. Costs a date check against the registry.
3. **Accept it, and write down that it is accepted** — with the reasoning being
   about supply chain rather than about compatibility, so the next reader is not
   told that CI green covers this.

Doing nothing silently is the one outcome this finding exists to prevent.

</details>

#### M-05 — write permissions were granted to a whole workflow

**Severity:** Low · **Status:** Fixed in this pass

`dependabot-auto-merge.yml` declared `contents: write` and `pull-requests: write`
at **workflow** scope. It has one job, so the effect was identical today — and a
second job added to that file would have inherited both without anyone deciding
it.

`release-dispatch.yml` already states the rule this breaks, in a comment on its
own job-scoped block: "declared on the job rather than the workflow so adding a
second job cannot inherit write." The convention existed; one file did not
follow it.

**Fixed** by `permissions: {}` at workflow scope and the grant on the job.

#### M-06 — the push credential passed through an external process's argv

**Severity:** Low · **Status:** Fixed in this pass

`ci.yml`'s "Create git tag" step carries a five-line comment explaining that the
token is kept out of the remote URL because "a URL is argv, and argv is readable
by anything else on the machine". It then wrote the credential with
`git config --local http.https://github.com/.extraheader "AUTHORIZATION: basic …"`
— an external binary, taking the base64-encoded token as an argument, and
leaving it in `.git/config` until a trap removed it.

**Ambient rather than novel**, and worth saying so plainly: `actions/checkout`
configures its own token exactly this way — visible in any run log as
`git config --file …/git-credentials-….config http.https://github.com/.extraheader
AUTHORIZATION: basic ***` — so the exposure exists on every checkout in every
repository that uses it. Note the difference, though: checkout writes to a
throwaway config file rather than to `.git/config`, so it leaves nothing behind
in the workspace. This step did. What made it a finding is that it is the one
occurrence the repository controls, in the step that argues against it.

**Fixed** by passing the header through `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_0`
/ `GIT_CONFIG_VALUE_0` on the `git push` invocation itself. `/proc/<pid>/environ`
is readable only by the owning user; `/proc/<pid>/cmdline` is readable by
everyone. Nothing is written to disk, so the trap and the `--unset-all` are gone
with it — one fewer cleanup path that has to run.

### Checked, and found sound

The pipeline half of this pass mostly produced these, and they are what make the
next review cheaper.

**No expression is interpolated into a shell.** Every `${{ … }}` that carries a
value reaches its script through `env:` first — including `github.actor`,
`github.event.pull_request.html_url`, `inputs.tag` and `matrix.platform`. This
is the injection GitHub Actions is most commonly broken by, and there is not one
instance of it. `ci.yml` even carries a comment noting that the syntax is
substituted inside comments too, which is the level of care this needs.

**A hostile branch name is data, and this one was measured rather than read.**
`auto-pr.yml` runs on every push to a non-protected branch, holds
`contents: write`, and *deletes* any branch that does not match the naming
convention — passing the name to `gh api -X DELETE`. Git refs forbid spaces,
`..` and `~^:?*[`, which leaves `` $ ` ' " ; & | ( ) < > ! # `` as the usable
set. Pushing ``wip/a$(id)b`id`c`` produced no `uid=` anywhere in the run: the
name appears literally in `git fetch`, `git checkout`, the workflow's own
`grep`, the warning it printed, and the delete call, which succeeded and removed
the branch. Nothing expanded, nothing executed, no pull request was opened.
Run `31607472014`.

**Every action is pinned to a 40-character commit SHA**, across all six
workflows, with the human-readable tag beside it in a comment. The two
references Dependabot cannot update — the Semgrep container and the actionlint
image, both by digest — are marked as needing a manual bump where they are used.

**Neither `pull_request_target` nor `workflow_run` appears anywhere.** Those are
the two triggers that run with the base repository's secrets against a
contributor's code, and the pipeline uses neither.

**A fork pull request cannot reach a secret.** `push` and `publish` are
`push`-event only, so the Docker Hub and release credentials are unreachable
from any pull request; the SARIF upload guards separately on
`head.repo.fork != true`. GitHub additionally forces a read-only token for
`pull_request` from a fork regardless of the `permissions:` block. Reasoned, not
measured — see Scope above.

**Credentials go to stdin or a config file, never to a command line.** skopeo
logs in with `--password-stdin` under a 0600 `REGISTRY_AUTH_FILE`; the Docker Hub
JWT is built by `jq` and posted with `curl --data @-`; the session token goes
through `curl -K -`. The one violation of this rule was M-06.

**The GPG signing material is handled correctly end to end.** The key is
imported through a pipe rather than a file, the passphrase is written under
`umask 077`, gpg reads it with `--passphrase-file` rather than an argument, the
signing identity is parsed out of the key's own UID and rejected if it is not
`Name <email>`, and both the passphrase and the secret key are deleted in the
job's last step — which is where `a4c3065` moved it after v1.0.0 signed nothing.

**`prepare-dev` keeps its checkout credential, and that is safe** — the one
place in the repository where `persist-credentials` is left on. The justification
is that no third-party code runs after it, and unlike the same argument in
`publish` (which was wrong, and became F-13) this one holds: the job runs `gpg`,
`git`, `gh` and one repository script. That script,
`scripts/set-workspace-version.sh`, edits `Cargo.lock` by hand and says in a
comment that it avoids `cargo update` precisely because "no job runs cargo with a
write token". The rule is enforced by the script, not merely stated.

**`release-dispatch.yml`'s owner check is real.** `workflow_dispatch` already
requires write access; the comparison of `github.actor` against
`github.repository_owner` narrows it further, to one person, and runs before the
checkout.

**The three fan-in gates cannot be satisfied by absence.** Each carries
`if: always()` and treats anything that is not `success` as failure, so a
dependency that is skipped fails the gate rather than silently satisfying it —
which is what a required check that is merely *not run* would otherwise do.

### Accepted risk and residuals

Decisions and known limits, not oversights.

1. **A job in no gate still blocks nothing.** The gates replaced "remember to
   update the ruleset" with "remember to add the job to a gate's `needs`", which
   is a much smaller surface — `build` and `push` now depend on their gate, so a
   source job left out of `Source gate` at least fails to order anything — but it
   is not zero. Adding a job means adding it to a gate.
2. **`secrets.RELEASE_PAT || secrets.GITHUB_TOKEN` degrades silently.** Without
   the PAT the tag is still pushed, by a different identity, and `release.yml`
   never fires because of GitHub's recursion guard. That is an availability
   behaviour with a documented recovery path rather than an authorisation one —
   but nothing announces which of the two identities acted.
3. **The four accepted risks from the 2026-08-08 pass are unchanged**: the
   `/:/hostfs:ro` mount, the Docker socket, the shell and package manager in the
   runtime image, and the six suppressed image findings.

### Recommendations

1. **Read the configuration, not only the workflows, next time.** M-04 was
   published claiming an exposure that `.github/dependabot.yml` had closed years
   earlier. A pipeline's controls are not all in `.github/workflows/`, and a
   review that treats a directory as the boundary will keep finding this.
2. **Measure the fork case**, if a second account is ever convenient. Everything
   else in this pass is either measured or reasoned from code that cannot move
   without CI noticing; that one rests on GitHub's behaviour.
3. **Re-run this pass after any change to the workflows' credential handling**,
   the same way the last pass asked for the renderer, the Docker client and the
   secret path — those three are exactly what produced M-02.
4. **Give huginn.io M-02, M-03, M-05 and M-06.** Its pipeline has the same shape
   and its `dependabot-auto-merge.yml` the same permissions block. The last
   pass's first recommendation was the same sentence about M-01, and it is worth
   noting that a fix landing in one project remains half a fix.

---

## Pass 2026-08-08 — the first code review


A review of muninn.io's own code and configuration, prompted by huginn.io's
audit of 2026-08-02: the two projects share conventions, and one of that audit's
findings turned out to apply here and had never been looked for.

### Scope and method

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

### Findings

| | Severity | Area | Summary | Status |
|---|---|---|---|---|
| [M-01](#m-01--secret-file-permissions-are-neither-checked-nor-reported) | Low | `muninn-core` | Secret file permissions are neither checked nor reported | **Fixed** — warns when a secret is readable beyond its owner. The first version of the fix was discarded before an operator could see it; see the finding |

One finding. That is not a claim that muninn is secure; it is what a source
review of these surfaces produced, and the section below on what was checked and
found sound is the more useful half of the document.

**A later pass found two more**, both on surfaces this review looked at and
called sound. `Redactor` skipped values shorter than eight bytes while loading
accepted them, and `telegraf config check`'s output reached `MuninnError`
through no redactor at all. Both were closed in 1.0.0, and the paragraph below
headed *Telegraf's output is redacted before muninn re-emits it* was true about
the child process and silently not true about the validator. That is the useful lesson of this
document: "we looked and it holds" is worth recording precisely because it can
be shown wrong later.

#### M-01 — Secret file permissions are neither checked nor reported

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
names the path, never the contents.

**The first attempt at this fix did not work, and this document said it did.**
The warning was emitted with `tracing::warn!` — but secrets are read during
validation, which runs *before* the tracing subscriber exists, because the log
level to initialise it with comes from the configuration being validated. The
commands that read a configuration without running never initialise a subscriber
at all. So the event was discarded on every path where an operator was meant to
see it, the two tests asserted only that a loose mode is not fatal, and this
page and the changelog both called it closed. Closed in 1.0.0 by returning the
finding through the same channel as every other configuration warning, which the
caller prints on stderr once it can. The test now asserts the diagnostic, not the absence of a
failure.

Unix only — mode bits are the check, and there is nothing equivalent to look at
elsewhere. Which means the code path and its tests are **compiled out on the
maintainer's Windows machine and first exercised by CI on Linux**; that is a
weaker verification than the rest of this document and is worth knowing. It is
also how the first version of the fix passed review.

### Checked, and found sound

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
the fact — the file is never briefly world-readable. It holds resolved secrets
by design, lives on a tmpfs, and is never persisted
([ADR-0003](adr/0003-ephemeral-generated-config.md)).

*Amended:* the mode was right and the *path* was not. The writer opened the
target with `create(true).truncate(true)`, which follows a symlink — so anything
able to place one where muninn was about to write could have redirected a file
containing resolved credentials, and a reader could observe a half-written
configuration. Not reachable in the shipped deployment, where `/run/muninn` is
0700, but the path is configurable and `render-config --output` writes wherever
it is told. Closed as F-09: the contents go to a fresh `mkstemp` file in the
same directory and are renamed onto the target, which replaces a link rather
than following it and is atomic. The same finding covered two other predictable
paths — the `validate --with-telegraf` scratch file and the `check-runtime`
write probe — both now `mkstemp` names, and a scratch file that cannot be
removed is now an error rather than an ignored result.

**Telegraf's output is redacted before muninn re-emits it.** Both stdout and
stderr pass through `Redactor`. This is the gap that type-level redaction cannot
close: `Secret`'s `Debug` and `Display` protect everything muninn formats
itself, and nothing at all about what a child process writes.

*Amended:* this was true of the running child and not of `telegraf config
check`, which is the same child saying the same kind of thing about the same
file, and whose output went into `MuninnError` unfiltered. Closed as part of
F-01; the redactor is now threaded through `check_config` too, and the
configured minimum credential length is what makes the redactor able to act at
all.
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

### Accepted risk

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

### Recommendations

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

### Related

- [`hardening.md`](hardening.md) — the posture this reviewed
- [`risks.md`](risks.md) — the open risks, including those accepted above
- [`SECURITY.md`](SECURITY.md) — how to report a vulnerability
- huginn.io's `docs/security-audit.md` — the 2026-08-02 review that prompted this
