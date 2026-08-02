# ADR-0008 — `load` and `system` render into one `[[inputs.system]]` instance

**Status:** accepted · **Date:** 2026-08-02

## Context

The project brief lists `load` and `system` as separate modules — load averages
on one side, uptime and logged-in users on the other. That is a reasonable split
from an operator's point of view; they are different questions.

Telegraf does not split them. There is no `inputs.load`. Load averages, uptime,
user counts, CPU counts, OS release and DMI data are all groups of a single
plugin, selected through one option:

```toml
[[inputs.system]]
  include = ["load", "uptime", "users"]   # default is ["legacy"], which covers all three
```

Rendering each muninn module to its own `[[inputs.system]]` block would give two
plugin instances collecting overlapping groups on the same schedule — every
metric emitted twice, with identical tags, at the same timestamp. In InfluxDB the
second write overwrites the first; in Prometheus it is a duplicate series.
Neither fails loudly.

## Decision

Both modules stay in the YAML. The renderer merges them.

A module may return a `merge_key`; modules sharing a key are combined into one
`PluginInstance` whose list-valued options are the union of theirs.

| YAML | `include` groups contributed |
|---|---|
| `load.enabled: true` | `load` |
| `system.enabled: true` | `uptime`, `users` |
| both enabled | `load`, `uptime`, `users` — one instance |
| neither | no `[[inputs.system]]` at all |

Union order follows a fixed group order, not the order the modules were written
in, so the output stays deterministic regardless of how the YAML is arranged.

## Consequences

- The YAML keeps the vocabulary an operator thinks in. "I want load averages" is
  `load: enabled: true`, not a group name inside a plugin they have never heard
  of.
- One mapping in the codebase is not one-to-one, which is exactly the kind of
  thing that gets broken by a well-meaning refactor. Hence this ADR, the
  `merge_key` in the trait, and four tested cases: each module alone, both, and
  neither.
- Enabling `system` alone gives uptime and users but *not* load, even though
  Telegraf's own default (`include = ["legacy"]`) would include it. muninn is
  explicit here rather than convenient: a module you did not enable does not
  collect.
- The merge machinery generalises. If a future module maps onto an already-used
  plugin, it declares the same key rather than needing new special-casing.

## Alternatives considered

**Drop the `load` module and fold load averages into `system`.** Simpler code,
worse configuration: "load" is what people search for, and hiding it inside
another module's defaults makes it undiscoverable.

**Emit two `[[inputs.system]]` instances with disjoint `include` groups** — one
with `["load"]`, one with `["uptime", "users"]`. This actually produces correct
metrics, since the groups do not overlap. Rejected anyway: it is a coincidence of
the current group set, not a property. The first pair of modules that share a
group would start duplicating silently, and nothing in the code would object.
Merging is correct by construction.

**Use `name_override` or plugin aliases to keep them separate.** Rejected: it
changes the measurement names that reach InfluxDB and Prometheus, which is a
visible, breaking difference from stock Telegraf for no gain.
