# ADR-0007 — Exclusions render as `tagdrop`, so the renderer must not sort keys

**Status:** accepted · **Date:** 2026-08-02

## Context

muninn's YAML offers `exclude_mountpoints`, `exclude_devices` and
`exclude_interfaces`. Telegraf's plugins do not.

Checked against Telegraf 1.39.2:

| Plugin | Include | Exclude |
|---|---|---|
| `inputs.disk` | `mount_points` | `ignore_fs` (filesystem type only) — **no path exclusion** |
| `inputs.diskio` | `devices` | none |
| `inputs.net` | `interfaces` | none |

The only general exclusion mechanism is Telegraf's metric filtering — `tagdrop`,
a sub-table mapping tag names to glob patterns.

That would be a routine mapping decision, except for a constraint in Telegraf's
`CONFIGURATION.md`:

> when using the explicit table syntax (with `[...]`) for `tagpass` and `tagdrop`
> parameters, they **must be defined at the end** of the plugin definition,
> otherwise subsequent plugin config options will be interpreted as part of the
> tagpass/tagdrop tables.

## The measurement

This is not a theoretical concern, and — importantly — validation does not catch
it. Two configs differing only in where `ignore_fs` sits relative to the
`[inputs.disk.tagdrop]` header, run against Telegraf 1.39.2:

| | `config check` | disk metrics | of which `fstype=tmpfs` |
|---|---|---|---|
| `ignore_fs` before the table | exit **0** | 5 | 0 |
| `ignore_fs` after the table | exit **0** | 15 | 10 |

Both are accepted. In the broken one, `ignore_fs` is parsed as
`inputs.disk.tagdrop.ignore_fs` — a drop rule for a tag no metric carries — so
the filesystem exclusions silently do nothing and ten unwanted time series
appear.

Both files are kept as `docs/reference/ordering-{correct,broken}.conf`.

## Decision

1. Every `exclude_*` option renders into a `tagdrop` sub-table on the appropriate
   tag: `path` for `inputs.disk`, `name` for `inputs.diskio`, `interface` for
   `inputs.net`.
2. **The TOML renderer does not sort keys.** A `PluginInstance` holds its scalars
   in declaration order and its sub-tables separately; the renderer always emits
   scalars first and sub-tables last.
3. Determinism comes from a declared order — an explicit rank per plugin plus the
   plugin name — not from sorting.

## Consequences

- The obvious implementation of "make the output deterministic" is wrong here,
  which is why this is written down. Sorting keys alphabetically puts
  `[inputs.disk.tagdrop]` before `ignore_fs` and produces the broken config
  above, silently.
- Exclusions are applied after collection rather than during it. Telegraf gathers
  every mount point and then discards the filtered ones. The cost is negligible
  at these volumes, and no alternative exists.
- Include and exclude use different mechanisms: includes are plugin options
  (collection is narrowed), excludes are filters (results are dropped). Where both
  are set the include applies first. Documented in `docs/modules.md`.
- The renderer carries a regression test asserting on the *rendered bytes*, not on
  Telegraf's verdict — because Telegraf's verdict is `0` either way.

## Alternatives considered

**Inline table syntax** — `tagdrop = { path = ["/snap*"] }` — which has no
ordering constraint. Rejected: Telegraf's own documentation notes the inline form
must live in the main plugin definition and not in any sub-table, so it trades a
positional rule for a nesting rule; the explicit form is what Telegraf's examples
use; and it reads considerably worse for long glob lists.

**Restrict muninn's YAML to what plugins support natively**, dropping
`exclude_mountpoints` entirely. Rejected: excluding snap and Docker overlay
mounts is one of the most common things an operator needs, and an include-only
list is an allow-list — a filesystem mounted next month goes unmonitored until
someone remembers to add it.

**Always emit the tagdrop table, even when empty**, for a uniform shape.
Rejected: an empty `tagdrop` is noise in a file operators read while debugging.
