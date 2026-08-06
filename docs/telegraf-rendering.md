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

Both files live in `docs/reference/ordering-{correct,broken}.conf`, and the
renderer's tests assert on the **rendered bytes**, because Telegraf's verdict is
`0` either way.

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

`docs/reference/telegraf.reference.conf` is what the renderer produces for
`config/muninn.example.yaml`.

It began as a hand-written target, verified against Telegraf 1.39.2 before any
renderer existed, so the first renderer commit had a correct goal rather than a
plausible one. It is now regenerated from the renderer itself and re-verified:

```bash
muninn --config config/muninn.example.yaml render-config > docs/reference/telegraf.reference.conf
docker run --rm -v "$PWD/docs/reference:/ref:ro" telegraf:1.39.2 \
  telegraf config check --strict-env-handling --config /ref/telegraf.reference.conf
```

`crates/muninn-modules/tests/reference_config_test.rs` then asserts the renderer
still produces it, byte for byte. The pairing is what keeps this honest: the
test proves the renderer is stable, and the independent Telegraf check proves
what it is stable *at* is something Telegraf accepts. Updating the reference
without re-running the check would turn the test into one that agrees with
whatever the code happens to do.

## How the output is tested

Three layers, deliberately not all of the same kind.

**Properties**, asserted on the rendered bytes: a sub-table never precedes a
scalar, rendering twice is identical, insertion order changes nothing, awkward
values round-trip, and a hostile value cannot inject a plugin.

**Behaviour**, asserted on the whole pipeline: each `exclude_*` lands on the
right tag, `load` and `system` merge in all four enable combinations, only
enabled modules appear, and redacted output contains no secret.

**One golden file** — but verified independently, by real Telegraf, rather than
accepted because the code produced it.

That last distinction is the point. A golden file accepted without checking is a
record of whatever the code happens to do; the Telegraf check is what makes it
evidence rather than an echo. If the renderer changes legitimately, regenerate
**and** re-run the check.

## What the renderer does not do

- **No raw TOML passthrough.** See [ADR-0004](adr/0004-no-raw-toml.md).
- **No plugin option validation.** muninn only emits options it models; whether
  Telegraf accepts them is Telegraf's answer, obtained via `config check`. Keeping
  a copy of 249 plugins' option surfaces in muninn would drift on every release.
- **No prose.** The only comments are a fixed header and one provenance line per
  block (`# module: cpu`, `# modules: load, system`). That line earns its place:
  the generated file is what an operator reads when a metric is missing, and
  "which module put this here" is the question they are asking. Explanation
  beyond that belongs in the YAML they actually edit.

## Related

- [ADR-0007](adr/0007-tagdrop-and-render-order.md) — the ordering rule and its measurement
- [ADR-0008](adr/0008-system-and-load-merge.md) — instance merging
- [ADR-0006](adr/0006-validate-with-config-check.md) — validation
- [`modules.md`](modules.md) — per-module option mapping
