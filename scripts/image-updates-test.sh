#!/usr/bin/env bash
# System tests for the image_updates module — the shipped image against a real
# Docker daemon.
#
#   bash scripts/image-updates-test.sh [image]        # default: muninn:dev
#   bash scripts/image-updates-test.sh muninn:dev I4  # selected cells
#
# Requires Docker, the image built first (docker build -t muninn:dev .), and
# network access to Docker Hub for every cell that resolves a tag.
#
# # What this measures
#
# The unit tests script the daemon's answers, which is what makes the verdict
# logic testable at all — but a scripted daemon agrees with whatever the code
# expects. This suite runs `muninn image-check` inside the runtime image against
# the *real* Engine API, on containers it creates itself, and compares the
# verdict against an answer known in advance.
#
# The `update_available=1` cell is the one that matters most, and it does not
# wait for a registry to publish something. It re-tags an old, pinned image as
# `alpine:latest` locally: the daemon then reports a container running
# `alpine:latest` whose recorded digest is 3.19's, and the registry's answer for
# that tag is certainly something else. A known-stale container, built on
# demand, with no push and nothing to wait for.
#
# # Every run is hardened
#
# Non-root, read-only root filesystem, --cap-drop=ALL, no-new-privileges, and a
# tmpfs for the runtime directory — the same posture updates-test.sh uses, for
# the same reason: a test that relaxed any of them would prove the module works
# in a posture nobody ships. The one addition is --group-add for the socket's
# group, because a non-root user cannot read a 0660 root:docker socket. That is
# a real deployment requirement, not a test concession; it is in
# docs/modules.md#docker.

set -uo pipefail

IMAGE="${MUNINN_IMAGE:-muninn:dev}"
case "${1:-}" in
    I*|"") : ;;                       # first argument is a cell, or absent
    *) IMAGE="$1"; shift ;;
esac

TMPFS_OPTS="mode=0700,uid=10001,gid=10001"
SOCKET="${DOCKER_SOCKET:-/var/run/docker.sock}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${IMAGE_UPDATES_WORK:-$ROOT/.fixtures/image-updates}"
mkdir -p "$WORK"

[ -n "${MSYSTEM:-}" ] && export MSYS_NO_PATHCONV=1
native() { if [ -n "${MSYSTEM:-}" ]; then (cd "$1" && pwd -W); else (cd "$1" && pwd); fi; }

RED=$'\033[0;31m'; GREEN=$'\033[0;32m'; YELLOW=$'\033[1;33m'; NC=$'\033[0m'
pass_n=0; fail_n=0; skip_n=0

pass() { pass_n=$((pass_n+1)); echo "  ${GREEN}✓ $1${NC}  $2"; }
fail() { fail_n=$((fail_n+1)); echo "  ${RED}✗ $1${NC}  $2"; }
# A cell whose *precondition* is absent, as opposed to one that failed. Counted
# separately and printed loudly, because a skip that reads like a pass is worse
# than no cell at all.
skip() { skip_n=$((skip_n+1)); echo "  ${YELLOW}– $1${NC}  skipped: $2"; }

# Pinned rather than floating. `alpine:latest` moves, and a cell whose expected
# answer moves with it is a cell that proves nothing on the day it breaks.
OLD_TAG="alpine:3.19"
PREFIX="muninn-iu-test"

cleanup() {
    docker rm -f "${PREFIX}-agent" >/dev/null 2>&1
    docker ps -aq --filter "name=^${PREFIX}-c" | while read -r id; do
        docker rm -f "$id" >/dev/null 2>&1
    done
    docker rmi -f "${PREFIX}/local:v1" >/dev/null 2>&1
}
trap cleanup EXIT

# ── Helpers ──────────────────────────────────────────────────────────────────

# The socket's group, so a non-root container can read it. Absent on Docker
# Desktop's VM socket, where the mount is world-readable and the flag is not
# needed.
socket_group() {
    stat -c %g "$SOCKET" 2>/dev/null || true
}

# `muninn image-check` in the shipped image, hardened, against the real daemon.
check() {
    local gid; gid=$(socket_group)
    local group_flag=()
    [ -n "$gid" ] && [ "$gid" != "0" ] && group_flag=(--group-add "$gid")

    docker run --rm \
        --read-only --cap-drop=ALL --security-opt no-new-privileges:true \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        "${group_flag[@]}" \
        -v "${SOCKET}:/var/run/docker.sock:ro" \
        "$IMAGE" image-check --endpoint unix:///var/run/docker.sock "$@" 2>/dev/null
}

# The line for one container, out of the whole report.
line_for() { # output  container_name
    echo "$1" | grep "container_name=$2," | head -1
}

# One field or tag out of an influx line.
field() { # line  key
    echo "$1" | sed -n "s/.*[ ,]$2=\([^,i ]*\)i\?.*/\1/p" | head -1
}

start_container() { # name  image  [extra docker run args...]
    local name="$1" image="$2"; shift 2
    docker rm -f "$name" >/dev/null 2>&1
    docker run -d --name "$name" "$@" "$image" sleep 3600 >/dev/null 2>&1
}

# Everything below needs the daemon, and most of it needs Docker Hub. Checked
# once, loudly, rather than as five identical failures.
preflight() {
    docker info >/dev/null 2>&1 || { echo "no reachable Docker daemon"; return 1; }
    docker pull -q "$OLD_TAG" >/dev/null 2>&1 || { echo "cannot pull ${OLD_TAG}"; return 1; }
    return 0
}

# ── Cells ────────────────────────────────────────────────────────────────────

I1() { # a daemon that is not there is reported, not crashed into
    local out rc
    out=$(docker run --rm "$IMAGE" image-check \
        --endpoint tcp://127.0.0.1:1 --timeout-secs 2 2>/dev/null); rc=$?
    if [ "$rc" != 0 ]; then
        fail I1 "image-check must exit 0 even when the daemon is unreachable (got $rc)"
    elif echo "$out" | grep -q 'muninn_image_updates,status=error,reason=docker_unreachable' \
         && ! echo "$out" | grep -q 'muninn_container_image_updates'; then
        pass I1 "an unreachable daemon reports check_success=0 and no container lines"
    else
        fail I1 "expected reason=docker_unreachable with no container lines, got: $out"
    fi
}

I2() { # a typo in the endpoint is data, not a panic
    local out
    out=$(docker run --rm "$IMAGE" image-check \
        --endpoint /var/run/docker.sock 2>/dev/null)
    if echo "$out" | grep -q 'reason=invalid_endpoint'; then
        pass I2 "an endpoint with no scheme reports invalid_endpoint"
    else
        fail I2 "expected reason=invalid_endpoint, got: $out"
    fi
}

I3() { # a container on a pinned tag the registry still serves is up to date
    start_container "${PREFIX}-c-current" "$OLD_TAG" || { fail I3 "could not start"; return; }
    local out line
    out=$(check --include "${PREFIX}-c-current")
    line=$(line_for "$out" "${PREFIX}-c-current")

    if [ "$(field "$line" check_success)" != 1 ]; then
        fail I3 "the check failed: $line"
    elif echo "$out" | grep -q "container_name=${PREFIX}-c-current update_available=0"; then
        pass I3 "a container on ${OLD_TAG} — a tag the registry has not moved — reports 0"
    else
        fail I3 "expected update_available=0, got: $out"
    fi
    docker rm -f "${PREFIX}-c-current" >/dev/null 2>&1
}

I4() { # THE cell: a container whose tag has moved reports 1, against a known answer
    # `alpine:latest` locally re-pointed at 3.19. The daemon reports the
    # container as running `alpine:latest`; the image it is actually running
    # records 3.19's digest; the registry's answer for `latest` is certainly
    # something else. No push, no waiting, and the expected answer is known.
    docker tag "$OLD_TAG" alpine:latest >/dev/null 2>&1 \
        || { fail I4 "could not re-tag ${OLD_TAG}"; return; }
    start_container "${PREFIX}-c-stale" alpine:latest || { fail I4 "could not start"; return; }

    local out
    out=$(check --include "${PREFIX}-c-stale")
    if echo "$out" | grep -q "container_name=${PREFIX}-c-stale update_available=1"; then
        pass I4 "a container running a tag that has since moved reports an available update"
    else
        fail I4 "expected update_available=1 for a deliberately stale container, got: $out"
    fi
    docker rm -f "${PREFIX}-c-stale" >/dev/null 2>&1
}

I5() { # a locally built image says why it cannot be judged, never "up to date"
    local d="$WORK/local-image"
    mkdir -p "$d"
    printf 'FROM %s\nRUN true\n' "$OLD_TAG" > "$d/Dockerfile"
    docker build -q -t "${PREFIX}/local:v1" "$d" >/dev/null 2>&1 \
        || { fail I5 "could not build a local image"; return; }
    start_container "${PREFIX}-c-local" "${PREFIX}/local:v1" \
        || { fail I5 "could not start"; return; }

    local out line
    out=$(check --include "${PREFIX}-c-local")
    line=$(line_for "$out" "${PREFIX}-c-local")

    if echo "$line" | grep -q 'reason=no_repo_digest' \
       && ! echo "$out" | grep -q "container_name=${PREFIX}-c-local update_available"; then
        pass I5 "an image that was never pulled reports no_repo_digest and NO verdict"
    else
        fail I5 "expected no_repo_digest without a verdict, got: $out"
    fi
    docker rm -f "${PREFIX}-c-local" >/dev/null 2>&1
}

I6() { # a digest-pinned container has no tag for anything to appear under
    local digest
    digest=$(docker inspect --format '{{index .RepoDigests 0}}' "$OLD_TAG" 2>/dev/null)
    if [ -z "$digest" ]; then
        skip I6 "${OLD_TAG} has no RepoDigests — it was not pulled from a registry"
        return
    fi
    start_container "${PREFIX}-c-pinned" "$digest" || { fail I6 "could not start"; return; }

    local out line
    out=$(check --include "${PREFIX}-c-pinned")
    line=$(line_for "$out" "${PREFIX}-c-pinned")
    if echo "$line" | grep -q 'reason=digest_pinned_reference'; then
        pass I6 "a container pinned to a digest reports digest_pinned_reference"
    else
        fail I6 "expected digest_pinned_reference, got: $out"
    fi
    docker rm -f "${PREFIX}-c-pinned" >/dev/null 2>&1
}

I7() { # the regression cell for repository normalisation
    # A container created as `docker.io/library/alpine:3.19` runs an image whose
    # RepoDigests records the familiar `alpine@sha256:...`. Compared literally
    # those do not match and this reports no_matching_repo_digest — a completely
    # ordinary container the module claims it cannot judge.
    start_container "${PREFIX}-c-fq" "docker.io/library/${OLD_TAG}" \
        || { fail I7 "could not start"; return; }

    local out line
    out=$(check --include "${PREFIX}-c-fq")
    line=$(line_for "$out" "${PREFIX}-c-fq")

    if echo "$line" | grep -q 'reason=no_matching_repo_digest'; then
        fail I7 "a fully qualified Docker Hub reference was not normalised: $line"
    elif [ "$(field "$line" check_success)" = 1 ]; then
        pass I7 "docker.io/library/${OLD_TAG} is judged, not dismissed as a different repository"
    else
        fail I7 "expected a verdict for a fully qualified reference, got: $out"
    fi
    docker rm -f "${PREFIX}-c-fq" >/dev/null 2>&1
}

I8() { # include and exclude decide what is even asked about
    start_container "${PREFIX}-c-in" "$OLD_TAG" || { fail I8 "could not start"; return; }
    start_container "${PREFIX}-c-out" "$OLD_TAG" || { fail I8 "could not start"; return; }

    local out
    out=$(check --include "${PREFIX}-c-*" --exclude "${PREFIX}-c-out")
    if echo "$out" | grep -q "container_name=${PREFIX}-c-in" \
       && ! echo "$out" | grep -q "container_name=${PREFIX}-c-out"; then
        pass I8 "an exclude pattern removes a container from an included set"
    else
        fail I8 "expected c-in present and c-out absent, got: $out"
    fi
    docker rm -f "${PREFIX}-c-in" "${PREFIX}-c-out" >/dev/null 2>&1
}

I9() { # an exhausted budget reports the containers it did not reach
    start_container "${PREFIX}-c-budget" "$OLD_TAG" || { fail I9 "could not start"; return; }

    local out line
    out=$(check --include "${PREFIX}-c-budget" --budget-secs 0)
    line=$(line_for "$out" "${PREFIX}-c-budget")

    # The daemon-level check still succeeded — the containers were listed. What
    # failed is per container, which is the whole point: Telegraf killing the
    # helper would have produced no line at all.
    if echo "$out" | grep -q 'muninn_image_updates,status=ok' \
       && echo "$line" | grep -q 'reason=budget_exceeded' \
       && ! echo "$out" | grep -q 'update_available'; then
        pass I9 "a container not reached within the budget is reported, not silently dropped"
    else
        fail I9 "expected reason=budget_exceeded with no verdict, got: $out"
    fi
    docker rm -f "${PREFIX}-c-budget" >/dev/null 2>&1
}

I10() { # end to end: the module enabled, in a running agent, through Telegraf
    docker tag "$OLD_TAG" alpine:latest >/dev/null 2>&1
    start_container "${PREFIX}-c-e2e" alpine:latest || { fail I10 "could not start"; return; }

    cat > "$WORK/muninn-image-updates.yaml" <<YAML
version: 1
agent:
  interval: 1s
  flush_interval: 1s
  hostname: "image-updates-test"
runtime:
  shutdown_grace_period: 8s
  telegraf_start_timeout: 20s
logging:
  format: json
  level: info
health:
  listen: "0.0.0.0:8080"
modules:
  image_updates:
    enabled: true
    interval: 1m
    container_include: ["${PREFIX}-c-e2e"]
outputs:
  prometheus:
    enabled: true
    listen: "0.0.0.0:9273"
YAML

    local gid; gid=$(socket_group)
    local group_flag=()
    [ -n "$gid" ] && [ "$gid" != "0" ] && group_flag=(--group-add "$gid")

    docker rm -f "${PREFIX}-agent" >/dev/null 2>&1
    docker run -d --name "${PREFIX}-agent" \
        --read-only --cap-drop=ALL --security-opt no-new-privileges:true \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        "${group_flag[@]}" \
        -v "$(native "$WORK")/muninn-image-updates.yaml:/etc/muninn/muninn.yaml:ro" \
        -v "${SOCKET}:/var/run/docker.sock:ro" \
        -p 18082:8080 -p 19275:9273 \
        "$IMAGE" >/dev/null 2>&1

    local deadline=$(( SECONDS + 90 )) ready=0
    while [ "$SECONDS" -lt "$deadline" ]; do
        curl -sf http://localhost:18082/health/ready >/dev/null 2>&1 && { ready=1; break; }
        sleep 1
    done
    if [ "$ready" != 1 ]; then
        fail I10 "never became ready"
        docker logs "${PREFIX}-agent" 2>&1 | tail -20
        docker rm -f "${PREFIX}-agent" >/dev/null 2>&1
        docker rm -f "${PREFIX}-c-e2e" >/dev/null 2>&1
        return
    fi

    # The startup check runs *after* readiness, so this waits for its result
    # rather than assuming it has already landed — the same shape as S12.
    local d1=$(( SECONDS + 60 )) checked=0
    while [ "$SECONDS" -lt "$d1" ]; do
        curl -sf http://localhost:18082/metrics 2>/dev/null \
            | grep -q 'muninn_module_check_success{module="image_updates"} 1' && { checked=1; break; }
        sleep 1
    done
    if [ "$checked" != 1 ]; then
        fail I10 "muninn_module_check_success{module=\"image_updates\"} never reached 1"
        docker logs "${PREFIX}-agent" 2>&1 | tail -20
    else
        pass I10 "the startup check records a successful daemon-level check"
    fi

    # And the verdict itself, on the Telegraf endpoint, which is where an
    # operator's alert rule reads it.
    local d2=$(( SECONDS + 150 )) got=""
    while [ "$SECONDS" -lt "$d2" ]; do
        got=$(curl -sf http://localhost:19275/metrics 2>/dev/null \
              | sed -n "s/^muninn_container_image_updates_update_available{.*container_name=\"${PREFIX}-c-e2e\".*} \([0-9]*\).*/\1/p" | head -1)
        [ -n "$got" ] && break
        sleep 2
    done
    if [ "$got" = "1" ]; then
        pass I10b "muninn_container_image_updates_update_available = 1 on the Telegraf endpoint"
    elif [ -n "$got" ]; then
        fail I10b "expected update_available=1 for the deliberately stale container, got ${got}"
    else
        fail I10b "no muninn_container_image_updates_update_available within 150s"
        docker logs "${PREFIX}-agent" 2>&1 | tail -20
    fi

    docker rm -f "${PREFIX}-agent" >/dev/null 2>&1
    docker rm -f "${PREFIX}-c-e2e" >/dev/null 2>&1
}

# ── Run ──────────────────────────────────────────────────────────────────────

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "${RED}image ${IMAGE} not found — build it first: docker build -t ${IMAGE} .${NC}" >&2
    exit 2
fi

echo "image_updates system tests against ${IMAGE}"
echo "work directory ${WORK}"
echo

if ! reason=$(preflight); then
    echo "${YELLOW}every cell needs a working daemon and Docker Hub: ${reason}${NC}" >&2
    exit 2
fi

CELLS=("$@")
[ ${#CELLS[@]} -eq 0 ] && CELLS=(I1 I2 I3 I4 I5 I6 I7 I8 I9 I10)

for cell in "${CELLS[@]}"; do
    if declare -F "$cell" >/dev/null; then
        "$cell"
    else
        fail "$cell" "no such cell"
    fi
done

echo
if [ "$skip_n" -gt 0 ]; then
    echo "${GREEN}${pass_n} passed${NC}, ${RED}${fail_n} failed${NC}, ${YELLOW}${skip_n} skipped${NC}"
else
    echo "${GREEN}${pass_n} passed${NC}, ${RED}${fail_n} failed${NC}"
fi
[ "$fail_n" -eq 0 ]
