# Spike — reading the host's pending package updates from a container

**Status:** planned (WP1) · **Decides:** [ADR-0009](../adr/0009-updates-module-approach.md)
and the runtime base image · **Blocks:** WP8, WP10

This is the highest-risk piece of muninn. It runs before the Dockerfile is
written, because its outcome decides whether the runtime image can be distroless
or has to carry apt and dpkg.

## 1. The problem

`apt` inside the muninn container reads the *container's* package database.
`apt-get -s upgrade` there reports the updates pending for debian-slim.

The result is not an error. It is a number, and a believable one — a small
integer that rises over time and drops after a rebuild, behaving exactly as a
correct implementation would. For a monitoring system this is the worst class of
bug there is: silently wrong, indistinguishable from right, and only detectable
by someone who thinks to check the host by hand.

Two constraints make it harder than it sounds.

**There is no plugin.** All 249 Telegraf 1.39.2 input plugins were checked;
nothing covers apt, dpkg or package updates. The result has to reach Telegraf
through `inputs.exec` running a helper muninn ships, emitting influx line
protocol on stdout. There is no well-trodden path to copy.

**The host's data is not the container's data**, and bridging that gap without
privileges is the entire question.

## 2. Approaches

### A — Read-only host mounts, simulated upgrade

Mount the host's apt state read-only and redirect apt at it:

```bash
apt-get -s upgrade \
  -o Dir::State::status=/hostfs/var/lib/dpkg/status \
  -o Dir::Etc::sourcelist=/hostfs/etc/apt/sources.list \
  -o Dir::Etc::sourceparts=/hostfs/etc/apt/sources.list.d \
  -o Dir::State::lists=/hostfs/var/lib/apt/lists \
  -o Dir::Cache=/tmp/muninn-apt-cache
```

Security updates come from the candidate version's origin suite (`*-security`),
read via `apt-cache policy` or the `InRelease` files under `lists/`.

**To establish**

- Where does apt write despite `-s`? Which of those can be redirected to a tmpfs
  and which cannot?
- Is the container's apt able to read the host's `lists/` format? That format has
  changed across releases; a Debian 12 container reading Ubuntu 24.04 lists is the
  interesting case.
- Is security-update classification reliable, or does it depend on repository
  metadata not all hosts have?
- What is the minimum mount set? Every path in it is host filesystem exposure.

**If chosen:** apt and dpkg must be in the runtime image → debian-slim, not
distroless. This is the expensive consequence, and the reason the spike runs
first.

### B — Host root read-only plus `chroot`

```bash
chroot /hostfs apt-get -s upgrade
```

**To establish** — required capabilities (`CAP_SYS_CHROOT` at minimum); whether
apt functions at all in a read-only chroot, given it wants
`/var/lib/apt/lists/partial` and a cache directory; whether an overlay on top of
the read-only mount is workable; which binaries and shared libraries must exist
in the host root, which is not guaranteed on a minimal host.

### C — `nsenter` into the host namespaces

Requires `--pid=host` plus `CAP_SYS_ADMIN` or `CAP_SYS_PTRACE`. This contradicts
the hardening requirements directly.

Evaluated for the record only, so the rejection is documented rather than
assumed. It will not be the default.

### D — External host helper

A systemd timer on the host writes a small file muninn reads read-only:

```
/var/lib/muninn/updates.prom   (or line protocol)
```

Contradicts the self-contained container goal — and is the only approach
guaranteed correct, because the check runs where the packages are. Kept as the
documented fallback if A and B both fail.

## 3. Test matrix

Simulated hosts are containers with a real Debian or Ubuntu rootfs, whose apt
directories are mounted read-only into the muninn test container. That keeps the
matrix reproducible and runnable in CI. **T11 checks the same thing against a
real host** (WSL Debian), which is what the brief's "compare against a native
check" criterion actually requires — container fixtures alone cannot satisfy it.

| # | Host | State | Expected |
|---|---|---|---|
| T1 | debian:12 | freshly upgraded | `0` pending, `0` security, `check_success=1` |
| T2 | debian:12 | ≥1 update available | count **identical** to native `apt-get -s upgrade` |
| T3 | debian:12 | security update available | `security > 0` and `security ≤ total` |
| T4 | debian:13 | as T2 | same — format compatibility across a major release |
| T5 | ubuntu:22.04 | as T2 | same |
| T6 | ubuntu:24.04 | as T2 | same |
| T7 | any | package lists older than 7 days | `lists_age_seconds` reported, result flagged stale |
| T8 | any | a required mount is absent | `check_success=0` — **never `0` pending** |
| T9 | any | dpkg status unreadable or corrupt | `check_success=0`, error names the path, never the contents |
| T10 | mixed | debian:12 container against an ubuntu:24.04 "host" | correct **or** a detected error — never silently wrong |
| T11 | WSL Debian | real host | result equals `apt-get -s upgrade` on that host |

T2 and T11 establish that it works. **T8, T9 and T10 are the ones that matter
most**: they establish that when it does not work, you can tell. A module that is
right 90 % of the time and confidently wrong the rest is worse than one that
refuses to answer.

T10 deserves particular attention. Container and host will often be different
distributions — that is the normal case, not the exotic one, since the muninn
image has a fixed base and hosts do not.

## 4. Metrics

```text
muninn_updates_pending{severity="all"}        gauge
muninn_updates_pending{severity="security"}   gauge
muninn_updates_check_success                  gauge  0|1
muninn_updates_check_timestamp_seconds        gauge
muninn_updates_lists_age_seconds              gauge
```

**The invariant.** A failed check emits `check_success=0` and **omits** the
pending gauges. It never emits zero. "No updates pending" and "I could not look"
are opposite conclusions and must not share a representation — an alert rule
cannot distinguish them afterwards.

A failure here produces `Degraded`, not `Failed`: the configuration is valid and
Telegraf keeps collecting everything else. The failure stays visible in the logs,
in `/status`, and in `check_success`.

## 5. Acceptance criteria

An approach may be adopted only if all hold:

1. It works reproducibly across Debian 12/13 and Ubuntu 22.04/24.04.
2. No host package data is modified. Verified by checksumming the mounted host
   paths before and after.
3. Failures are unambiguous (T8, T9, T10).
4. The result is verified against a native check on a real host (T11).
5. Required mounts and permissions are documented per module.
6. It does not require capabilities the hardening baseline excludes.

**If none qualifies:** the module ships marked `experimental`, disabled by
default, checking its preconditions hard when enabled and failing loudly when
they are absent. Approach D is documented as the supported path for operators who
need the metric to be trustworthy.

## 6. Deliverables

| Artefact | Content |
|---|---|
| `spikes/updates/run.sh` | Runs the whole matrix; reproducible |
| `spikes/updates/fixtures/` | Rootfs preparation per distribution and state |
| `docs/spikes/updates-spike.md` | This file, extended with a result per cell |
| `docs/adr/0009-updates-module-approach.md` | Finalised from "proposed" |
| `docs/hardening.md` | Base-image consequence and its security assessment |

## 7. Time box

Two days of work. If no approach has satisfied the acceptance criteria by then,
that is itself the result: record the findings, ship the module as experimental,
document approach D, and move on. The rest of the MVP does not depend on this
metric, and an open-ended investigation into apt internals is not a good trade
against the twelve other work packages.
