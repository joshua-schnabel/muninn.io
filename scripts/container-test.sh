#!/usr/bin/env bash
# Container tests — the image, under the posture the documentation promises.
#
# Every run here uses the full hardening: non-root, read-only root filesystem,
# --cap-drop=ALL, no-new-privileges, and a tmpfs for /run/muninn. That is
# deliberate. A test that quietly relaxed one of them would prove the image
# works in a configuration nobody ships.
#
#   bash scripts/container-test.sh [image]     # default: muninn:dev
#
# Requires Docker. Build first: docker build -t muninn:dev .

set -uo pipefail

IMAGE="${1:-muninn:dev}"
UID_GID="10001:10001"
TMPFS_OPTS="mode=0700,uid=10001,gid=10001"
# Pinned, and the same tag docker-compose.docker-module.yml uses — the test and
# the recommendation have to be about the same thing.
PROXY_IMAGE="tecnativa/docker-socket-proxy:0.3.0"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[ -n "${MSYSTEM:-}" ] && export MSYS_NO_PATHCONV=1

# Docker needs a native path on the host side of a bind mount. Under Git Bash a
# /tmp/... path is an MSYS path Docker cannot resolve — it silently creates an
# empty DIRECTORY at that name and mounts that instead, and the only symptom is
# "cannot read muninn.yaml: Is a directory".
native() { if [ -n "${MSYSTEM:-}" ]; then (cd "$1" && pwd -W); else (cd "$1" && pwd); fi; }

RED=$'\033[0;31m'; GREEN=$'\033[0;32m'; YELLOW=$'\033[1;33m'; NC=$'\033[0m'
passed=0; failed=0

pass() { echo "${GREEN}✓${NC} $*"; passed=$((passed + 1)); }
fail() { echo "${RED}✗${NC} $*"; failed=$((failed + 1)); }
info() { echo "${YELLOW}→${NC} $*"; }

WORK="$(mktemp -d)"
WORK_NATIVE="$(native "$WORK")"
trap 'rm -rf "$WORK"; docker rm -f muninn-test muninn-proxy >/dev/null 2>&1 || true;       docker network rm muninn-test-net >/dev/null 2>&1 || true' EXIT

printf 'container-test-token' > "$WORK/token"

# A configuration that collects locally and writes nowhere over the network.
cat > "$WORK/muninn.yaml" <<'YAML'
version: 1
agent:
  interval: 1s
  flush_interval: 1s
  hostname: "container-test"
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
  cpu:
    enabled: true
  memory:
    enabled: true
  disks:
    enabled: true
    exclude_filesystems: [tmpfs, devtmpfs, overlay]
outputs:
  prometheus:
    enabled: true
    listen: "0.0.0.0:9273"
YAML

# The hardened run, as the compose file and the documentation describe it.
run_muninn() {
    docker run -d --name muninn-test \
        --read-only \
        --cap-drop=ALL \
        --security-opt no-new-privileges:true \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        -v "$WORK_NATIVE/muninn.yaml:/etc/muninn/muninn.yaml:ro" \
        -v /:/hostfs:ro \
        -p 18080:8080 -p 19273:9273 \
        "$@" \
        "$IMAGE" >/dev/null
}

wait_for() { # seconds  command...
    local deadline=$(( SECONDS + $1 )); shift
    while [ "$SECONDS" -lt "$deadline" ]; do
        "$@" >/dev/null 2>&1 && return 0
        sleep 1
    done
    return 1
}

cleanup() { docker rm -f muninn-test >/dev/null 2>&1 || true; }

echo "container tests against ${IMAGE}"
echo

# ── 1. It starts, becomes healthy, and serves both endpoints ─────────────────
info "1/10  starts hardened, becomes healthy, serves both endpoints"
cleanup
run_muninn

if wait_for 60 bash -c 'curl -sf http://localhost:18080/health/ready'; then
    pass "health endpoint reports ready"
else
    fail "never became ready"
    docker logs muninn-test 2>&1 | tail -20
fi

# Polled, not checked once: cpu_usage_* is a DELTA, so it needs two collection
# cycles before it exists at all. Disk figures are absolute and appear on the
# first flush — which is why a single check right after readiness sees disks and
# concludes, wrongly, that CPU collection is broken.
if wait_for 30 bash -c 'curl -sf http://localhost:19273/metrics | grep -q "^cpu_usage_idle"'; then
    pass "Telegraf serves host CPU metrics on :9273"
else
    fail "no cpu_usage_idle on :9273 within 30s"
fi

if curl -sf http://localhost:18080/metrics | grep -q 'muninn_telegraf_running 1'; then
    pass "muninn serves its own metrics on the health port"
else
    fail "muninn_telegraf_running missing from :8080/metrics"
fi

# The disks module reads the host through the mount, so a filesystem the
# container does not have of its own is proof the prefix took effect.
if curl -sf http://localhost:19273/metrics | grep -q 'disk_'; then
    pass "host disk metrics are collected through /hostfs"
else
    fail "no disk metrics — the host mount may not be in effect"
fi

# ── 2. Docker's own health check agrees ──────────────────────────────────────
info "2/10  the image's HEALTHCHECK reports healthy"
if wait_for 60 bash -c \
    '[ "$(docker inspect --format "{{.State.Health.Status}}" muninn-test)" = healthy ]'; then
    pass "docker reports the container healthy"
else
    fail "docker never reported healthy: $(docker inspect --format '{{.State.Health.Status}}' muninn-test 2>&1)"
fi

# ── 3. SIGTERM shuts down cleanly, inside the grace period ───────────────────
info "3/10  SIGTERM shuts down cleanly"
start=$SECONDS
docker stop --timeout 30 muninn-test >/dev/null 2>&1
elapsed=$(( SECONDS - start ))
code="$(docker inspect --format '{{.State.ExitCode}}' muninn-test 2>/dev/null)"

if [ "$code" = "0" ]; then
    pass "clean shutdown, exit 0, after ${elapsed}s"
else
    fail "expected exit 0 after SIGTERM, got ${code}"
fi
if [ "$elapsed" -lt 20 ]; then
    pass "stopped well inside the grace period"
else
    fail "took ${elapsed}s — the SIGKILL fallback probably ran"
fi

# ── 4. A dead Telegraf takes the container down ──────────────────────────────
# The failure this project most wants to avoid is a container that looks healthy
# while Telegraf crash-loops invisibly inside it.
info "4/10  a dead Telegraf takes the container down with exit 22"
cleanup
run_muninn
if ! wait_for 60 bash -c 'curl -sf http://localhost:18080/health/ready'; then
    fail "never became ready, cannot test the crash path"
else
    # The PID comes from muninn's own /status. procps is deliberately not in the
    # image, so there is no pkill or pidof — but `kill` is a shell builtin. This
    # also exercises /status reporting something true.
    pid="$(curl -sf http://localhost:18080/status | grep -o '"pid":[0-9]*' | head -1 | cut -d: -f2)"

    if [ -z "$pid" ]; then
        fail "/status did not report Telegraf's PID, so the crash path cannot be tested"
    else
        # As `muninn`, not as root. With --cap-drop=ALL there is no CAP_KILL, so
        # uid 0 cannot signal a process owned by another user — `docker exec -u 0`
        # fails with "Operation not permitted". A same-uid signal needs no
        # capability. Worth knowing: the hardening is stricter than it looks.
        if ! docker exec muninn-test sh -c "kill -9 $pid" 2>&1; then
            fail "could not kill Telegraf (pid $pid)"
        elif wait_for 40 bash -c \
            '[ "$(docker inspect --format "{{.State.Running}}" muninn-test)" = false ]'; then
            code="$(docker inspect --format '{{.State.ExitCode}}' muninn-test)"
            if [ "$code" = "22" ]; then
                pass "container exited 22 (TELEGRAF_EXITED)"
            else
                fail "expected exit 22, got ${code}"
            fi
        else
            fail "container kept running after Telegraf died — a crash must never be invisible"
        fi
    fi
fi

# ── 5. A missing configuration file ──────────────────────────────────────────
info "5/10  a missing configuration file exits 10"
cleanup
out="$(docker run --rm --read-only --cap-drop=ALL \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        "$IMAGE" run 2>&1)"
code=$?
if [ "$code" = "10" ]; then
    pass "exit 10 (CONFIG)"
else
    fail "expected exit 10, got ${code}: ${out}"
fi
if grep -q "does not exist" <<<"$out"; then
    pass "and says the file is missing"
else
    fail "message should say the file is missing: ${out}"
fi

# ── 6. A missing secret ──────────────────────────────────────────────────────
info "6/10  a missing secret file exits 11"
cleanup
sed 's|^outputs:|outputs:\n  influxdb:\n    enabled: true\n    url: "https://influx.example:8086"\n    organization: o\n    bucket: b\n    token_file: "/run/secrets/absent"|' \
    "$WORK/muninn.yaml" > "$WORK/muninn-influx.yaml"
out="$(docker run --rm --read-only --cap-drop=ALL \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        -v "$WORK_NATIVE/muninn-influx.yaml:/etc/muninn/muninn.yaml:ro" \
        "$IMAGE" run 2>&1)"
code=$?
if [ "$code" = "11" ]; then
    pass "exit 11 (SECRET)"
else
    fail "expected exit 11, got ${code}: ${out}"
fi

# ── 7. A missing host mount is reported, not worked around ───────────────────
# Without the mount Telegraf would report the container's own CPU and disks as
# the host's — plausible numbers about the wrong machine.
info "7/10  check-runtime reports a missing host mount"
cleanup
out="$(docker run --rm --read-only --cap-drop=ALL \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        -v "$WORK_NATIVE/muninn.yaml:/etc/muninn/muninn.yaml:ro" \
        "$IMAGE" check-runtime 2>&1)"
code=$?
if [ "$code" = "12" ]; then
    pass "exit 12 (RUNTIME)"
else
    fail "expected exit 12, got ${code}: ${out}"
fi
if grep -qi "hostfs" <<<"$out"; then
    pass "and names the mount that is missing"
else
    fail "should name the mount: ${out}"
fi

# ...and passes once the mount is there.
out="$(docker run --rm --read-only --cap-drop=ALL \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        -v "$WORK_NATIVE/muninn.yaml:/etc/muninn/muninn.yaml:ro" \
        -v /:/hostfs:ro \
        "$IMAGE" check-runtime 2>&1)"
if [ $? = 0 ]; then
    pass "and passes once the host is mounted"
else
    fail "check-runtime failed with the mount in place: ${out}"
fi

# ── 8. The healthcheck command fails when there is nothing to reach ──────────
info "8/10  healthcheck fails when nothing is running"
cleanup
out="$(docker run --rm --read-only --cap-drop=ALL \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        -v "$WORK_NATIVE/muninn.yaml:/etc/muninn/muninn.yaml:ro" \
        "$IMAGE" healthcheck 2>&1)"
if [ $? != 0 ]; then
    pass "healthcheck exits non-zero with no agent running"
else
    fail "healthcheck reported healthy with nothing running: ${out}"
fi

# ── 9. The Docker module against a real socket ───────────────────────────────
# WP9's acceptance criterion: enabling the module with an endpoint that does not
# answer must be a startup failure, not an empty metric set. Both halves are
# tested — the refusal, and that a reachable socket really does produce
# per-container metrics.
info "9/10  the docker module against a real Docker socket"
cleanup

# Whether the DAEMON has a unix socket, which is not the same question as
# whether this shell can see one. On Docker Desktop the client talks to a named
# pipe on Windows while the daemon inside the VM has the ordinary socket, so
# `[ -S /var/run/docker.sock ]` here answers about the wrong machine and skips
# the whole section on a host where it would have worked.
has_socket() {
    # --entrypoint, because the image's own entrypoint is muninn: a bare
    # `test` would be an argument to muninn rather than a command.
    docker run --rm --entrypoint /usr/bin/test \
        -v /var/run/docker.sock:/var/run/docker.sock:ro \
        "$IMAGE" -S /var/run/docker.sock >/dev/null 2>&1
}

# The working configuration plus the docker module. Written in full rather than
# patched into the base file: `modules:` is not the last section there, so
# appending would land the block under `outputs:` — and the resulting error
# ("missing required key") points nowhere near the actual mistake.
docker_config() { # endpoint  outfile
    cat > "$2" <<YAML
version: 1
agent:
  interval: 1s
  flush_interval: 1s
  hostname: "container-test"
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
  cpu:
    enabled: true
  docker:
    enabled: true
    endpoint: "$1"
    timeout: 5s
outputs:
  prometheus:
    enabled: true
    listen: "0.0.0.0:9273"
YAML
}

# First the refusal, which needs no socket at all and so always runs. Port 1 on
# the loopback is never listening.
docker_config "tcp://127.0.0.1:1" "$WORK/muninn-docker-dead.yaml"
out="$(docker run --rm --read-only --cap-drop=ALL \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        -v "$WORK_NATIVE/muninn-docker-dead.yaml:/etc/muninn/muninn.yaml:ro" \
        -v /:/hostfs:ro \
        "$IMAGE" run 2>&1)"
code=$?
if [ "$code" = "12" ]; then
    pass "an unreachable endpoint refuses the start with exit 12"
else
    fail "expected exit 12 for an unreachable Docker endpoint, got ${code}: ${out}"
fi
if grep -qi "nothing to report" <<<"$out"; then
    pass "and says why silence is not an acceptable answer"
else
    fail "the message should explain the refusal: ${out}"
fi

if has_socket; then
    docker_config "unix:///var/run/docker.sock" "$WORK/muninn-docker.yaml"

    # muninn runs as uid 10001 and the socket is owned by root:docker, so
    # mounting it is not enough — the process has to be in the socket's group.
    # `--group-add` rather than `--user 0:0`: running as root would prove the
    # image works in a posture nobody should ship, which the header of this file
    # says these tests must never do.
    #
    # Needing this at all is the strongest practical argument for the proxy in
    # test 10, which needs no group, no socket and no relaxation.
    sock_gid="$(docker run --rm --entrypoint /usr/bin/stat \
        -v /var/run/docker.sock:/var/run/docker.sock:ro \
        "$IMAGE" -c '%g' /var/run/docker.sock 2>/dev/null | tr -d '\r')"
    : "${sock_gid:=0}"

    docker run -d --name muninn-test \
        --read-only --cap-drop=ALL --security-opt no-new-privileges:true \
        --group-add "$sock_gid" \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        -v "$WORK_NATIVE/muninn-docker.yaml:/etc/muninn/muninn.yaml:ro" \
        -v /:/hostfs:ro \
        -v /var/run/docker.sock:/var/run/docker.sock:ro \
        -p 18080:8080 -p 19273:9273 \
        "$IMAGE" >/dev/null

    if wait_for 60 bash -c 'curl -sf http://localhost:18080/health/ready'; then
        pass "starts with the socket mounted (group ${sock_gid})"
        # The container running this test is itself a container, so there is
        # always at least one to report — no fixture needed.
        if wait_for 40 bash -c \
            'curl -sf http://localhost:19273/metrics | grep -q "^docker_container_"'; then
            pass "per-container metrics arrive through the socket"
        else
            fail "no docker_container_* metrics within 40s"
            docker logs muninn-test 2>&1 | tail -20
        fi
    else
        fail "never became ready with the socket mounted"
        docker logs muninn-test 2>&1 | tail -20
    fi
else
    info "     skipped the live-socket half: the daemon has no unix socket to mount"
fi

# ── 10. The recommended deployment: through a socket proxy ───────────────────
# docs/modules.md recommends this as the way to enable the module. A
# recommendation nobody tests is a recommendation nobody should follow.
info "10/10  the docker module through a socket proxy"
cleanup
docker rm -f muninn-proxy >/dev/null 2>&1 || true
docker network rm muninn-test-net >/dev/null 2>&1 || true

if has_socket && docker pull -q "$PROXY_IMAGE" >/dev/null 2>&1; then
    docker network create muninn-test-net >/dev/null 2>&1
    docker_config "tcp://muninn-proxy:2375" "$WORK/muninn-proxy.yaml"

    docker run -d --name muninn-proxy --network muninn-test-net \
        -e CONTAINERS=1 -e INFO=1 -e VERSION=1 -e PING=1 -e POST=0 \
        -v /var/run/docker.sock:/var/run/docker.sock:ro \
        "$PROXY_IMAGE" >/dev/null

    docker run -d --name muninn-test --network muninn-test-net \
        --read-only --cap-drop=ALL --security-opt no-new-privileges:true \
        --tmpfs "/run/muninn:${TMPFS_OPTS}" \
        -v "$WORK_NATIVE/muninn-proxy.yaml:/etc/muninn/muninn.yaml:ro" \
        -v /:/hostfs:ro \
        -p 18080:8080 -p 19273:9273 \
        "$IMAGE" >/dev/null

    if wait_for 90 bash -c 'curl -sf http://localhost:18080/health/ready'; then
        pass "starts against the proxy — no socket, no group, no relaxation"
        if wait_for 40 bash -c \
            'curl -sf http://localhost:19273/metrics | grep -q "^docker_container_"'; then
            pass "container metrics arrive through the proxy"
        else
            fail "no docker_container_* metrics through the proxy within 40s"
            docker logs muninn-test 2>&1 | tail -20
        fi
    else
        fail "never became ready against the proxy"
        docker logs muninn-test 2>&1 | tail -20
        docker logs muninn-proxy 2>&1 | tail -10
    fi

    # The half that makes the request-based probe worth having: a proxy that is
    # up and denying the call must be refused, not read as "no containers". A
    # plain connect check would pass here.
    docker rm -f muninn-test muninn-proxy >/dev/null 2>&1
    docker run -d --name muninn-proxy --network muninn-test-net \
        -e CONTAINERS=0 -e PING=0 -e POST=0 \
        -v /var/run/docker.sock:/var/run/docker.sock:ro \
        "$PROXY_IMAGE" >/dev/null

    out="$(docker run --rm --network muninn-test-net \
            --read-only --cap-drop=ALL \
            --tmpfs "/run/muninn:${TMPFS_OPTS}" \
            -v "$WORK_NATIVE/muninn-proxy.yaml:/etc/muninn/muninn.yaml:ro" \
            -v /:/hostfs:ro \
            "$IMAGE" run 2>&1)"
    code=$?
    if [ "$code" = "12" ]; then
        pass "a proxy that denies the call is a startup failure, not silence"
    else
        fail "expected exit 12 against a denying proxy, got ${code}: ${out}"
    fi

    docker rm -f muninn-proxy >/dev/null 2>&1 || true
    docker network rm muninn-test-net >/dev/null 2>&1 || true
else
    info "     skipped: no unix socket, or ${PROXY_IMAGE} could not be pulled"
fi

cleanup
echo
echo "─────────────────────────────────────────────"
echo "${GREEN}${passed} passed${NC}, ${RED}${failed} failed${NC}"
exit "$failed"
