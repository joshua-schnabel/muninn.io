# Generating the Telegraf configuration

How a validated muninn config becomes a Telegraf TOML file that is byte-identical
across runs and that Telegraf accepts.

## The pipeline

```text
normalised config
      │
      ▼
  each enabled module: render() → Vec<PluginInstance>
      │
      ▼
  merge instances sharing a merge_key        (load + system)
      │
      ▼
  order: [agent], inputs by rank, outputs by rank
      │
      ▼
  serialise: scalars in declaration order, sub-tables last
      │
      ▼
  /run/muninn/telegraf.conf
      │
      ▼
  telegraf config check --strict-env-handling
```

## The intermediate representation

Modules never build strings. They return typed instances:

```rust
pub struct PluginInstance {
    /// e.g. "inputs.disk"
    plugin: &'static str,
    /// Sort rank within its section. Fixed per plugin, never derived from config.
    rank: u16,
    /// Emitted first, in this order.
    scalars: Vec<(&'static str, TomlValue)>,
    /// Emitted last. tagdrop, tagpass, tls, ...
    subtables: Vec<(&'static str, TomlTable)>,
}
```

Splitting scalars from sub-tables is not tidiness. It is what makes the ordering
rule below enforceable by construction rather than by discipline.

## Rule 1 — scalars first, sub-tables last

Telegraf's `CONFIGURATION.md`:

> when using the explicit table syntax (with `[...]`) for `tagpass` and `tagdrop`
> parameters, they **must be defined at the end** of the plugin definition,
> otherwise subsequent plugin config options will be interpreted as part of the
> tagpass/tagdrop tables.

This is TOML semantics, not a Telegraf quirk: once `[inputs.disk.tagdrop]` opens,
every following key belongs to that table.

**Validation does not catch the mistake.** Measured against Telegraf 1.39.2:

| | `telegraf config check` | disk metrics | of which `fstype=tmpfs` |
|---|---|---|---|
| `ignore_fs` before the table | exit **0** | 5 | 0 |
| `ignore_fs` after the table | exit **0** | 15 | 10 |

Both accepted. In the second, `ignore_fs` became
`inputs.disk.tagdrop.ignore_fs` — a drop rule for a tag no metric carries — so
the exclusions did nothing and ten unwanted series appeared.

Both files live in `docs/reference/ordering-{correct,broken}.conf`, and WP3
asserts on the **rendered bytes**, because Telegraf's verdict is `0` either way.

## Rule 2 — determinism comes from declared order, not sorting

The reflexive way to make generated output deterministic is to sort keys. Here
that is actively wrong: alphabetical order puts `[inputs.disk.tagdrop]` before
`ignore_fs` and produces the broken config above.

So ordering is declared:

- sections in fixed order: `[agent]`, inputs, outputs;
- instances within a section by `rank`, ties broken by plugin name;
- scalars in the order the module pushed them;
- sub-tables after all scalars, in declaration order.

Guarantee: **identical config plus identical muninn version yields byte-identical
output.** No timestamps, no generated identifiers, no map iteration order, no
locale-dependent formatting. The only version-varying content is the header
comment naming the muninn version.

## Rule 3 — one encoder, one place to escape

Every value goes through the `toml` crate. No module concatenates strings, and no
module writes a `format!` that reaches the output.

This matters because the values are operator-supplied: mount-point globs, device
patterns, URLs, file paths. A path containing a quote or a backslash — routine on
a badly-named volume — must not be able to alter the structure of the generated
file. Escaping is tested against spaces, `"`, `\`, `'`, newlines and non-ASCII.

## Rule 4 — secrets are resolved, and redacted on every other path

The generated file contains real secret values, because Telegraf needs them and
muninn deliberately does not use `${ENV}` indirection
([ADR-0003](adr/0003-ephemeral-generated-config.md)).

Everywhere else they are redacted, and by type rather than by convention:

```rust
struct Secret(String);
// Debug and Display both render "***"
// The value is reachable only through .expose(), called in exactly one place
```

So `tracing::debug!(?config)` cannot leak a token, and neither can an error
message, a `/status` response or a panic backtrace. `muninn render-config`
redacts by default; its output is safe to paste into an issue.

## The reference output

`docs/reference/telegraf.reference.conf` is what the renderer must produce for
`config/muninn.example.yaml`. It was written by hand and verified with

```bash
telegraf config check --strict-env-handling --config telegraf.reference.conf
```

against Telegraf 1.39.2 — exit 0 — before any renderer code existed. It is WP3's
primary snapshot fixture, so the first renderer commit has a correct target
rather than a plausible one.

## Snapshot testing

Every module has a snapshot of its rendered fragment, plus whole-config snapshots
for: minimal config, full example, InfluxDB only, Prometheus only, both outputs,
every module enabled, and redacted `render-config` output.

**Snapshots are reviewed, never auto-accepted.** `cargo insta accept` without
reading the diff turns the test suite into a record of whatever the code happens
to do. Use `cargo snap-review` and read.

## What the renderer does not do

- **No raw TOML passthrough.** See [ADR-0004](adr/0004-no-raw-toml.md).
- **No plugin option validation.** muninn only emits options it models; whether
  Telegraf accepts them is Telegraf's answer, obtained via `config check`. Keeping
  a copy of 249 plugins' option surfaces in muninn would drift on every release.
- **No comments beyond a fixed header.** The file is machine-consumed and
  ephemeral. Explanation belongs in the YAML the operator actually edits.

## Related

- [ADR-0007](adr/0007-tagdrop-and-render-order.md) — the ordering rule and its measurement
- [ADR-0008](adr/0008-system-and-load-merge.md) — instance merging
- [ADR-0006](adr/0006-validate-with-config-check.md) — validation
- [`modules.md`](modules.md) — per-module option mapping
