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
| **The Docker socket** | Root-equivalent when the Docker module is enabled |
| **Network endpoints** | Health and metrics listeners, unauthenticated by default |
| **Supply chain** | The pinned Telegraf binary and every Rust dependency |
| **Container posture** | Non-root, read-only, capabilities dropped |

Findings of particular interest:

- any path by which a secret value reaches a log line, an error message, an HTTP
  response or a persisted file;
- any way the generated configuration can be influenced to do something the YAML
  did not ask for — a config injection through an operator-supplied path, glob or
  URL;
- privilege escalation from the container to the host beyond what the documented
  mounts already grant;
- a way to make a failed check report a healthy value.

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
