# Versioning and stability

muninn.io follows [Semantic Versioning 2.0.0](https://semver.org). A version
number only means something if you know which surfaces it protects, so this page
says which.

## The stable surface

Breaking any of these requires a major release.

| Surface | Covered |
|---|---|
| **Config schema** | Every YAML key in [`configuration.md`](configuration.md), including defaults and validation rules. A config that loads today loads on every later 1.x |
| **Schema version** | `version: 1` keeps working. A future `version: 2` does not remove support for 1 |
| **CLI** | `run`, `validate`, `render-config`, `check-runtime`, `update-check`, `image-check`, `healthcheck`, `version`, and the global flags |
| **Exit codes** | The values and meanings in [`supervision.md`](supervision.md). A code may gain meaning in a minor release; it never changes meaning |
| **Health endpoints** | `/health/live` and `/health/ready`, their paths, status codes and readiness semantics |
| **muninn metrics** | The `muninn_*` metric names, label names and units. New families may appear in a minor release; renaming or removing one is breaking |
| **Container contract** | Config path `/etc/muninn/muninn.yaml`, runtime directory `/run/muninn`, ports 8080 and 9273, non-root runtime |
| **Module semantics** | Which Telegraf plugin each module renders to, and what its options mean. A change that alters existing metrics is breaking |

## Explicitly unstable

May change in any release:

- **The generated Telegraf configuration.** It is an internal artefact, ephemeral
  by design. Its exact bytes are guaranteed *deterministic* for a given muninn
  version, not *stable across* versions — a plugin option added in a minor
  release changes the output, and that is fine.
- **The `/status` response shape.** It is a diagnostic, not an API. The fields it
  must never contain — secrets, full config — are stable; the JSON shape is not.
- **The Rust crate APIs.** `muninn-core`, `muninn-telegraf`, `muninn-modules` and
  `muninn-health` are internal structure, not a published library.
- **Log messages, fields and formatting**, in both human and JSON output.
- **Anything marked experimental.** Such a feature may change shape or be removed
  entirely without a major release — that is what the marker is for. Nothing
  carries it today: the updates module held it while it was still open whether
  the approach worked at all, and shed it once
  [`updates-evidence.md`](updates-evidence.md) settled that and the module
  shipped against real hosts. Its one known limit is a documented lower bound
  ([R8](risks.md)), not an unsettled design.

## The metrics Telegraf emits

Host metric names — `cpu_usage_idle`, `disk_used_percent` and so on — come from
Telegraf, not from muninn. They are stable in the sense that muninn does not
rename them, but they follow Telegraf's own compatibility policy.

A Telegraf version bump can therefore change them without muninn changing at all.
Such a bump is treated as a **minor** release at minimum, with the change noted
in the changelog, and as **major** if it alters metrics the MVP modules produce.

## MSRV

The minimum supported Rust version (currently **1.88**, `rust-version` in
`Cargo.toml`) may be raised in a minor release, never in a patch release. The
Dockerfile builder is pinned to the same version, so the published image is the
enforced gate.

Note the resolver interaction: edition 2024 uses resolver 3, which is MSRV-aware
and will hold a dependency back rather than require a newer compiler. That is how
a project silently stays on the release *before* a CVE fix. If the floor starts
constraining resolution, the floor gets raised — the old crate does not get
accepted.

## Telegraf version

The pinned Telegraf version is recorded in
[ADR-0011](adr/0011-telegraf-pinning.md) and reported by `muninn version`.

muninn refuses to start if the runtime binary's version does not match the one it
was built against. Plugin options and defaults move between minor releases, so a
mismatched pair is not a configuration muninn can reason about.

## Releasing

The version comes from `CHANGELOG.md`; the pipeline creates the tag. Never
hand-push a `v*` tag. See [`ci-cd.md`](ci-cd.md).

## Supported versions

Only the latest release receives fixes. See [`SECURITY.md`](SECURITY.md).
