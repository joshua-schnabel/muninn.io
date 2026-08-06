# Reading the host's pending package updates from a container

The measured basis for the updates module. Approach A was chosen here, and
[ADR-0009](adr/0009-updates-module-approach.md) records the decision; this page
is the evidence behind it, including the numbers the module is still checked
against and the failure modes that were found the hard way.

The same ground truth is asserted continuously by `scripts/updates-test.sh`,
which runs the shipped image against these fixtures on every pipeline run.

## 1. Method

Simulated hosts are containers built from real Debian and Ubuntu images whose apt
state is exported and then mounted read-only into a probe container. Dated image
tags are pinned rather than `:12` / `:24.04`, because the current images are fully
patched and every cell would trivially report zero.

| Fixture | Image | Ground truth |
|---|---|---|
| deb12-fresh | `debian:12`, then `apt-get upgrade -y` | 0 pending |
| deb12-stale | `debian:bookworm-20240211` | 41 pending, 3 security |
| deb13-stale | `debian:trixie-20250428` | 39 pending, 2 security |
| ubu22-stale | `ubuntu:jammy-20240227` | 50 pending, 40 security |
| ubu24-stale | `ubuntu:noble-20240605` | 66 pending, 34 security |
| deb12-oldlists | as deb12-stale, indices backdated 30 days | 41 pending |

Ground truth is what each host answers about *itself*, from inside itself, with
the same `apt-get -s dist-upgrade` the probe runs. The probe was a shell script
run from `debian:12-slim`; `muninn update-check` replaced it and is what
`scripts/updates-test.sh` measures today.

The fixture deliberately preserves `/etc/os-release` as a symlink **and**
`/usr/lib/os-release` as its target, because flattening that would have hidden a
real failure mode — see §4.

## 2. Results

All thirteen cells pass.

| # | Case | Expected | Measured | |
|---|---|---|---|---|
| T1 | debian:12, freshly upgraded | 0 / 0, success | `pending_all=0, pending_security=0, check_success=1` | ✅ |
| T2 | debian:12, outdated | matches host | **41 / 3** — identical to ground truth | ✅ |
| T3 | security subset | `0 < security ≤ total` | 3 of 41 | ✅ |
| T4 | debian:13, outdated | matches host | **39 / 2** — identical | ✅ |
| T5 | ubuntu:22.04, outdated | matches host | **50 / 40** — identical | ✅ |
| T6 | ubuntu:24.04, outdated | matches host | **66 / 34** — identical | ✅ |
| T7 | package lists 30 days old | age reported, result still produced | `lists_age_seconds=2592334`, `check_success=1` | ✅ |
| T8 | required mount absent | failure, **no** pending fields | `check_success=0, reason=hostfs_not_mounted`, counts omitted | ✅ |
| T9 | dpkg status empty | failure, **no** pending fields | `check_success=0, reason=dpkg_status_empty` | ✅ |
| T9b | dpkg status corrupt | failure, never zero | `check_success=0, reason=apt_failed` | ✅ |
| T10 | debian:12-slim reads an ubuntu:24.04 host | correct or detected error | **66 / 34** — correct across distributions | ✅ |
| T11 | real host, probe run in place | matches native apt | 0 = 0 — agrees; that host has nothing pending | ✅ |
| T11b | real host, probe run **from a container**, fresh indices | matches native apt | **41 / 11** — identical to the host's own answer | ✅ |

### The cells that matter most

T2 through T6 establish that it *works*. **T8, T9 and T10 establish that when it
does not work, you can tell** — which is the property the whole module stands on.

T9b is worth singling out. A structurally corrupt dpkg status file makes apt exit
non-zero, and the probe reports `check_success=0`. The alternative — apt parsing
zero packages and cheerfully reporting "0 updates pending" — is precisely the
failure this module exists to avoid, and the test asserts against it explicitly.

T10 was the one expected to break and did not. A `debian:12-slim` container with
apt 2.6.1 reads an Ubuntu 24.04 host's package indices and produces that host's
exact answer. This matters because container and host being different
distributions is the *normal* case, not the exotic one: the muninn image has a
fixed base and hosts do not.

### The real-host cells

T11 runs the probe *on* the host. It agrees — but that host's package indices
were four months old, so both it and native apt answer zero, and agreeing on zero
does not exercise the counting path.

**T11b closes that**, and is the more faithful cell anyway because it runs the
probe the way muninn actually will: from a container, against the host's
filesystem. `scripts/fixtures/build-host-native.sh` fetches fresh indices
into a scratch directory — via `Dir::State::lists`, so `/var/lib/apt/lists` is
left untouched; a measurement that modified the machine it was measuring would
invalidate its own criterion — and exports the host's real dpkg status alongside
them.

That host has 295 installed packages and a genuinely non-zero answer:

```
real host, its own apt, fresh indices : 41 pending, 11 security
probe from a debian:12-slim container : 41 pending, 11 security
```

So the counting path is confirmed against a real machine, not only against
container fixtures.

### Hardening and non-modification

Two acceptance criteria checked separately from the matrix:

```
non-root (65534), --cap-drop=ALL, --read-only, tmpfs /tmp
  → pending_all=41i, pending_security=3i, check_success=1i
SHA-256 over all 461 files of the host tree, before and after
  → 3003823b…b72e2 both times — identical
```

Approach A needs no capabilities, no write access to the host, and no root.

The rejected approaches, and why each fails, are recorded in
[ADR-0009](adr/0009-updates-module-approach.md#alternatives-considered).

## 3. Findings worth keeping

**`/etc/os-release` is a symlink to `/usr/lib/os-release`.** The first probe
version read only `/etc/os-release` and reported `os_release_unreadable` for a
plainly Debian host, because a mount that includes `/etc` but not `/usr/lib`
leaves the symlink dangling. The probe now tries both. This is concrete support
for [ADR-0005](adr/0005-hostfs-mount.md): mounting hand-picked paths breaks in
ways that mounting the root does not.

**`Debug::NoLocking=1` is required.** Without it apt tries to take
`/var/lib/dpkg/lock` on a read-only mount and fails.

**`Dir::Cache` is the only directory apt genuinely needs to write**, and pointing
it at a scratch directory inside the container is sufficient. `Dir::State::lists`
can stay read-only despite apt's usual `lists/partial` handling, because
simulation only reads the indices.

**Security classification comes from the candidate version's origin.** Debian
writes `Debian-Security:12/stable-security`, Ubuntu writes
`Ubuntu:24.04/noble-security`. Matching on `-security` rather than a vendor name
keeps both working — and keeps working for third-party security suites.

**`apt-get -s dist-upgrade`, not `upgrade`.** `upgrade` will not install new
packages, so it under-reports where a security fix pulls in a new dependency.

The cost — `apt` and `dpkg` in the runtime image, so debian-slim rather than
distroless — is quantified in [`hardening.md`](hardening.md) and accepted in
[ADR-0009](adr/0009-updates-module-approach.md#consequences).

## 4. What the implementation found afterwards

Two things these measurements could not show, because both are properties of the
deployment rather than of the approach:

- **apt takes temp files outside `Dir::Cache`.** It calls
  `mkstemp /tmp/clearsigned.message.XXXXXX` while reading signed release files,
  even with `-s`. The hardened cell above had a tmpfs on `/tmp`; muninn's
  documented deployment does not, and the check failed there with
  `GetTempFile (30: Read-only file system)` on a host it could read perfectly.
  `TMPDIR` is now set to the scratch directory for the apt child itself.
- **The Ubuntu security count moved.** Rebuilt against today's archive, the
  Ubuntu 24.04 fixture reports 66 pending / **0** security where it reported
  66/34 here — the packages are the same, but the candidate now resolves through
  `noble-updates` rather than `noble-security`. The host's own apt says the same,
  so this is a property of the classification rule rather than a regression. See
  [R8](risks.md).
