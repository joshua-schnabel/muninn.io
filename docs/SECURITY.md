# Security policy

## Reporting a vulnerability

**Do not open a public issue for a security problem.**

Report privately through GitHub's
[private vulnerability reporting](https://github.com/joshua-schnabel/muninn.io/security/advisories/new),
which notifies the maintainer without disclosing anything publicly.

Please include: what the issue is, how to reproduce it, which version or commit
you tested, and what an attacker could achieve. A proof of concept helps but is
not required.

You can expect an acknowledgement within a few days. This is a solo project, so
the response is best-effort rather than an SLA — but security reports go to the
front of the queue.

## Scope

muninn's security-relevant surface, in rough order of severity:

| Area | Why it matters |
|---|---|
| **Secret handling** | Tokens and passwords are read from files, resolved into the generated config, and must never appear in logs, errors, `/status` or `render-config` output |
| **The generated config** | Holds resolved secrets in plaintext on a tmpfs |
| **The host mount** | Read access to the entire host filesystem |
| **The Docker socket** | Root-equivalent when the `docker` or `image_updates` module is enabled |
| **Network endpoints** | Health and metrics listeners, unauthenticated by default |
| **Supply chain** | The pinned Telegraf binary and every Rust dependency |
| **Container posture** | Non-root, read-only, capabilities dropped |
| **The build pipeline** | What can reach a token, a secret or the published image — see below |

Findings of particular interest:

- any path by which a secret value reaches a log line, an error message, an HTTP
  response or a persisted file;
- any way the generated configuration can be influenced to do something the YAML
  did not ask for — a config injection through an operator-supplied path, glob or
  URL;
- privilege escalation from the container to the host beyond what the documented
  mounts already grant;
- a way to make a failed check report a healthy value.

## The build pipeline is part of the surface

An agent that ships as a signed-off container image is only as trustworthy as
the pipeline that produced it, so `.github/workflows/` is in scope for a report
just as the Rust is. What is deliberate there:

- **No workflow gives a token to third-party code.** `actions/checkout` writes
  its token into `.git/config` by default, and `cargo` executes `build.rs` and
  proc-macros from every dependency in the tree. Every checkout sets
  `persist-credentials: false` except the two jobs that push (`ci.yml`
  `publish`, `release.yml` `prepare-dev`), and neither of those runs `cargo`.
  The release's test run is a separate job with `contents: read` for exactly
  this reason.
- **Credentials never go through argv.** A command line is world-readable on the
  runner through `/proc`. Registry credentials go to `skopeo login` on stdin and
  live in a `0600` auth file for the length of the step; API bodies and headers
  go to `curl` through stdin, not `-d` and `-H`.
- **Permissions are per job, not per workflow**, and default to
  `contents: read`. `security-events: write` belongs to the one job that uploads
  SARIF.
- **Everything executable is pinned.** Third-party actions by commit SHA, the
  Semgrep and actionlint images by digest, `cargo-deny` and `cargo-llvm-cov` by
  version, base images by digest, and Telegraf by SHA-256
  ([ADR-0011](adr/0011-telegraf-pinning.md)) — which the CI jobs reuse via
  `scripts/fetch-telegraf.sh` rather than pulling a mutable `telegraf:x.y.z`
  tag.
- **Secrets are unreachable from a pull request.** The publishing jobs are
  `if: github.event_name == 'push'`, so a PR build — including one from a fork —
  never has the DockerHub token in its environment.

One accepted trade, stated plainly: **Dependabot patch and minor bumps
auto-merge into `dev`** once the full pipeline is green
(`.github/workflows/dependabot-auto-merge.yml`). A compromised upstream release
that passes every gate would land without a human reading it. It is bounded by a
three-day cooldown before Dependabot opens the PR, by `cargo deny`'s advisory
gate, and by the fact that `dev` is not `main` — but it is a real trade and it
is a deliberate one. Major bumps always wait for a human.

## Out of scope

- The documented consequences of `/:/hostfs:ro`. Read access to the host
  filesystem is what the mount is for, it is documented in
  [`hardening.md`](hardening.md), and it is an operator's decision.
- The documented consequences of enabling the Docker module and providing the
  socket. Also an operator's deliberate act, with the exposure stated at the
  point of decision.
- Unauthenticated metrics endpoints on a deployment that did not configure
  authentication or network isolation.
- Vulnerabilities in Telegraf itself — report those to
  [InfluxData](https://github.com/influxdata/telegraf/security). If a Telegraf CVE
  affects the pinned version here, a report is welcome so the pin can be moved.

The line: if muninn's documentation tells you the risk before you accept it, it is
a documented trade. If muninn does something you would not expect from its
documentation, that is a vulnerability.

## Supported versions

Only the latest release receives fixes.

## Disclosure

Coordinated. A fix is released first, then an advisory naming the affected
versions. Credit is given unless you prefer otherwise.
