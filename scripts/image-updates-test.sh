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
# # The authenticated registry (I11-I13)
#
# Cells I1-I10 all use a public registry, where an anonymous distribution query
# succeeds — so none of them ever exercised ADR-0013's original claim, that
# muninn needs no credential handling because the daemon "already knows any
# registry credentials the host is configured with". These three did, and it was
# false: `docker login` writes to the *client's* config and the CLI forwards the
# credential in an X-Registry-Auth header, which muninn was not sending.
#
# They now cover the fix. A local `registry:2` behind htpasswd, and muninn given
# the credential the way this project accepts credentials — a password *file*
# named by its own configuration: I11 asserts the image is judged, I12 that a
# rejected credential is a reason with no verdict, I13 the same for a repository
# the registry does not have. Finding F-14, and R9.
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

# The local authenticated registry cells (I11-I13) use 127.0.0.1 deliberately:
# Docker treats 127.0.0.0/8 as an insecure registry by default, so these need no
# daemon configuration and therefore no privileged setup step on a runner.
REGISTRY_IMAGE="registry:2"
HTPASSWD_IMAGE="httpd:2"
REGISTRY_ADDR="127.0.0.1:${IMAGE_UPDATES_REGISTRY_PORT:-5000}"
REGISTRY_USER="muninn"
REGISTRY_PASS="s3cret-for-a-test-only"
REGISTRY_REPO="${REGISTRY_ADDR}/muninn-test/app"

cleanup() {
    docker rm -f "${PREFIX}-agent" >/dev/null 2>&1
    docker ps -aq --filter "name=^${PREFIX}-c" | while read -r id; do
        docker rm -f "$id" >/dev/null 2>&1
    done
    docker rmi -f "${PREFIX}/local:v1" >/dev/null 2>&1
    # The registry cells log the *host's* docker client in. Leaving that behind
    # would be this suite editing the credential store of the machine it ran on,
    # so the logout is part of teardown rather than of the last cell that
    # happens to need it.
    docker logout "$REGISTRY_ADDR" >/dev/null 2>&1
    docker rm -f "${PREFIX}-registry" >/dev/null 2>&1
    docker rmi -f "${REGISTRY_REPO}:v1" >/dev/null 2>&1
}
trap cleanup EXIT

# ── Helpers ──────────────────────────────────────────────────────────────────

# The group that owns the socket, as a *container* sees it.
#
# Not `stat` on the host, which is wrong in both directions: on Docker Desktop
# the host has no such file at all (the socket lives in the VM), and on Linux
# the gid a container has to name is the one inside its own user namespace.
# Asking a throwaway container is the only answer that is right on both.
#
# Empty means "could not tell" — the run then goes ahead without the flag and
# fails with a permission error that says so, which is better than silently
# skipping the cell.
SOCKET_GID=""
detect_socket_group() {
    docker run --rm -v "${SOCKET}:/var/run/docker.sock:ro" "$OLD_TAG" \
        stat -c %g /var/run/docker.sock 2>/dev/null | tr -d '\r'
}

# The `--group-add` a non-root container needs to read a 0660 socket.
#
# Group 0 is included rather than skipped: on Docker Desktop the socket is
# root:root, so 0 is the answer, and skipping it is what made every cell here
# fail with EACCES the first time this suite was run. This is the same grant
# docs/modules.md#docker tells an operator to make — the container stays
# non-root, read-only and capability-free; it gains exactly the group needed to
# open one socket.
group_flag() {
    [ -n "$SOCKET_GID" ] && printf '%s' "--group-add=${SOCKET_GID}"
}

# A configuration to mount at /etc/muninn/muninn.yaml for the next `check`,
# together with the directory holding any secret files it names. Empty for
# every cell that needs no credentials, which is all of I1-I10: `image-check`
# reads its configuration only for `modules.image_updates.registry_auth`, and
# says so on stderr when there is none.
CHECK_CONFIG=""
CHECK_SECRETS=""

# Where `check` puts muninn's stderr instead of discarding it.
#
# The line protocol on stdout is what the cells assert against, and stderr used
# to go to /dev/null so it could not corrupt that. But muninn says on stderr why
# it dropped a credential — an unreadable file, a rejected registry — and
# throwing that away is what made the first I11 failure a guess: the reason
# token says the daemon's query failed, not whether muninn ever sent a header.
# A failing cell quotes the last lines of this file.
CHECK_STDERR="$WORK/check.stderr"

check_stderr_tail() { # -> the last few lines, or nothing
    [ -s "$CHECK_STDERR" ] && tr '\n' '|' < "$CHECK_STDERR" | tail -c 400
}

# `muninn image-check` in the shipped image, hardened, against the real daemon.
check() {
    local flag; flag=$(group_flag)
    local mounts=()
    if [ -n "$CHECK_CONFIG" ]; then
        mounts+=(-v "$(native "$(dirname "$CHECK_CONFIG")")/$(basename "$CHECK_CONFIG"):/etc/muninn/muninn.yaml:ro")
    fi
    if [ -n "$CHECK_SECRETS" ]; then
        mounts+=(-v "$(native "$CHECK_SECRETS"):/run/secrets:ro")
    fi
    docker run --rm \
        --read-only --cap-drop=ALL --security-opt no-new-privileges:true \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        ${flag:+"$flag"} \
        "${mounts[@]}" \
        -v "${SOCKET}:/var/run/docker.sock:ro" \
        "$IMAGE" image-check --endpoint unix:///var/run/docker.sock "$@" 2>"$CHECK_STDERR"
}

# A muninn configuration whose image_updates module carries one registry
# credential, plus the password file it names.
#
# The password is a file and the file is mounted, because that is the only
# shape this project accepts a credential in — and writing the cell any other
# way would be testing a path operators cannot use.
write_auth_config() { # <password>
    local dir="$WORK/auth-config"
    rm -rf "$dir" && mkdir -p "$dir/secrets"
    printf '%s' "$1" > "$dir/secrets/registry-password"

    # Deliberately NOT 0600, and the reason is the one an operator meets too.
    #
    # A bind mount carries the host's uid, and the image runs as uid 10001
    # (Dockerfile `USER muninn`). A 0600 file written by the host user is
    # therefore unreadable inside the container, muninn drops the credential
    # with a warning, and the daemon is asked without a header — which reports
    # `distribution_query_failed`, i.e. exactly the symptom this cell was
    # written to detect. The first CI run of I11 failed that way, against a
    # module that was working: `chmod 0600` here measured the fixture, not
    # muninn. `scripts/integration-test.sh` never hit it only because it has
    # always written its InfluxDB token at the default umask.
    #
    # The mode is what muninn warns about; ownership is what decides whether it
    # can read at all. Documented at the key, in docs/configuration.md.
    chmod 0644 "$dir/secrets/registry-password" 2>/dev/null || true
    cat > "$dir/muninn.yaml" <<YAML
version: 1
modules:
  image_updates:
    enabled: true
    registry_auth:
      - registry: "${REGISTRY_ADDR}"
        username: "${REGISTRY_USER}"
        password_file: /run/secrets/registry-password
outputs:
  prometheus:
    enabled: true
YAML
    CHECK_CONFIG="$dir/muninn.yaml"
    CHECK_SECRETS="$dir/secrets"
}

clear_auth_config() { CHECK_CONFIG=""; CHECK_SECRETS=""; }

# The line for one container, out of the whole report. The check line comes
# first, so this is it rather than the verdict line.
line_for() { # output  container_name
    echo "$1" | grep "container_name=$2," | head -1
}

# Whether the verdict line for one container carries a given value.
#
# The tag set between the name and the field matters: the line is
# `...,container_name=X,image=Y update_available=0i`, so a pattern that expects
# the field straight after the name never matches — which is how the first run
# of this suite reported I3 and I4 red against a module that was answering
# correctly.
verdict_is() { # output  container_name  0|1
    echo "$1" | grep -q "container_name=$2,[^ ]* update_available=$3i"
}

has_verdict() { # output  container_name
    echo "$1" | grep -q "container_name=$2,[^ ]* update_available="
}

# The reason tag out of an influx line, for a message.
#
# `field` reads fields, which are `key=value` with a type suffix; `reason` is a
# tag, so it needs its own reader. It had one inline in three cells and all
# three were broken in the same way — a `\1` backreference that reached the
# file as a literal control byte, so every cell that printed a reason printed
# nothing where the reason should be. That is why it lives here now.
reason_of() { # line
    printf '%s' "$1" | sed -n 's/.*[ ,]reason=\([A-Za-z0-9_]*\).*//p' | head -1
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

# ── The authenticated registry the I11-I13 cells measure against ─────────────
#
# Started fresh each time it is called, with storage in the container's own
# writable layer and no volume. That is what makes "the repository is not there"
# testable at all: restarting it is how a registry that held an image comes to
# answer 404 for it while the host still has the image and its RepoDigest.
start_registry() { # -> 0 up, 1 could not
    local auth; auth="$WORK/auth"
    mkdir -p "$auth"

    # bcrypt, generated by the httpd image rather than by a host htpasswd that
    # may not be installed. -B because registry:2 rejects the older formats.
    if [ ! -s "$auth/htpasswd" ]; then
        docker run --rm --entrypoint htpasswd "$HTPASSWD_IMAGE" \
            -Bbn "$REGISTRY_USER" "$REGISTRY_PASS" > "$auth/htpasswd" 2>/dev/null \
            || return 1
        [ -s "$auth/htpasswd" ] || return 1
    fi

    docker rm -f "${PREFIX}-registry" >/dev/null 2>&1
    docker run -d --name "${PREFIX}-registry" \
        -p "${REGISTRY_ADDR}:5000" \
        -v "$(native "$auth")/htpasswd:/auth/htpasswd:ro" \
        -e "REGISTRY_AUTH=htpasswd" \
        -e "REGISTRY_AUTH_HTPASSWD_REALM=muninn-test" \
        -e "REGISTRY_AUTH_HTPASSWD_PATH=/auth/htpasswd" \
        "$REGISTRY_IMAGE" >/dev/null 2>&1 || return 1

    # Up means "answers /v2/ with 401", not "the container is running": an
    # unauthenticated 401 is the registry saying both that it is listening and
    # that the auth stanza took effect. Polled, not slept at.
    local deadline=$(( SECONDS + 60 ))
    while [ "$SECONDS" -lt "$deadline" ]; do
        [ "$(curl -s -o /dev/null -w '%{http_code}' "http://${REGISTRY_ADDR}/v2/" 2>/dev/null)" = "401" ] \
            && return 0
        sleep 1
    done
    return 1
}

# The three cells share a setup, and each of them is meaningless if it did not
# complete, so it reports its own reason for skipping rather than failing.
registry_fixture() { # -> 0 ready, 1 unusable (message on stdout)
    if ! docker pull -q "$REGISTRY_IMAGE" >/dev/null 2>&1 \
       || ! docker pull -q "$HTPASSWD_IMAGE" >/dev/null 2>&1; then
        echo "cannot pull ${REGISTRY_IMAGE} / ${HTPASSWD_IMAGE}"
        return 1
    fi
    if ! start_registry; then
        echo "the local registry never answered 401 on ${REGISTRY_ADDR}/v2/"
        return 1
    fi
    if ! docker login "$REGISTRY_ADDR" -u "$REGISTRY_USER" -p "$REGISTRY_PASS" >/dev/null 2>&1; then
        echo "could not log the host's docker client into ${REGISTRY_ADDR}"
        return 1
    fi
    docker tag "$OLD_TAG" "${REGISTRY_REPO}:v1" >/dev/null 2>&1
    if ! docker push -q "${REGISTRY_REPO}:v1" >/dev/null 2>&1; then
        echo "could not push to ${REGISTRY_REPO}"
        return 1
    fi
    return 0
}

# Everything below needs the daemon, and most of it needs Docker Hub. Checked
# once, loudly, rather than as five identical failures.
#
# Reasons go to stderr rather than stdout, and this is deliberately not called
# in a command substitution: it sets SOCKET_GID, and a subshell would discard
# it — which is exactly how the first run of this suite got EACCES on every
# cell that touches the socket.
preflight() {
    docker info >/dev/null 2>&1 \
        || { echo "no reachable Docker daemon" >&2; return 1; }
    docker pull -q "$OLD_TAG" >/dev/null 2>&1 \
        || { echo "cannot pull ${OLD_TAG}" >&2; return 1; }
    SOCKET_GID=$(detect_socket_group)
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
    elif verdict_is "$out" "${PREFIX}-c-current" 0; then
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
    if verdict_is "$out" "${PREFIX}-c-stale" 1; then
        pass I4 "a container running a tag that has since moved reports an available update"
    else
        fail I4 "expected update_available=1 for a deliberately stale container, got: $out"
    fi
    docker rm -f "${PREFIX}-c-stale" >/dev/null 2>&1
}

I5() { # an image that exists only locally is never reported as "up to date"
    #
    # The assertion is the invariant, not the reason token, and that is
    # deliberate: which reason this produces depends on the daemon's image
    # store. With the classic store a locally built image has no `RepoDigests`
    # and this reports `no_repo_digest`; with the containerd store (Docker 29
    # here) the daemon records a locally computed digest, so the check goes on
    # to ask the registry about a repository that was never pushed and reports
    # `distribution_query_failed` instead.
    #
    # Both are honest — `check_success=0`, no verdict — and muninn has no way
    # to tell "this repository does not exist anywhere" from "the registry did
    # not answer" without parsing daemon error prose. So the cell defends the
    # property that actually matters and R9 carries the ambiguity.
    local d="$WORK/local-image"
    mkdir -p "$d"
    printf 'FROM %s\nRUN true\n' "$OLD_TAG" > "$d/Dockerfile"
    # `native` for the same reason every -v in this file uses it: MSYS path
    # conversion is off, so a /c/... context path is not one the daemon can
    # resolve.
    docker build -q -t "${PREFIX}/local:v1" "$(native "$d")" >/dev/null 2>&1 \
        || { fail I5 "could not build a local image"; return; }
    start_container "${PREFIX}-c-local" "${PREFIX}/local:v1" \
        || { fail I5 "could not start"; return; }

    local out line
    out=$(check --include "${PREFIX}-c-local")
    line=$(line_for "$out" "${PREFIX}-c-local")

    if [ "$(field "$line" check_success)" = 0 ] \
       && echo "$line" | grep -qE 'reason=(no_repo_digest|distribution_query_failed)' \
       && ! has_verdict "$out" "${PREFIX}-c-local"; then
        pass I5 "an image that only exists locally reports a reason and NO verdict \
($(echo "$line" | sed -n 's/.*reason=\([a-z_]*\).*/\1/p'))"
    else
        fail I5 "expected check_success=0 with a reason and no verdict, got: $out"
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

    local flag; flag=$(group_flag)

    docker rm -f "${PREFIX}-agent" >/dev/null 2>&1
    docker run -d --name "${PREFIX}-agent" \
        --read-only --cap-drop=ALL --security-opt no-new-privileges:true \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        ${flag:+"$flag"} \
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

I11() { # THE cell for F-14: does a private registry work at all?
    #
    # ADR-0013's original premise was that muninn needed no credential handling,
    # because asking the daemon meant the daemon "already knows any registry
    # credentials the host is configured with". This cell is what executed that
    # sentence for the first time, and it was false: `docker login` writes to
    # the *client's* config and the CLI forwards the credential in an
    # X-Registry-Auth header, which muninn was not sending. The measured answer
    # was distribution_query_failed against a registry the host could push to.
    #
    # So the cell now asserts the fixed behaviour: with the credential in
    # muninn's own configuration, as a password *file*, the image is judged.
    local why
    if ! why=$(registry_fixture); then
        skip I11 "$why"
        return
    fi
    start_container "${PREFIX}-c-private" "${REGISTRY_REPO}:v1"         || { fail I11 "could not start a container from the private registry"; return; }

    write_auth_config "$REGISTRY_PASS"
    local out line
    out=$(check --include "${PREFIX}-c-private")
    line=$(line_for "$out" "${PREFIX}-c-private")
    clear_auth_config

    if [ "$(field "$line" check_success)" = 1 ] && has_verdict "$out" "${PREFIX}-c-private"; then
        pass I11 "an image from an authenticated registry is judged, using the configured credential"
    else
        fail I11 "expected a verdict for an authenticated registry (reason=$(reason_of "$line")): $line [muninn said: $(check_stderr_tail)]"
    fi
    docker rm -f "${PREFIX}-c-private" >/dev/null 2>&1
}

I12() { # a credential the registry rejects must never produce a healthy value
    #
    # The password in muninn's configuration is wrong, so the registry answers
    # 401. This is the project's sharpest rule applied to the new code path: a
    # rejected credential is a reason with no verdict, never "up to date".
    local why
    if ! why=$(registry_fixture); then
        skip I12 "$why"
        return
    fi
    start_container "${PREFIX}-c-401" "${REGISTRY_REPO}:v1"         || { fail I12 "could not start"; return; }

    write_auth_config "definitely-not-the-password"
    local out line
    out=$(check --include "${PREFIX}-c-401")
    line=$(line_for "$out" "${PREFIX}-c-401")
    clear_auth_config

    if [ "$(field "$line" check_success)" = 0 ]        && echo "$line" | grep -q 'reason='        && ! has_verdict "$out" "${PREFIX}-c-401"; then
        pass I12 "a rejected credential reports a reason and NO verdict ($(reason_of "$line"))"
    else
        fail I12 "expected check_success=0 with a reason and no verdict, got: $out"
    fi
    docker rm -f "${PREFIX}-c-401" >/dev/null 2>&1
}

I13() { # authenticated, but the repository is not there
    #
    # Correct credentials this time, against a registry restarted with empty
    # storage: the host keeps the image and its RepoDigest, so the module has
    # every reason to ask, and the answer is "no such repository" rather than a
    # refusal to connect or to authorise. Still a reason with no verdict.
    local why
    if ! why=$(registry_fixture); then
        skip I13 "$why"
        return
    fi
    start_container "${PREFIX}-c-404" "${REGISTRY_REPO}:v1"         || { fail I13 "could not start"; return; }
    if ! start_registry; then
        skip I13 "the registry did not come back up empty"
        docker rm -f "${PREFIX}-c-404" >/dev/null 2>&1
        return
    fi

    write_auth_config "$REGISTRY_PASS"
    local out line
    out=$(check --include "${PREFIX}-c-404")
    line=$(line_for "$out" "${PREFIX}-c-404")
    clear_auth_config

    if [ "$(field "$line" check_success)" = 0 ]        && echo "$line" | grep -q 'reason='        && ! has_verdict "$out" "${PREFIX}-c-404"; then
        pass I13 "a repository the registry does not have reports a reason and NO verdict ($(reason_of "$line"))"
    else
        fail I13 "expected check_success=0 with a reason and no verdict, got: $out"
    fi
    docker rm -f "${PREFIX}-c-404" >/dev/null 2>&1
}

# ── Run ──────────────────────────────────────────────────────────────────────

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "${RED}image ${IMAGE} not found — build it first: docker build -t ${IMAGE} .${NC}" >&2
    exit 2
fi

echo "image_updates system tests against ${IMAGE}"
echo "work directory ${WORK}"
echo

if ! preflight; then
    echo "${YELLOW}every cell needs a working daemon and Docker Hub${NC}" >&2
    exit 2
fi
echo "socket group ${SOCKET_GID:-<undetermined>}"
echo

CELLS=("$@")
[ ${#CELLS[@]} -eq 0 ] && CELLS=(I1 I2 I3 I4 I5 I6 I7 I8 I9 I10 I11 I12 I13)

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
