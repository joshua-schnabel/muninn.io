#!/usr/bin/env bash
# WP1 spike runner — evaluates approach A (read-only host mounts + simulated
# upgrade) against the test matrix in docs/spikes/updates-spike.md.
#
#   bash spikes/updates/run.sh            # full matrix
#   bash spikes/updates/run.sh T8 T9      # selected cells
#
# Simulated hosts are containers with a real Debian/Ubuntu rootfs whose apt
# state is exported and mounted read-only into a probe container. T11 runs the
# same probe against a real host and is the one cell container fixtures cannot
# stand in for.
#
# Requires: docker, bash. T11 additionally requires WSL with a Debian distro.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="${SPIKE_WORK:-$ROOT/spikes/updates/work}"

# The container muninn would ship. If approach A is adopted, this is what the
# runtime image has to be based on.
PROBE_IMAGE="${PROBE_IMAGE:-debian:12-slim}"

# Dated tags rather than :12 / :24.04, because the current images are fully
# patched and would give every cell zero pending updates. An outdated host is
# the interesting case and has to be pinned to stay reproducible.
HOST_DEB12="debian:bookworm-20240211"
HOST_DEB13="debian:trixie-20250428"
HOST_UBU22="ubuntu:jammy-20240227"
HOST_UBU24="ubuntu:noble-20240605"
HOST_DEB12_CURRENT="debian:12"

[ -n "${MSYSTEM:-}" ] && export MSYS_NO_PATHCONV=1
native() { if [ -n "${MSYSTEM:-}" ]; then (cd "$1" && pwd -W); else (cd "$1" && pwd); fi; }

RED=$'\033[0;31m'; GREEN=$'\033[0;32m'; YELLOW=$'\033[1;33m'; DIM=$'\033[2m'; NC=$'\033[0m'
pass_n=0; fail_n=0
declare -a RESULTS

record() { # id  verdict  detail
    RESULTS+=("$1|$2|$3")
    if [ "$2" = PASS ]; then
        pass_n=$((pass_n+1)); echo "  ${GREEN}✓ $1${NC}  $3"
    else
        fail_n=$((fail_n+1)); echo "  ${RED}✗ $1${NC}  $3"
    fi
}

# Build a fixture once and cache it — each build pulls an image and runs
# apt-get update, which is the slow part of the matrix.
fixture() { # image  name  state
    local dir="$WORK/$2"
    if [ ! -f "$dir/meta.txt" ]; then
        # Progress goes to stderr: this function's stdout is the fixture path,
        # captured by the caller.
        echo "  ${DIM}building fixture $2 ($1, $3)...${NC}" >&2
        bash "$ROOT/spikes/updates/fixtures/build-host.sh" "$1" "$dir" "$3" >/dev/null 2>&1 \
            || { echo "  ${RED}fixture build failed: $2${NC}" >&2; return 1; }
    fi
    echo "$dir"
}

meta() { sed -n "s/^$2=//p" "$1/meta.txt"; }

# Run the probe against a fixture's rootfs, optionally from a different image.
probe() { # fixture_dir  [probe_image]
    docker run --rm \
        -v "$(native "$1/rootfs"):/hostfs:ro" \
        -v "$(native "$ROOT/spikes/updates"):/s:ro" \
        "${2:-$PROBE_IMAGE}" sh /s/probe.sh 2>/dev/null
}

field() { echo "$1" | grep -o "$2=[-0-9]*i" | cut -d= -f2 | tr -d i; }
reason() { echo "$1" | grep -o 'reason=[a-z_]*' | cut -d= -f2; }

# ── Cells ────────────────────────────────────────────────────────────────────

t_matches_ground_truth() { # id  image  name  label
    local d; d=$(fixture "$2" "$3" stale) || { record "$1" FAIL "fixture build failed"; return; }
    local want_total want_sec out got_total got_sec
    want_total=$(meta "$d" total); want_sec=$(meta "$d" security)
    out=$(probe "$d")
    got_total=$(field "$out" pending_all); got_sec=$(field "$out" pending_security)
    if [ "$(field "$out" check_success)" != 1 ]; then
        record "$1" FAIL "$4: check failed ($(reason "$out"))"
    elif [ "$got_total" = "$want_total" ] && [ "$got_sec" = "$want_sec" ]; then
        record "$1" PASS "$4: $got_total pending / $got_sec security — matches host"
    else
        record "$1" FAIL "$4: got $got_total/$got_sec, host says $want_total/$want_sec"
    fi
}

T1() { # freshly upgraded host reports nothing pending
    local d; d=$(fixture "$HOST_DEB12_CURRENT" deb12-fresh fresh) || { record T1 FAIL "fixture"; return; }
    local out; out=$(probe "$d")
    local total sec
    total=$(field "$out" pending_all); sec=$(field "$out" pending_security)
    if [ "$(field "$out" check_success)" = 1 ] && [ "$total" = 0 ] && [ "$sec" = 0 ]; then
        record T1 PASS "debian:12 up to date: 0 pending, 0 security, check_success=1"
    else
        record T1 FAIL "expected 0/0 with success, got ${total}/${sec} ($(reason "$out"))"
    fi
}

T2() { t_matches_ground_truth T2 "$HOST_DEB12" deb12-stale "debian:12"; }

T3() { # security count is present, non-zero and bounded by the total
    local d; d=$(fixture "$HOST_DEB12" deb12-stale stale) || { record T3 FAIL "fixture"; return; }
    local out total sec
    out=$(probe "$d"); total=$(field "$out" pending_all); sec=$(field "$out" pending_security)
    if [ -n "$sec" ] && [ "$sec" -gt 0 ] && [ "$sec" -le "$total" ]; then
        record T3 PASS "security $sec of $total, correctly bounded"
    else
        record T3 FAIL "security=$sec total=$total — expected 0 < security <= total"
    fi
}

T4() { t_matches_ground_truth T4 "$HOST_DEB13" deb13-stale "debian:13"; }
T5() { t_matches_ground_truth T5 "$HOST_UBU22" ubu22-stale "ubuntu:22.04"; }
T6() { t_matches_ground_truth T6 "$HOST_UBU24" ubu24-stale "ubuntu:24.04"; }

T7() { # stale package lists: still correct, but the age is reported
    local d; d=$(fixture "$HOST_DEB12" deb12-oldlists oldlists) || { record T7 FAIL "fixture"; return; }
    local out age
    out=$(probe "$d"); age=$(field "$out" lists_age_seconds)
    if [ "$(field "$out" check_success)" = 1 ] && [ -n "$age" ] && [ "$age" -gt 2000000 ]; then
        record T7 PASS "30-day-old lists reported as ${age}s, result still produced"
    else
        record T7 FAIL "lists_age_seconds=$age (expected > 2000000), success=$(field "$out" check_success)"
    fi
}

T8() { # required mount absent — must NOT report zero
    local out
    out=$(docker run --rm -v "$(native "$ROOT/spikes/updates"):/s:ro" "$PROBE_IMAGE" sh /s/probe.sh 2>/dev/null)
    if [ "$(field "$out" check_success)" = 0 ] && ! echo "$out" | grep -q pending_all; then
        record T8 PASS "no mount: check_success=0, reason=$(reason "$out"), pending omitted"
    else
        record T8 FAIL "expected failure without pending fields, got: $out"
    fi
}

T9() { # unreadable / corrupt dpkg status — must NOT report zero
    local d; d=$(fixture "$HOST_DEB12" deb12-stale stale) || { record T9 FAIL "fixture"; return; }
    local broken="$WORK/deb12-corrupt"
    rm -rf "$broken"; mkdir -p "$broken"
    cp -a "$d/rootfs" "$broken/rootfs"
    : > "$broken/rootfs/var/lib/dpkg/status"          # truncate to empty
    local out; out=$(probe "$broken")
    if [ "$(field "$out" check_success)" = 0 ] && ! echo "$out" | grep -q pending_all; then
        record T9 PASS "empty dpkg status: check_success=0, reason=$(reason "$out")"
    else
        record T9 FAIL "expected failure without pending fields, got: $out"
    fi

    # And a structurally corrupt (not merely empty) status file.
    printf 'Package: broken\nthis is not a control file\n\x00\x00' > "$broken/rootfs/var/lib/dpkg/status"
    out=$(probe "$broken")
    if [ "$(field "$out" check_success)" = 0 ]; then
        record T9b PASS "corrupt dpkg status: check_success=0, reason=$(reason "$out")"
    elif [ "$(field "$out" pending_all)" = 0 ]; then
        record T9b FAIL "corrupt status reported as 0 pending — the exact failure this must never have"
    else
        record T9b FAIL "corrupt status produced a count: $out"
    fi
}

T10() { # container distro != host distro — correct, or a detected error; never silently wrong
    local d; d=$(fixture "$HOST_UBU24" ubu24-stale stale) || { record T10 FAIL "fixture"; return; }
    local want_total want_sec out total
    want_total=$(meta "$d" total); want_sec=$(meta "$d" security)
    out=$(probe "$d" "$PROBE_IMAGE")   # debian:12-slim reading an Ubuntu 24.04 host
    total=$(field "$out" pending_all)
    if [ "$(field "$out" check_success)" = 0 ]; then
        record T10 PASS "cross-distro detected and refused: reason=$(reason "$out")"
    elif [ "$total" = "$want_total" ] && [ "$(field "$out" pending_security)" = "$want_sec" ]; then
        record T10 PASS "debian:12-slim reads ubuntu:24.04 correctly: $total/$want_sec"
    else
        record T10 FAIL "silently wrong: got $total, host says $want_total"
    fi
}

T11() { # a real host, not a container fixture
    local distro="${WSL_DISTRO:-Debian}"
    if ! command -v wsl.exe >/dev/null 2>&1; then
        record T11 FAIL "wsl.exe not available — cannot verify against a real host"
        return
    fi

    # Parse apt's summary line ("N upgraded, ...") rather than counting Inst
    # lines: it is present even when the count is zero, so an empty result is
    # unambiguously a broken invocation rather than a genuine zero.
    local truth
    truth=$(wsl.exe -d "$distro" -- bash -c \
        "apt-get -s dist-upgrade 2>/dev/null | sed -n 's/^\([0-9]\+\) upgraded.*/\1/p' | tail -1" \
        2>/dev/null | tr -d '\r')
    if [ -z "$truth" ]; then
        record T11 FAIL "could not obtain a native answer from WSL $distro"
        return
    fi

    local script got total
    script=$(wsl.exe -d "$distro" -- wslpath -u "$(native "$ROOT/spikes/updates")/probe.sh" 2>/dev/null | tr -d '\r')
    got=$(wsl.exe -d "$distro" -- bash -c "HOSTFS=/ sh '$script'" 2>/dev/null | tr -d '\r')
    total=$(field "$got" pending_all)

    if [ "$(field "$got" check_success)" != 1 ]; then
        record T11 FAIL "real host: check failed ($(reason "$got"))"
    elif [ "$total" != "$truth" ]; then
        record T11 FAIL "real host: probe says $total, native says $truth"
    elif [ "$truth" = 0 ]; then
        # Honest about the strength of the evidence: agreeing on zero is
        # agreement, but it does not exercise the counting path. T11b covers it.
        record T11 PASS "in-place on the real host: agrees ($total pending) — host has nothing pending, see T11b"
    else
        record T11 PASS "in-place on the real host: $total pending, matches native apt-get"
    fi
}

T11b() { # the real host, with fresh indices, counted from a container
    # T11 runs the probe *on* the host. This runs it the way muninn actually
    # will — from a container, against the host's filesystem — and against a
    # host with a genuinely non-zero answer, which T11 cannot guarantee.
    local distro="${WSL_DISTRO:-Debian}"
    local d="$WORK/wsl-real"

    if [ ! -f "$d/meta.txt" ]; then
        if ! command -v wsl.exe >/dev/null 2>&1; then
            record T11b FAIL "wsl.exe not available — cannot build a native host fixture"
            return
        fi
        echo "  ${DIM}exporting a fixture from the real WSL $distro host...${NC}" >&2
        local wsl_out wsl_script
        wsl_out=$(wsl.exe -d "$distro" -- wslpath -u "$(native "$WORK")" 2>/dev/null | tr -d '\r')/wsl-real
        wsl_script=$(wsl.exe -d "$distro" -- wslpath -u "$(native "$ROOT/spikes/updates/fixtures")" 2>/dev/null | tr -d '\r')/build-host-native.sh
        wsl.exe -d "$distro" -- bash "$wsl_script" "$wsl_out" >/dev/null 2>&1 \
            || { record T11b FAIL "native fixture export failed"; return; }
    fi

    local want_total want_sec out got_total got_sec
    want_total=$(meta "$d" total); want_sec=$(meta "$d" security)
    out=$(probe "$d")
    got_total=$(field "$out" pending_all); got_sec=$(field "$out" pending_security)

    if [ "$(field "$out" check_success)" != 1 ]; then
        record T11b FAIL "real host from a container: check failed ($(reason "$out"))"
    elif [ "$want_total" = 0 ]; then
        record T11b FAIL "real host has nothing pending even with fresh indices — cell proves nothing"
    elif [ "$got_total" = "$want_total" ] && [ "$got_sec" = "$want_sec" ]; then
        record T11b PASS "real host from a container: $got_total pending / $got_sec security — matches native apt"
    else
        record T11b FAIL "got $got_total/$got_sec, real host says $want_total/$want_sec"
    fi
}


# ── Main ─────────────────────────────────────────────────────────────────────

mkdir -p "$WORK"
cells=("$@")
[ ${#cells[@]} -eq 0 ] && cells=(T1 T2 T3 T4 T5 T6 T7 T8 T9 T10 T11 T11b)

echo "${YELLOW}muninn WP1 — host update spike, approach A${NC}"
echo "probe image: $PROBE_IMAGE"
echo "work dir:    $WORK"
echo

for c in "${cells[@]}"; do
    echo "${YELLOW}$c${NC}"
    "$c"
done

echo
echo "─────────────────────────────────────────────"
printf '%-6s %-6s %s\n' ID VERDICT DETAIL
for r in "${RESULTS[@]}"; do
    IFS='|' read -r id verdict detail <<< "$r"
    printf '%-6s %-6s %s\n' "$id" "$verdict" "$detail"
done
echo "─────────────────────────────────────────────"
echo "${GREEN}${pass_n} passed${NC}, ${RED}${fail_n} failed${NC}"
exit "$fail_n"
