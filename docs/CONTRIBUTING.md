# Contributing

## Before you start

muninn is feature-complete and released — `0.1.0` is the first version. Read
[`architecture.md`](architecture.md) for the shape of the thing, and
[`roadmap.md`](roadmap.md) for what is still open.

If you are an AI coding agent, read [`../AGENTS.md`](../AGENTS.md) first — it is
the operating manual and it is more specific than this page.

## Branching

Branch off `dev` with one of these prefixes:

```
feature/   fix/   chore/   docs/   test/
```

Flow: `feature/* → dev` (squash merge) → `main` (merge commit). No direct pushes
to `dev` or `main`.

## Commits

[Conventional Commits](https://www.conventionalcommits.org/):
`feat` · `fix` · `chore` · `docs` · `test` · `refactor` · `perf` · `style`.

Write the body for the reader who finds the commit in a year while bisecting.
State **why**, and state **what you verified**:

```
fix: reject wildcard port collisions between health and prometheus

0.0.0.0:8080 and 127.0.0.1:8080 cannot both bind, but the check
compared addresses for equality and passed them. The failure surfaced
as a listener silently not starting, with only a logged error, while
readiness still reported true.

Verified: new test covers wildcard-vs-specific in both directions;
cargo test --workspace passes.
```

"Should work" is not a verification. If you did not run it, say so.

End the body with the `Co-Authored-By` trailer when applicable.

## Gates

Run these before opening a PR:

```bash
cargo fmt --all -- --check
cargo lint          # clippy --workspace --all-targets --all-features -- -D warnings
cargo t-all         # test --workspace
cargo audit-all     # deny check
cargo cov-ci        # coverage, ≥80% workspace lines
```

CI enforces every one of these on every PR, and adds the image build, the Trivy
and Semgrep scans and the three system suites — see
[`ci-cd.md`](ci-cd.md). Run them locally anyway: a PR that fails a gate is a PR
that wastes a review, and the image jobs take tens of minutes to tell you the
same thing `cargo fmt` would have said in one second.

## Standards

**Production code without a test is not merged.** If you find untested code in a
review, ask for a test before approving.

**No `unwrap()`, `expect()` or `panic!` in non-test code.** Return a `Result` and
propagate. Panics are for tests and genuinely unreachable invariants, with a
comment saying why it is unreachable.

**Fix clippy, do not silence it.** If an `#[allow]` is genuinely needed, give a
one-line reason.

**Comments explain why, not what.** The code already says what it does. What it
cannot say is which bug this line prevents.

**Match the surrounding code.** Naming, layout, comment density, idiom.
Consistency beats personal preference.

**Never accept a snapshot without reading the diff.** Use `cargo snap-review`.
This is a hard rule, not a suggestion — see [`testing.md`](testing.md).

## Changes that need a decision record

If a change alters an architectural commitment, add or amend an ADR in
[`adr/`](adr/) in the same PR. Specifically:

- the Telegraf validation strategy,
- the rendering order or determinism guarantee,
- how secrets are stored, resolved or redacted,
- the restart or supervision model,
- what the container mounts or which capabilities it needs,
- how a module maps onto Telegraf plugins, if the mapping is not one-to-one.

An ADR is short: context, decision, consequences, alternatives considered. The
alternatives section is the one people actually come back for.

## Security-relevant changes

Anything touching secrets, mounts, the Docker socket, network exposure,
dependencies or workflow permissions must be called out explicitly in the PR
description. Say what changes and what the new exposure is. When in doubt, choose
the safer option and flag it.

See [`hardening.md`](hardening.md) and [`SECURITY.md`](SECURITY.md).

## Adding a dependency

Needs approval first. Every crate widens the supply-chain surface, and this
project deliberately runs a small dependency set. In the PR, say what it does,
why the standard library or an existing dependency does not, and what its own
dependency tree looks like.

## Documentation

Code and documentation land together. A module without an entry in
[`modules.md`](modules.md), or a config key without an entry in
[`configuration.md`](configuration.md), is not finished.

Everything committed is in **English** — code, comments, commit messages, docs.

## Related

- [`AGENTS.md`](../AGENTS.md) — the rules, the gates and the doc map
- [`testing.md`](testing.md) — what a test has to look like here
- [`workflows.md`](workflows.md) — what CI will run on your branch
- [`releasing.md`](releasing.md) — how a release is cut
