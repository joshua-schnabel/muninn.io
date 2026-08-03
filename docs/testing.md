# Testing

Adapted from huginn.io's testing guide, with the parts that matter more here
emphasised — muninn is PID 1 with a child process, so process-lifecycle bugs are
its native failure mode.

## The pyramid

```
              ▲
             /S\        System integration
            /───\       • Real Docker stack: muninn + Telegraf + InfluxDB
           /     \      • CI only, highest confidence
          / E2E   \
         /─────────\    End-to-end
        /           \   • The compiled binary as a subprocess
       /  Integration \ • Real Telegraf, real sockets
      /───────────────\
     /                 \ Integration
    /   Unit            \• Several components together
   /─────────────────────\• Snapshot tests of rendered config
  /                       \
 /─────────────────────────\Unit
                            • One function or module
                            • Fast, isolated, deterministic
```

| Level | Location | Speed |
|---|---|---|
| Unit | `#[cfg(test)]` inside source modules | < 1 s |
| Snapshot | `#[cfg(test)]` with `insta` | < 1 s |
| Integration / E2E | `muninn/tests/*.rs` | seconds |
| System | `scripts/integration-test.sh` + compose | minutes, CI |

## The platform gap, and why it is not optional

Development happens on Windows; the artefact is a Linux container. The tests
that matter most — signal handling, file permissions, reaping a child — are
`#[cfg(unix)]`, so on Windows they compile and are **silently absent**. A green
local run is not a green run.

```bash
bash scripts/test-linux.sh              # whole workspace, in a container
bash scripts/test-linux.sh -p muninn    # anything after the name goes to cargo test
```

It runs the suite in `rust:1.88-slim` against the Telegraf binary taken from the
pinned image, so the tests see the same version the artefact ships.

This is not belt-and-braces. The first time the suite ran under Linux it found a
real bug: signal handlers were installed inside the supervise loop, leaving a
window during startup where SIGTERM still had its default disposition and killed
muninn instead of shutting it down. Nothing on the development machine could
have seen it.

## Test the artefact, not just the code

huginn.io shipped a bug where `run()` returned immediately, `main()` exited, the
Tokio runtime was dropped, and every probe was cancelled before it fired. The
daemon monitored nothing. **Every test passed**, for months — because they all
spawned `run()` into the *test's* runtime, which outlived it. Production has no
such runtime.

This matters more for muninn. The whole point of the program is what happens
across a process lifecycle: PID 1 duties, spawning a child, forwarding SIGTERM,
waiting out a grace period, SIGKILL, reaping, exit codes. **No in-process test can
observe any of it.**

So `muninn/tests/` runs the compiled binary via `CARGO_BIN_EXE_muninn` and
asserts on observable behaviour: it stays up, it serves, it starts Telegraf, it
exits `0` on SIGTERM within the grace period, and it exits `22` when Telegraf
dies.

**Rule: if the behaviour depends on the process lifecycle, test the process.**

## Snapshot tests

The generated Telegraf configuration is exactly the kind of large structured
artefact where a reviewed diff beats a wall of assertions. Every module has a
snapshot of its rendered fragment, plus whole-config snapshots for the minimal
config, the full example, each output alone, both together, every module enabled,
and redacted `render-config` output.

**Snapshots are reviewed, never auto-accepted.**

```bash
cargo snap-review     # cargo insta review — step through and read each diff
```

`cargo insta accept` on an unread diff turns the suite into a record of whatever
the code happens to do. This is the single most likely way muninn's determinism
guarantee rots, which is why it is also a hard rule in `AGENTS.md`.

One independent check stands behind the snapshots: `docs/reference/telegraf.reference.conf`
is verified against real Telegraf, so a wrongly-accepted snapshot still has
something to fail against.

## What must have tests

**Configuration** — every field parses; every field has at least one negative
test; unknown keys are rejected with the key path named; missing and unknown
schema versions are distinct errors; durations reject zero, negative and
unparseable values.

**Secrets** — missing, unreadable and empty files are distinct errors; the path
appears in errors and the contents never do; formatting a secret with `{:?}`
yields `***` (assert the real value is absent, not just that `***` is present).

**Semantics** — no output enabled is fatal; port collisions are caught including
wildcard overlap (`0.0.0.0:8080` against `127.0.0.1:8080`); InfluxDB without a
readable token is fatal.

**Rendering** — determinism (render twice, compare bytes); sub-tables come last;
`load` + `system` merge in all four enable combinations; escaping survives
spaces, `"`, `\`, `'`, newlines and non-ASCII in paths and globs.

**Supervision** — every legal state transition, and the rejection of illegal
ones; exit codes; signal handling; grace-period expiry leading to SIGKILL.

## Conventions

**Names read as English sentences.** `rejects_unknown_key`,
`merges_load_and_system_into_one_instance`, `reports_failure_when_mount_missing`.
Not `test_config_1`, not `should_work`.

**Don't sleep — poll.** A fixed `sleep` before an assertion is a flake waiting
for a loaded CI runner. Poll with a deadline (`tokio::time::timeout` around a
retry loop). huginn.io had exactly this flake: a 150 ms sleep and one unretried
request, which failed under concurrent compile load.

**Serialise environment tests.** The environment is process-global and cargo runs
tests on parallel threads, so one test's `remove_var` races another's `set_var`.
Use a mutex-guarded helper.

**Never hit real external services.** Local sockets, temporary containers or
fixtures. A test that needs the internet is a test that fails on a train.

**A skipped test must say so.** The lifecycle tests need a real Telegraf binary
(`MUNINN_TELEGRAF_BIN`); without one they print `SKIP:` and a reason. A test that
quietly passes when its precondition is absent is indistinguishable from one that
verified something, which is worse than having no test at all.

**Do not assert against the ambient environment.** A test that read
`MUNINN_TELEGRAF_BIN` and expected it unset passed on Windows and failed in the
container, where it is legitimately set. The fix was to separate the decision
from reading the environment and test the decision.

## Coverage

CI enforces ≥ 80 % workspace line coverage:

```bash
cargo cov-ci        # cargo llvm-cov --workspace --lcov ... --fail-under-lines 80
cargo cov-html      # per-file, per-region report
```

Know what that does and does not buy you. It is an **aggregate**, not a per-file
floor — a well-covered crate can mask an entirely untested module. It counts
**lines**, not branches. And a covered line is not a checked behaviour: huginn.io
had a function at ~100 % coverage that was unreachable in production, with three
tests inflating the gate while asserting nothing about the shipped binary.

Treat 80 % as a floor against collapse, not as evidence anything is tested.

## Running

```bash
cargo t-all                      # everything
cargo test -p muninn-core        # one crate
cargo test rejects_unknown_key   # by name
cargo t-verbose                  # with stdout
cargo snap-review                # review pending snapshots
```

Against the image, which has to be built first (`docker build -t muninn:dev .`):

```bash
bash scripts/container-test.sh          # the artefact, in the documented posture
bash scripts/updates-test.sh            # the updates module against real host trees
bash scripts/updates-test.sh S8 S9      # selected cells
```

`updates-test.sh` is the system-test level the brief asks for in §18.6, and it is
the only suite that can catch one particular kind of failure: the updates module
runs real `apt` against a real host tree, so nothing below it has a truth to
compare against. It shares its fixtures with the WP1 spike, and compares the
shipped image's answers against the same ground truth the spike measured — if the
implementation ever drifts from what was proven, a cell goes red rather than a
number going quietly wrong.

## Quick reference

| Change | Test type | Where |
|---|---|---|
| New config field | Unit + negative | `config/` module, inline |
| New module | Unit + snapshot | module file, inline |
| New render rule | Snapshot + determinism | `muninn-telegraf` |
| New state transition | Unit | `supervisor/state.rs` |
| New CLI command | Integration | `muninn/tests/` |
| Anything lifecycle-dependent | E2E subprocess | `muninn/tests/` |
| Bug fix | Reproduce first | same file as the fix |
