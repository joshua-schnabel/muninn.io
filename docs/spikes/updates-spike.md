# Spike — reading the host's pending package updates from a container

**Status: complete.** Approach A adopted. Decides
[ADR-0009](../adr/0009-updates-module-approach.md) and the runtime base image.
Unblocks WP8 and WP10.

Run it yourself: `bash spikes/updates/run.sh` (needs Docker; T11 needs WSL).

## Result in one paragraph

Approach A — mounting the host's apt and dpkg state read-only and running
`apt-get -s dist-upgrade` against it — works, and works exactly. Across Debian
12, Debian 13, Ubuntu 22.04 and Ubuntu 24.04 it reproduces the host's own answer
to the package, including the security subset, and it does so from a container
running a *different* distribution than the host. It runs under muninn's full
hardening (non-root, `--cap-drop=ALL`, read-only root filesystem) and leaves the
host tree byte-identical. Approaches B and C are dead: both need capabilities the
hardening baseline excludes, and B fails even when granted them.

The cost is that the runtime image needs `apt` and `dpkg`, so it is debian-slim
rather than distroless. That is a real regression and is quantified below.

## 1. The problem

`apt` inside the container reads the *container's* package database. Running
`apt-get -s upgrade` in the muninn container reports the updates pending for
debian-slim.

The result is not an error. It is a number, and a believable one — it rises over
time and drops after a rebuild, behaving exactly as a correct implementation
would. For a monitoring system that is the worst failure mode there is.

Two constraints made it harder than it sounds. **Telegraf has no package input
plugin** (all 249 checked), so the result has to come from `inputs.exec` running
a helper muninn ships. And **the host's data is not the container's data**, which
is the whole question.

## 2. Method

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
the same `apt-get -s dist-upgrade` the probe runs. The probe is
`spikes/updates/probe.sh`, run from `debian:12-slim`.

The fixture deliberately preserves `/etc/os-release` as a symlink **and**
`/usr/lib/os-release` as its target, because flattening that would have hidden a
real failure mode — see §5.

## 3. Results

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
filesystem. `spikes/updates/fixtures/build-host-native.sh` fetches fresh indices
into a scratch directory — via `Dir::State::lists`, so `/var/lib/apt/lists` is
left untouched; a spike that modified the machine it was measuring would
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

## 4. Approaches B, C and D

**B — host root read-only plus `chroot`. Rejected, fails twice.**

```
--cap-drop=ALL   → chroot: cannot change root directory: Operation not permitted
default caps     → exit 100
                   E: Unable to mkstemp /tmp/clearsigned.message… (Read-only file system)
                   E: The package lists or status file could not be parsed or opened.
```

It needs `CAP_SYS_CHROOT`, which the hardening baseline drops — and even when
granted, apt cannot work inside a read-only chroot because it wants scratch space
the mount does not provide. Making that work would mean an overlay on top of the
host root, which is more machinery than approach A needs in total.

**C — `nsenter` into the host namespaces. Rejected as expected.**

```
--cap-drop=ALL                        → nsenter: reassociate to namespace 'ns/mnt' failed
--pid=host --cap-add=SYS_ADMIN        → works
```

Confirmed empirically rather than assumed: it works only with the host PID
namespace and `CAP_SYS_ADMIN`, both excluded by the hardening requirements. Not a
candidate.

**D — external host helper. Not needed, retained as documentation.**

A systemd timer writing a file muninn reads. Since A works under full hardening,
D is no longer the fallback it was planned as. It stays documented for operators
who will not mount the host filesystem at all.

## 5. Findings worth keeping

**`/etc/os-release` is a symlink to `/usr/lib/os-release`.** The first probe
version read only `/etc/os-release` and reported `os_release_unreadable` for a
plainly Debian host, because a mount that includes `/etc` but not `/usr/lib`
leaves the symlink dangling. The probe now tries both. This is concrete support
for [ADR-0005](../adr/0005-hostfs-mount.md): mounting hand-picked paths breaks in
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

## 6. The cost: the runtime base image

Approach A needs `apt` and `dpkg` in the runtime image, so it cannot be
distroless. Measured with Trivy:

| | `gcr.io/distroless/cc-debian12` | `debian:12-slim` |
|---|---|---|
| Size | 8 MB | 26 MB |
| Packages | 10 | 88 |
| CRITICAL | 0 | 5 |
| HIGH | 0 | 17 |
| MEDIUM | 4 | 57 |
| **Fixable** | **0** | **0** |

Every one of those CVEs is currently unfixable — `will_not_fix`, `affected` or
`fix_deferred` — so the Trivy gate (fixable CRITICAL/HIGH) stays green. Four of
the five CRITICAL are in `perl-base`, which muninn never invokes and which is
present because it is Essential in Debian, not because apt pulls it in.

The sharper cost is qualitative: debian-slim ships a shell and a package manager
inside a container that has the host filesystem mounted. That is a meaningful aid
to anyone who achieves code execution there.

**Decision: a single debian-slim image** (maintainer's call, made with these
numbers in hand). A two-variant scheme — distroless by default, debian-slim for
the updates module — was considered and set aside in favour of one artefact and
one CI path, consistent with the brief's self-contained-container goal.

The mitigations are therefore load-bearing rather than optional, and
`docs/hardening.md` records them: non-root, read-only root filesystem,
`--cap-drop=ALL`, `no-new-privileges`, all verified working with approach A.

## 7. What WP10 implements

The probe in `spikes/updates/probe.sh` is the specification. Its structure —
preconditions first, each failing with a specific low-cardinality reason, then
the simulated upgrade, then counting — is what the Rust implementation mirrors.

```text
muninn_updates_pending{severity="all"}        gauge
muninn_updates_pending{severity="security"}   gauge
muninn_updates_check_success                  gauge  0|1
muninn_updates_check_timestamp_seconds        gauge
muninn_updates_lists_age_seconds              gauge
```

**The invariant, now demonstrated rather than asserted:** a failed check emits
`check_success=0` and omits the pending counts. T8, T9 and T9b are the tests that
hold it.

Open for WP10: whether to keep a shell helper invoked through `inputs.exec`, or
to shell out to `apt-get` from muninn itself and emit the line protocol from
Rust. The spike does not decide this — both run the same apt invocation, which is
the part that had to be proven.
