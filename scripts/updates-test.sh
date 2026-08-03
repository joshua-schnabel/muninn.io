#!/usr/bin/env bash
# System tests for the updates module — the shipped image against real hosts.
#
#   bash scripts/updates-test.sh [image]        # default: muninn:dev
#   bash scripts/updates-test.sh muninn:dev S8  # selected cells
#
# Requires Docker, and the image built first: docker build -t muninn:dev .
# S11 additionally requires WSL with a Debian distribution.
#
# # How this differs from the WP1 spike
#
# `spikes/updates/run.sh` measured whether approach A can work, using a shell
# probe. This measures whether the *artefact* does: `muninn update-check` inside
# the runtime image, against the same fixtures, compared against the same ground
# truth. If these two ever disagree, the implementation has drifted from what the
# spike proved — which is the whole reason this is a separate script and not an
# extension of that one.
#
# The fixtures are shared with the spike (spikes/updates/work) so a matrix built
# once serves both. Each one pulls an image and runs apt-get update, which is by
# far the slow part.
#
# # Every run is hardened
#
# Non-root, read-only root filesystem, --cap-drop=ALL, no-new-privileges, and a
# tmpfs for the runtime directory. A test that relaxed any of them would prove
# the module works in a posture nobody ships.

set -uo pipefail

IMAGE="${MUNINN_IMAGE:-muninn:dev}"
case "${1:-}" in
    S*|"") : ;;                       # first argument is a cell, or absent
    *) IMAGE="$1"; shift ;;
esac

TMPFS_OPTS="mode=0700,uid=10001,gid=10001"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${UPDATES_WORK:-${SPIKE_WORK:-$ROOT/spikes/updates/work}}"

[ -n "${MSYSTEM:-}" ] && export MSYS_NO_PATHCONV=1
native() { if [ -n "${MSYSTEM:-}" ]; then (cd "$1" && pwd -W); else (cd "$1" && pwd); fi; }

RED=$'\033[0;31m'; GREEN=$'\033[0;32m'; YELLOW=$'\033[1;33m'; DIM=$'\033[2m'; NC=$'\033[0m'
pass_n=0; fail_n=0

pass() { pass_n=$((pass_n+1)); echo "  ${GREEN}✓ $1${NC}  $2"; }
fail() { fail_n=$((fail_n+1)); echo "  ${RED}✗ $1${NC}  $2"; }

trap 'docker rm -f muninn-updates-test >/dev/null 2>&1 || true' EXIT

# Dated tags, matching the spike: the current images are fully patched and would
# give every cell zero pending updates. An outdated host is the interesting case
# and has to be pinned to stay reproducible.
HOST_DEB12="debian:bookworm-20240211"
HOST_DEB13="debian:trixie-20250428"
HOST_UBU22="ubuntu:jammy-20240227"
HOST_UBU24="ubuntu:noble-20240605"
HOST_DEB12_CURRENT="debian:12"

fixture() { # image  name  state  → prints the fixture directory
    local dir="$WORK/$2"
    if [ ! -f "$dir/meta.txt" ]; then
        echo "  ${DIM}building fixture $2 ($1, $3)...${NC}" >&2
        bash "$ROOT/spikes/updates/fixtures/build-host.sh" "$1" "$dir" "$3" >/dev/null 2>&1 \
            || { echo "  ${RED}fixture build failed: $2${NC}" >&2; return 1; }
    fi
    echo "$dir"
}

meta() { sed -n "s/^$2=//p" "$1/meta.txt"; }

# Run the shipped check against a fixture's rootfs, in the documented posture.
#
# --entrypoint is not needed: the image's entrypoint IS muninn, so `update-check`
# is passed to it exactly as Telegraf's inputs.exec passes it.
check() { # fixture_dir
    docker run --rm \
        --read-only --cap-drop=ALL --security-opt no-new-privileges:true \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        -v "$(native "$1/rootfs"):/hostfs:ro" \
        -e TMPDIR=/run/muninn \
        "$IMAGE" update-check --hostfs /hostfs 2>/dev/null
}

field() { echo "$1" | grep -o "$2=[-0-9]*i" | head -1 | cut -d= -f2 | tr -d i; }
pending() { echo "$1" | grep "severity=$2 " | grep -o 'pending=[0-9]*i' | cut -d= -f2 | tr -d i; }
reason() { echo "$1" | grep -o 'reason=[a-z_]*' | head -1 | cut -d= -f2; }

# ── Cells ────────────────────────────────────────────────────────────────────

matches_ground_truth() { # id  image  fixture-name  label
    local d; d=$(fixture "$2" "$3" stale) || { fail "$1" "fixture build failed"; return; }
    local want_total want_sec out got_total got_sec
    want_total=$(meta "$d" total); want_sec=$(meta "$d" security)
    out=$(check "$d")
    got_total=$(pending "$out" all); got_sec=$(pending "$out" security)
    if [ "$(field "$out" check_success)" != 1 ]; then
        fail "$1" "$4: the check failed ($(reason "$out"))"
    elif [ "$got_total" = "$want_total" ] && [ "$got_sec" = "$want_sec" ]; then
        pass "$1" "$4: $got_total pending / $got_sec security — matches the host's own answer"
    else
        fail "$1" "$4: got ${got_total}/${got_sec}, the host says ${want_total}/${want_sec}"
    fi
}

S1() { # a freshly upgraded host reports nothing pending — and says so
    local d; d=$(fixture "$HOST_DEB12_CURRENT" deb12-fresh fresh) || { fail S1 "fixture"; return; }
    local out; out=$(check "$d")
    if [ "$(field "$out" check_success)" = 1 ] && [ "$(pending "$out" all)" = 0 ]; then
        pass S1 "an up-to-date host: 0 pending with check_success=1 — a zero that means zero"
    else
        fail S1 "expected 0 pending with success, got: $out"
    fi
}

S2() { matches_ground_truth S2 "$HOST_DEB12" deb12-stale "debian:12"; }

S3() { # the security subset is present, non-zero and bounded by the total
    local d; d=$(fixture "$HOST_DEB12" deb12-stale stale) || { fail S3 "fixture"; return; }
    local out total sec
    out=$(check "$d"); total=$(pending "$out" all); sec=$(pending "$out" security)
    if [ -n "$sec" ] && [ "$sec" -gt 0 ] && [ "$sec" -le "$total" ]; then
        pass S3 "security $sec of $total, correctly bounded"
    else
        fail S3 "security=${sec} total=${total} — expected 0 < security <= total"
    fi
}

S4() { matches_ground_truth S4 "$HOST_DEB13" deb13-stale "debian:13"; }
S5() { matches_ground_truth S5 "$HOST_UBU22" ubu22-stale "ubuntu:22.04"; }
S6() { matches_ground_truth S6 "$HOST_UBU24" ubu24-stale "ubuntu:24.04, a different distribution than the image"; }

S7() { # stale package lists: still correct, and the age is reported
    local d; d=$(fixture "$HOST_DEB12" deb12-oldlists oldlists) || { fail S7 "fixture"; return; }
    local out age
    out=$(check "$d"); age=$(field "$out" lists_age_seconds)
    if [ "$(field "$out" check_success)" = 1 ] && [ -n "$age" ] && [ "$age" -gt 2000000 ]; then
        pass S7 "30-day-old indices reported as ${age}s, and the count still produced"
    else
        fail S7 "lists_age_seconds=${age} (expected > 2000000), success=$(field "$out" check_success)"
    fi
}

S8() { # no host mount at all — the invariant, on the artefact
    local out
    out=$(docker run --rm --read-only --cap-drop=ALL \
            --tmpfs "/run/muninn:${TMPFS_OPTS}" -e TMPDIR=/run/muninn \
            "$IMAGE" update-check --hostfs /hostfs 2>/dev/null)
    local rc=$?
    if [ "$rc" != 0 ]; then
        fail S8 "exited ${rc}; a non-zero exit makes Telegraf emit nothing at all"
    elif [ "$(reason "$out")" = "hostfs_not_mounted" ] && ! echo "$out" | grep -q pending; then
        # The image creates /hostfs, so forgetting the mount leaves it empty
        # rather than absent. Naming that specifically is the difference between
        # "mount the host filesystem" and a puzzle about a missing dpkg status.
        pass S8 "no mount: check_success=0, reason=hostfs_not_mounted, and no count at all"
    else
        fail S8 "expected a failure with no counts, got: $out"
    fi
}

S9() { # an empty and then a structurally corrupt dpkg status
    local d; d=$(fixture "$HOST_DEB12" deb12-stale stale) || { fail S9 "fixture"; return; }
    local broken="$WORK/deb12-corrupt-impl"
    rm -rf "$broken"; mkdir -p "$broken"
    cp -a "$d/rootfs" "$broken/rootfs"

    : > "$broken/rootfs/var/lib/dpkg/status"
    local out; out=$(check "$broken")
    if [ "$(field "$out" check_success)" = 0 ] && ! echo "$out" | grep -q pending; then
        pass S9 "empty dpkg status: check_success=0, reason=$(reason "$out")"
    else
        fail S9 "expected a failure with no counts, got: $out"
    fi

    printf 'Package: broken\nthis is not a control file\n\x00\x00' > "$broken/rootfs/var/lib/dpkg/status"
    out=$(check "$broken")
    if [ "$(field "$out" check_success)" = 0 ]; then
        pass S9b "corrupt dpkg status: check_success=0, reason=$(reason "$out")"
    elif [ "$(pending "$out" all)" = 0 ]; then
        fail S9b "reported 0 pending from a corrupt database — the exact failure this must never have"
    else
        fail S9b "a corrupt database produced a count: $out"
    fi
    rm -rf "$broken"
}

S10() { # a host that is not Debian-family gets a refusal, not a number
    local d="$WORK/not-debian"
    rm -rf "$d"; mkdir -p "$d/rootfs/var/lib/dpkg" "$d/rootfs/var/lib/apt/lists" \
                          "$d/rootfs/etc/apt" "$d/rootfs/usr/lib"
    echo "Package: musl" > "$d/rootfs/var/lib/dpkg/status"
    echo "Package: musl" > "$d/rootfs/var/lib/apt/lists/x_Packages"
    printf 'ID=alpine\nVERSION_ID="3.20"\n' > "$d/rootfs/etc/os-release"

    local out; out=$(check "$d")
    if [ "$(reason "$out")" = "host_not_debian_family" ] && ! echo "$out" | grep -q pending; then
        pass S10 "an Alpine host is refused rather than answered with apt's opinion"
    else
        fail S10 "expected host_not_debian_family without counts, got: $out"
    fi
    rm -rf "$d"
}

S11() { # a real host, which no container fixture can stand in for
    local distro="${WSL_DISTRO:-Debian}"
    if ! command -v wsl.exe >/dev/null 2>&1; then
        fail S11 "wsl.exe not available — cannot compare against a real host"
        return
    fi

    # apt's summary line rather than a count of Inst lines: it is present even
    # when the answer is zero, so an empty result is unambiguously a broken
    # invocation rather than a genuine zero.
    local truth
    truth=$(wsl.exe -d "$distro" -- bash -c \
        "apt-get -s dist-upgrade 2>/dev/null | sed -n 's/^\([0-9]\+\) upgraded.*/\1/p' | tail -1" \
        2>/dev/null | tr -d '\r')
    if [ -z "$truth" ]; then
        fail S11 "could not obtain a native answer from WSL ${distro}"
        return
    fi

    # The host's state, exported the way a /:/hostfs:ro mount presents it. Shared
    # with the spike's T11b fixture, and built by the same script — it runs
    # inside WSL, on the machine it is measuring, and fetches fresh indices into
    # a scratch directory so the host's own apt state is left untouched.
    local d="$WORK/wsl-real"
    if [ ! -f "$d/meta.txt" ]; then
        echo "  ${DIM}exporting a fixture from the real WSL ${distro} host...${NC}" >&2
        local wsl_out wsl_script
        wsl_out=$(wsl.exe -d "$distro" -- wslpath -u "$(native "$WORK")" 2>/dev/null | tr -d '\r')/wsl-real
        wsl_script=$(wsl.exe -d "$distro" -- wslpath -u "$(native "$ROOT/spikes/updates/fixtures")" 2>/dev/null | tr -d '\r')/build-host-native.sh
        wsl.exe -d "$distro" -- bash "$wsl_script" "$wsl_out" >/dev/null 2>&1 \
            || { fail S11 "could not export the WSL host's state"; return; }
    fi

    local out got want_total want_sec got_sec
    want_total=$(meta "$d" total); want_sec=$(meta "$d" security)
    out=$(check "$d"); got=$(pending "$out" all); got_sec=$(pending "$out" security)

    if [ "$(field "$out" check_success)" != 1 ]; then
        fail S11 "real host: the check failed ($(reason "$out"))"
    elif [ "$got" = "$want_total" ] && [ "$got_sec" = "$want_sec" ]; then
        # The fixture's indices are fresher than the host's own, so its answer is
        # the one to compare against; the live number is reported for context.
        pass S11 "real WSL ${distro} host from a container: ${got} pending / ${got_sec} security \
— identical to that host's own apt (live, with its older indices: ${truth})"
    else
        fail S11 "real host: got ${got}/${got_sec}, the host itself says ${want_total}/${want_sec}"
    fi
}

# The configuration the two end-to-end cells run.
#
# Only the updates module is enabled, and that is not a shortcut: the "host" here
# is an exported apt and dpkg tree with no /proc in it, so a CPU module would
# correctly refuse to start against it. What is under test is the updates path.
write_e2e_config() { # outfile  hostname
    cat > "$1" <<YAML
version: 1
agent:
  interval: 1s
  flush_interval: 1s
  hostname: "$2"
runtime:
  shutdown_grace_period: 8s
  telegraf_start_timeout: 20s
  host_mount_prefix: /hostfs
logging:
  format: json
  level: info
health:
  listen: "0.0.0.0:8080"
modules:
  updates:
    enabled: true
    interval: 1m
outputs:
  prometheus:
    enabled: true
    listen: "0.0.0.0:9273"
YAML
}

S12() { # end to end: the module enabled, in a running container
    local d; d=$(fixture "$HOST_DEB12" deb12-stale stale) || { fail S12 "fixture"; return; }
    local want; want=$(meta "$d" total)
    write_e2e_config "$WORK/muninn-updates.yaml" updates-test

    docker rm -f muninn-updates-test >/dev/null 2>&1
    # The fixture rootfs is the host: the module reads the same /hostfs every
    # other module does, so this is the real path rather than a bypass.
    docker run -d --name muninn-updates-test \
        --read-only --cap-drop=ALL --security-opt no-new-privileges:true \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        -v "$(native "$WORK")/muninn-updates.yaml:/etc/muninn/muninn.yaml:ro" \
        -v "$(native "$d/rootfs"):/hostfs:ro" \
        -p 18081:8080 -p 19274:9273 \
        "$IMAGE" >/dev/null 2>&1

    local deadline=$(( SECONDS + 90 )) ready=0
    while [ "$SECONDS" -lt "$deadline" ]; do
        curl -sf http://localhost:18081/health/ready >/dev/null 2>&1 && { ready=1; break; }
        sleep 1
    done
    if [ "$ready" != 1 ]; then
        fail S12 "never became ready"
        docker logs muninn-updates-test 2>&1 | tail -20
        docker rm -f muninn-updates-test >/dev/null 2>&1
        return
    fi

    # muninn's own view, recorded by the startup check. That check runs *after*
    # readiness — a full apt resolution takes seconds, and holding readiness for
    # it would delay an orchestrator over something unrelated to collection — so
    # this waits for the result rather than assuming it has already landed.
    local deadline1=$(( SECONDS + 60 )) checked=0
    while [ "$SECONDS" -lt "$deadline1" ]; do
        curl -sf http://localhost:18081/metrics 2>/dev/null \
            | grep -q 'muninn_module_check_success{module="updates"} 1' && { checked=1; break; }
        sleep 2
    done
    if [ "$checked" = 1 ]; then
        pass S12 "the startup check succeeded and is visible on /metrics"
    else
        fail S12 "muninn_module_check_success{module=\"updates\"} never reached 1"
        curl -s http://localhost:18081/metrics 2>/dev/null | grep muninn_module || true
        docker logs muninn-updates-test 2>&1 | grep -i update | tail -5 || true
    fi

    # Telegraf's view, through inputs.exec — the path an operator actually reads.
    # The module runs on its own interval, so the first sample is one interval
    # away plus Telegraf's alignment — this waits for a real collection cycle.
    local deadline2=$(( SECONDS + 150 )) got=""
    while [ "$SECONDS" -lt "$deadline2" ]; do
        got=$(curl -sf http://localhost:19274/metrics 2>/dev/null \
              | sed -n 's/^muninn_updates_pending{.*severity="all".*} \([0-9]*\).*/\1/p' | head -1)
        [ -n "$got" ] && break
        sleep 2
    done

    if [ -z "$got" ]; then
        fail S12b "no muninn_updates_pending on the Telegraf endpoint within 150s"
        docker logs muninn-updates-test 2>&1 | tail -20
    elif [ "$got" = "$want" ]; then
        pass S12b "muninn_updates_pending{severity=\"all\"} = ${got}, the host's own answer"
    else
        fail S12b "muninn_updates_pending is ${got}, the host says ${want}"
    fi

    # Tagged with status and reason, so it is matched by prefix rather than by a
    # bare name — the labels are part of the contract.
    local success
    success=$(curl -sf http://localhost:19274/metrics 2>/dev/null \
              | sed -n 's/^muninn_updates_check_success{.*} \([0-9]*\).*/\1/p' | head -1)
    if [ "$success" = 1 ]; then
        pass S12c "muninn_updates_check_success is exported alongside the count"
    else
        fail S12c "muninn_updates_check_success is '${success}' on the Telegraf endpoint"
    fi

    docker rm -f muninn-updates-test >/dev/null 2>&1
}

S13() { # a failing module degrades muninn; it does not stop it
    local conf="$WORK/muninn-updates-broken.yaml"
    local d="$WORK/degraded-host"
    rm -rf "$d"; mkdir -p "$d/rootfs/var/lib/dpkg" "$d/rootfs/var/lib/apt/lists" \
                          "$d/rootfs/etc/apt" "$d/rootfs/usr/lib"
    # A Debian host whose package database is unreadable: the preconditions pass
    # far enough to be a real deployment, and then the check cannot answer.
    : > "$d/rootfs/var/lib/dpkg/status"
    echo "Package: x" > "$d/rootfs/var/lib/apt/lists/x_Packages"
    printf 'ID=debian\nVERSION_ID="12"\n' > "$d/rootfs/etc/os-release"

    write_e2e_config "$conf" updates-degraded

    docker rm -f muninn-updates-test >/dev/null 2>&1
    docker run -d --name muninn-updates-test \
        --read-only --cap-drop=ALL --security-opt no-new-privileges:true \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        -v "$(native "$WORK")/muninn-updates-broken.yaml:/etc/muninn/muninn.yaml:ro" \
        -v "$(native "$d/rootfs"):/hostfs:ro" \
        -p 18081:8080 -p 19274:9273 \
        "$IMAGE" >/dev/null 2>&1

    local deadline=$(( SECONDS + 90 )) ready=0
    while [ "$SECONDS" -lt "$deadline" ]; do
        curl -sf http://localhost:18081/health/ready >/dev/null 2>&1 && { ready=1; break; }
        sleep 1
    done

    if [ "$ready" != 1 ]; then
        fail S13 "a failing updates module took the agent out of service"
        docker logs muninn-updates-test 2>&1 | tail -20
    else
        local metrics; metrics=$(curl -s http://localhost:18081/metrics 2>/dev/null)
        if echo "$metrics" | grep -q 'muninn_module_check_success{module="updates"} 0'; then
            pass S13 "still ready, and the failure is reported rather than hidden"
        else
            fail S13 "ready, but the failed check is not visible on /metrics"
            echo "$metrics" | grep muninn_module || true
        fi
        local status; status=$(curl -s http://localhost:18081/status 2>/dev/null)
        if echo "$status" | grep -q '"degraded"'; then
            pass S13b "/status reports degraded — a working agent with one module down"
        else
            fail S13b "/status does not report degraded: $status"
        fi
    fi

    docker rm -f muninn-updates-test >/dev/null 2>&1
    rm -rf "$d"
}

# ── Run ──────────────────────────────────────────────────────────────────────

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "${RED}image ${IMAGE} not found — build it first: docker build -t ${IMAGE} .${NC}" >&2
    exit 2
fi

CELLS=("$@")
[ ${#CELLS[@]} -eq 0 ] && CELLS=(S1 S2 S3 S4 S5 S6 S7 S8 S9 S10 S11 S12 S13)

echo "updates system tests against ${IMAGE}"
echo "fixtures in ${WORK}"
echo
for cell in "${CELLS[@]}"; do
    if declare -F "$cell" >/dev/null; then
        "$cell"
    else
        fail "$cell" "no such cell"
    fi
done

echo
echo "${GREEN}${pass_n} passed${NC}, ${RED}${fail_n} failed${NC}"
[ "$fail_n" -eq 0 ]
