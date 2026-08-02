# ADR-0004 — No raw Telegraf TOML in the muninn configuration

**Status:** accepted · **Date:** 2026-08-02

## Context

There is an obvious escape hatch for a wrapper like this: let the operator paste
arbitrary Telegraf TOML into the YAML, and splice it into the generated file.

```yaml
# not supported
extra_telegraf_config: |
  [[inputs.postgresql]]
    address = "host=localhost"
```

It answers every "but I need plugin X" request at once, and it is genuinely
tempting for that reason.

## Decision

muninn does not support raw TOML fragments. Every input and output is a typed
module with named options.

## Consequences

- A plugin muninn does not model cannot be used. That is a real limitation, and
  the roadmap's phase 5 is where additional inputs get modelled properly.
- Everything muninn generates can be validated before Telegraf starts, because
  muninn understands all of it.
- Error messages can name a YAML key. With a pasted fragment, the best available
  message is Telegraf's own parse error against a line number in a file the
  operator never wrote.
- The generated output stays deterministic. A pasted string would have to be
  reproduced byte-for-byte including its whitespace, and would break the
  guarantee that identical input yields identical output.
- `muninn check-runtime` stays meaningful. Modules declare the mounts and
  capabilities they need; a raw fragment declares nothing, so its requirements
  could not be checked and its failures would surface at runtime.
- Secrets stay file-only. A raw block is a place to type a token, and it would
  bypass the redaction that `render-config` and the logs rely on.
- Requests for unsupported plugins become visible as issues rather than being
  absorbed silently. That is a feature: it is how the module list learns what
  people actually need.

## Alternatives considered

**Allow raw blocks but validate the merged result with `telegraf config check`.**
This catches syntax errors and unknown plugins. It does not catch the things that
matter: a fragment that binds a port muninn already uses, needs a mount nobody
declared, or writes credentials into the generated file. And it would still break
determinism and error attribution.

**Allow raw blocks behind an `unsupported:` key with a loud warning.** Rejected.
In practice the escape hatch becomes the documented answer to every gap, the
typed modules stop being extended, and the abstraction the project exists to
provide erodes. If a plugin is worth using, it is worth a module.

**Allow raw blocks only for outputs.** Rejected: outputs are where the secrets
are, so this is the worst place to open the hatch.
