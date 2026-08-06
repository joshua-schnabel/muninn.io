#!/usr/bin/env bash
# System integration — the whole path, with every hop a real process.
#
# The tests below this level each prove one thing well: unit tests prove a
# function, the lifecycle tests prove the binary, the container tests prove the
# image. None of them can prove that a metric collected from the host actually
# arrives in a database and is queryable, because every one of them stops at a
# boundary. This runs the stack:
#
#   muninn → generated TOML → Telegraf → the host → InfluxDB and Prometheus
#
#   bash scripts/integration-test.sh [image]     # default: muninn:dev
#
# Requires Docker with the compose plugin. Build first: docker build -t muninn:dev .
#
# The ten steps of the brief's §18.3 are cells I1–I10; the InfluxDB round trip
# of §18.4 is I11–I13; the injected crash is I14; the failure paths of secrets
# and mounts are I15–I17. Each cell says what it would catch, because a cell
# that cannot fail is a cell that is not testing anything.

set -uo pipefail

IMAGE="${1:-muninn:dev}"
COMPOSE_FILE="docker-compose.integration.yml"
STACK="muninn-integration"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1
[ -n "${MSYSTEM:-}" ] && export MSYS_NO_PATHCONV=1

# Docker needs a native path on the host side of a bind mount. Under Git Bash a
# /tmp/... path is an MSYS path Docker cannot resolve — it silently creates an
# empty DIRECTORY at that name and mounts that instead.
native() { if [ -n "${MSYSTEM:-}" ]; then (cd "$1" && pwd -W); else (cd "$1" && pwd); fi; }

RED=$'\033[0;31m'; GREEN=$'\033[0;32m'; YELLOW=$'\033[1;33m'; NC=$'\033[0m'
passed=0; failed=0

pass() { echo "  ${GREEN}✓ $1${NC}  ${*:2}"; passed=$((passed + 1)); }
fail() { echo "  ${RED}✗ $1${NC}  ${*:2}"; failed=$((failed + 1)); }
info() { echo "${YELLOW}→${NC} $*"; }

WORK="$(mktemp -d)"
MUNINN_WORK="$(native "$WORK")"
export MUNINN_WORK
export MUNINN_IMAGE="$IMAGE"

# A throwaway credential, generated per run, for a database destroyed with the
# stack. Random rather than fixed so that a token accidentally left in a log
# from one run cannot be used against another.
MUNINN_INFLUX_TOKEN="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"
export MUNINN_INFLUX_TOKEN
printf '%s' "$MUNINN_INFLUX_TOKEN" > "$WORK/influxdb-token"

compose() { docker compose -f "$COMPOSE_FILE" "$@"; }

teardown() { compose down --remove-orphans --timeout 25 >/dev/null 2>&1 || true; }
trap 'teardown; rm -rf "$WORK"' EXIT

wait_for() { # seconds  command...
    local deadline=$(( SECONDS + $1 )); shift
    while [ "$SECONDS" -lt "$deadline" ]; do
        "$@" >/dev/null 2>&1 && return 0
        sleep 1
    done
    return 1
}

# A Flux query against the stack's InfluxDB, run by the CLI inside its own
# container so the assertion does not depend on an influx client on the host.
flux() { # query
    compose exec -T influxdb influx query \
        --host http://localhost:8086 \
        --org testorg \
        --token "$MUNINN_INFLUX_TOKEN" \
        --raw "$1" 2>&1
}

promql() { # query
    curl -sf --get --data-urlencode "query=$1" \
        http://localhost:19090/api/v1/query 2>&1
}

echo "integration stack against ${IMAGE}"
echo "work dir ${WORK}"
echo

if ! docker compose version >/dev/null 2>&1; then
    echo "${RED}docker compose is not available — this suite needs the compose plugin${NC}"
    exit 1
fi
if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "${RED}no image ${IMAGE} — build it first: docker build -t ${IMAGE} .${NC}"
    exit 1
fi

# ── The configuration is loaded and generates a valid Telegraf config ────────
# Cells I1–I3 need no stack: they are the steps muninn performs before it starts
# anything, run against the shipped integration configuration itself rather than
# a fixture written for the test. If the file the stack is about to use does not
# validate, everything after this would fail for an uninteresting reason.
info "I1–I3  load, generate, validate — before anything starts"

out="$(docker run --rm \
        --read-only --cap-drop=ALL \
        --tmpfs /run/muninn:mode=0700,uid=10001,gid=10001 \
        -v "$(native "$ROOT")/config/muninn.integration.yaml:/etc/muninn/muninn.yaml:ro" \
        -v "$MUNINN_WORK/influxdb-token:/run/secrets/influxdb_token:ro" \
        "$IMAGE" validate 2>&1)"
if [ $? = 0 ]; then
    pass "I1" "the stack's own configuration loads and validates"
else
    fail "I1" "config/muninn.integration.yaml does not validate: ${out}"
fi

rendered="$(docker run --rm \
        --read-only --cap-drop=ALL \
        --tmpfs /run/muninn:mode=0700,uid=10001,gid=10001 \
        -v "$(native "$ROOT")/config/muninn.integration.yaml:/etc/muninn/muninn.yaml:ro" \
        -v "$MUNINN_WORK/influxdb-token:/run/secrets/influxdb_token:ro" \
        "$IMAGE" render-config 2>/dev/null)"
if grep -q '\[\[outputs.influxdb_v2\]\]' <<<"$rendered" &&
   grep -q '\[\[outputs.prometheus_client\]\]' <<<"$rendered" &&
   grep -q '\[\[inputs.cpu\]\]' <<<"$rendered"; then
    pass "I2" "generates both outputs and the host inputs"
else
    fail "I2" "the rendered configuration is missing an output or an input"
fi
# The token is in the file muninn writes for Telegraf, and in nothing an
# operator can see. Both halves are the point.
if grep -q "$MUNINN_INFLUX_TOKEN" <<<"$rendered"; then
    fail "I2b" "render-config printed the real token"
else
    pass "I2b" "and redacts the token on the way out"
fi

out="$(docker run --rm \
        --read-only --cap-drop=ALL \
        --tmpfs /run/muninn:mode=0700,uid=10001,gid=10001 \
        -v "$(native "$ROOT")/config/muninn.integration.yaml:/etc/muninn/muninn.yaml:ro" \
        -v "$MUNINN_WORK/influxdb-token:/run/secrets/influxdb_token:ro" \
        "$IMAGE" validate --with-telegraf 2>&1)"
if [ $? = 0 ]; then
    pass "I3" "Telegraf itself accepts the generated configuration"
else
    fail "I3" "telegraf config check rejected it: ${out}"
fi

# ── The stack comes up ───────────────────────────────────────────────────────
info "I4–I6  the stack starts, muninn supervises, readiness turns true"

if compose up -d >/dev/null 2>&1; then
    pass "I4" "InfluxDB, muninn and Prometheus started"
else
    fail "I4" "the stack did not start"
    compose logs --tail 40
    echo; echo "${RED}aborting: nothing below can run${NC}"
    exit 1
fi

if wait_for 90 bash -c 'curl -sf http://localhost:18080/health/live >/dev/null'; then
    pass "I5" "muninn answers /health/live"
else
    fail "I5" "muninn never answered the liveness endpoint"
    compose logs --tail 40 muninn
fi

if wait_for 90 bash -c 'curl -sf http://localhost:18080/health/ready >/dev/null'; then
    pass "I6" "and reports ready — Telegraf is running under it"
else
    fail "I6" "never became ready"
    compose logs --tail 40 muninn
fi

# The claim readiness makes: there is a live child. /status names its PID, and
# I14 later kills exactly that process, so a wrong PID here would show up there.
pid="$(curl -sf http://localhost:18080/status | grep -o '"pid":[0-9]*' | head -1 | cut -d: -f2)"
if [ -n "$pid" ]; then
    pass "I6b" "/status reports Telegraf's PID (${pid})"
else
    fail "I6b" "/status did not name a supervised process"
fi

# ── The metrics are real ─────────────────────────────────────────────────────
info "I7–I8  a real host metric, scraped by a real Prometheus"

# Polled, not checked once: cpu_usage_* is a DELTA and needs two collection
# cycles before it exists at all.
if wait_for 60 bash -c 'curl -sf http://localhost:19273/metrics | grep -q "^cpu_usage_idle"'; then
    pass "I7" "Telegraf serves a host CPU metric on :9273"
else
    fail "I7" "no cpu_usage_idle within 60s"
    compose logs --tail 30 muninn
fi

# A value, not just a name. `cpu_usage_idle` between 0 and 100 is the difference
# between "the series exists" and "the series describes a machine".
idle="$(curl -sf http://localhost:19273/metrics | awk '/^cpu_usage_idle\{cpu="cpu-total"/ {print $2; exit}')"
if [ -n "$idle" ] && awk -v v="$idle" 'BEGIN { exit !(v >= 0 && v <= 100) }'; then
    pass "I8" "and its value is a plausible percentage (${idle})"
else
    fail "I8" "cpu_usage_idle was '${idle}', which is not a percentage"
fi

# Prometheus scraping it proves the exposition format is one Prometheus accepts.
# curl cannot show that: a malformed line is still bytes over HTTP.
if wait_for 90 bash -c 'curl -sf --get --data-urlencode "query=cpu_usage_idle" http://localhost:19090/api/v1/query | grep -q "\"result\":\[{"'; then
    pass "I8b" "a real Prometheus scraped the host series"
else
    fail "I8b" "Prometheus has no cpu_usage_idle after 90s"
    promql 'up' | head -c 400; echo
fi

# The other endpoint, and R2's whole point: :9273 alone cannot tell a dead agent
# from a dead host, because both look like a target that stopped answering.
if wait_for 60 bash -c 'curl -sf --get --data-urlencode "query=muninn_telegraf_running" http://localhost:19090/api/v1/query | grep -q "\"value\""'; then
    pass "I8c" "and muninn's own liveness series alongside it"
else
    fail "I8c" "muninn_telegraf_running never reached Prometheus"
fi

# ── No secret reaches the logs ───────────────────────────────────────────────
# A regression guard, and labelled as one rather than oversold: Telegraf does
# not normally quote a configuration value in a diagnostic, so this cell would
# have passed before muninn began scrubbing its child's output. It cannot be
# made to fail on demand — which is exactly why the scrubbing exists, because
# "Telegraf does not normally do that" is an assumption about software this
# project does not control (docs/hardening.md#secrets).
#
# What it does catch is the day that assumption stops holding, or the day the
# redactor stops being wired into the supervisor. The container's whole log —
# muninn's own lines and Telegraf's forwarded ones together — must never
# contain the throwaway token this run generated.
info "I8d  the token never appears in the container logs"

logs="$(compose logs --no-color muninn 2>&1)"
if grep -qF "$MUNINN_INFLUX_TOKEN" <<<"$logs"; then
    fail "I8d" "the InfluxDB token appeared in the container logs"
    grep -nF "$MUNINN_INFLUX_TOKEN" <<<"$logs" | head -3
else
    pass "I8d" "muninn's and Telegraf's output are both free of the token"
fi

# ── The write path ───────────────────────────────────────────────────────────
info "I9–I11  InfluxDB receives the writes and can be queried"

if wait_for 90 bash -c 'docker compose -f docker-compose.integration.yml exec -T influxdb influx query --host http://localhost:8086 --org testorg --token "$MUNINN_INFLUX_TOKEN" --raw "from(bucket:\"testbucket\") |> range(start:-5m) |> filter(fn:(r) => r._measurement == \"cpu\") |> limit(n:1)" 2>/dev/null | grep -q ",cpu,"'; then
    pass "I9" "the cpu measurement arrived in InfluxDB"
else
    fail "I9" "no cpu measurement in InfluxDB after 90s"
    compose logs --tail 30 muninn
fi

# Tagged with the configured hostname rather than the container ID. This is R3:
# without agent.hostname every recreate starts a new series and dashboards
# silently lose their history — a bug that only shows up weeks later.
result="$(flux 'from(bucket:"testbucket") |> range(start:-5m) |> filter(fn:(r) => r.host == "muninn-integration") |> limit(n:1)')"
if grep -q "muninn-integration" <<<"$result"; then
    pass "I10" "tagged with the configured hostname, not the container ID"
else
    fail "I10" "no series tagged host=muninn-integration: ${result}"
fi

# More than one measurement, so the assertion is about the pipeline rather than
# about one plugin that happens to work.
found=0
for measurement in cpu mem system disk; do
    result="$(flux "from(bucket:\"testbucket\") |> range(start:-5m) |> filter(fn:(r) => r._measurement == \"${measurement}\") |> limit(n:1)")"
    grep -q ",${measurement}," <<<"$result" && found=$((found + 1))
done
if [ "$found" -ge 3 ]; then
    pass "I11" "${found} of 4 expected measurements present"
else
    fail "I11" "only ${found} of 4 measurements arrived"
fi

# ── Shutdown ─────────────────────────────────────────────────────────────────
info "I12–I13  SIGTERM shuts the stack down cleanly and leaves nothing behind"

start=$SECONDS
compose stop --timeout 25 muninn >/dev/null 2>&1
elapsed=$(( SECONDS - start ))
code="$(docker inspect --format '{{.State.ExitCode}}' "${STACK}-muninn-1" 2>/dev/null)"

if [ "$code" = "0" ]; then
    pass "I12" "clean shutdown, exit 0, after ${elapsed}s"
else
    fail "I12" "expected exit 0 after SIGTERM, got '${code}'"
    compose logs --tail 30 muninn
fi
if [ "$elapsed" -lt 20 ]; then
    pass "I12b" "inside the grace period — the SIGKILL fallback did not run"
else
    fail "I12b" "took ${elapsed}s, so muninn was killed rather than stopping"
fi

# The generated configuration held a resolved token and lived on a tmpfs. A
# stopped container must not still have it on a layer somebody can export.
if docker cp "${STACK}-muninn-1:/run/muninn/telegraf.conf" "$WORK/leaked.conf" >/dev/null 2>&1 &&
   [ -s "$WORK/leaked.conf" ]; then
    fail "I13" "the generated configuration survived the container"
else
    pass "I13" "the generated configuration is gone with the tmpfs"
fi

# ── The crash path ───────────────────────────────────────────────────────────
# The failure this project most wants to avoid: a container that looks healthy
# while Telegraf is dead inside it.
info "I14  an injected Telegraf crash is detected, not absorbed"

compose start muninn >/dev/null 2>&1
if ! wait_for 90 bash -c 'curl -sf http://localhost:18080/health/ready >/dev/null'; then
    fail "I14" "muninn did not come back up, so the crash path cannot be tested"
else
    pid="$(curl -sf http://localhost:18080/status | grep -o '"pid":[0-9]*' | head -1 | cut -d: -f2)"
    if [ -z "$pid" ]; then
        fail "I14" "/status did not report a PID to kill"
    else
        # As muninn's own uid: with --cap-drop=ALL there is no CAP_KILL, so even
        # uid 0 cannot signal a process owned by another user.
        compose exec -T muninn sh -c "kill -9 $pid" >/dev/null 2>&1
        if wait_for 60 bash -c \
            '[ "$(docker inspect --format "{{.State.Running}}" muninn-integration-muninn-1)" = false ]'; then
            code="$(docker inspect --format '{{.State.ExitCode}}' "${STACK}-muninn-1")"
            if [ "$code" = "22" ]; then
                pass "I14" "exit 22 (TELEGRAF_EXITED) — the crash reached the orchestrator"
            else
                fail "I14" "expected exit 22 after killing Telegraf, got ${code}"
            fi
        else
            fail "I14" "the container kept running with a dead Telegraf"
        fi
    fi
fi

teardown

# ── Failure paths ────────────────────────────────────────────────────────────
# Happy paths above, the ways a deployment goes wrong here. These run against
# the image directly: they fail before anything would connect to the stack, and
# keeping them independent means a broken stack cannot hide a broken refusal.
info "I15–I17  secrets and mounts, when the deployment is wrong"

# A configuration whose only variable is the token path.
influx_config() { # token-path  outfile
    cat > "$WORK/$2" <<YAML
version: 1
agent:
  interval: 2s
  flush_interval: 2s
  hostname: "integration-failure-path"
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
outputs:
  influxdb:
    enabled: true
    url: "http://influxdb:8086"
    organization: testorg
    bucket: testbucket
    token_file: "$1"
    timeout: 2s
YAML
}

# The token that is not mounted where the configuration says. Docker creates a
# DIRECTORY for a bind mount whose source is missing, so this is what a typo in
# a compose file actually produces — and a directory read as a token would be an
# I/O error much later, from Telegraf, about something else.
influx_config "/run/secrets/influxdb_token" "muninn-no-secret.yaml"
out="$(docker run --rm \
        --read-only --cap-drop=ALL \
        --tmpfs /run/muninn:mode=0700,uid=10001,gid=10001 \
        -v "$MUNINN_WORK/muninn-no-secret.yaml:/etc/muninn/muninn.yaml:ro" \
        -v /:/hostfs:ro \
        "$IMAGE" run 2>&1)"
code=$?
if [ "$code" = "11" ]; then
    pass "I15" "an unmounted secret exits 11 (SECRET), before anything starts"
else
    fail "I15" "expected exit 11 for a missing token file, got ${code}: ${out}"
fi
if grep -q "influxdb_token" <<<"$out" && ! grep -q "$MUNINN_INFLUX_TOKEN" <<<"$out"; then
    pass "I15b" "and names the path without ever printing a value"
else
    fail "I15b" "the message should name the path and nothing else: ${out}"
fi

# An empty file is the shape a secret takes when whatever should have written it
# failed. Treating it as an empty token would authenticate as nobody and fail
# later, from InfluxDB, as a 401 nobody traces back to here.
printf '' > "$WORK/empty-token"
influx_config "/run/secrets/influxdb_token" "muninn-empty-secret.yaml"
out="$(docker run --rm \
        --read-only --cap-drop=ALL \
        --tmpfs /run/muninn:mode=0700,uid=10001,gid=10001 \
        -v "$MUNINN_WORK/muninn-empty-secret.yaml:/etc/muninn/muninn.yaml:ro" \
        -v "$MUNINN_WORK/empty-token:/run/secrets/influxdb_token:ro" \
        -v /:/hostfs:ro \
        "$IMAGE" run 2>&1)"
code=$?
if [ "$code" = "11" ]; then
    pass "I16" "an empty secret file exits 11 rather than authenticating as nobody"
else
    fail "I16" "expected exit 11 for an empty token file, got ${code}: ${out}"
fi

# The host mount, forgotten. The image creates /hostfs itself, so the directory
# exists and only the module's own path check catches it. Without that, Telegraf
# reports the CONTAINER's CPU and disks as the host's — plausible numbers about
# the wrong machine, with no error anywhere.
printf 'integration-throwaway' > "$WORK/token"
influx_config "/run/secrets/influxdb_token" "muninn-no-mount.yaml"
out="$(docker run --rm \
        --read-only --cap-drop=ALL \
        --tmpfs /run/muninn:mode=0700,uid=10001,gid=10001 \
        -v "$MUNINN_WORK/muninn-no-mount.yaml:/etc/muninn/muninn.yaml:ro" \
        -v "$MUNINN_WORK/token:/run/secrets/influxdb_token:ro" \
        "$IMAGE" run 2>&1)"
code=$?
if [ "$code" = "12" ]; then
    pass "I17" "a forgotten host mount exits 12 (RUNTIME) rather than measuring the container"
else
    fail "I17" "expected exit 12 without the host mount, got ${code}: ${out}"
fi
if grep -qi "hostfs" <<<"$out" && grep -qi "cpu" <<<"$out"; then
    pass "I17b" "and names both the path and the module that needs it"
else
    fail "I17b" "the finding should name the path and the module: ${out}"
fi

echo
echo "─────────────────────────────────────────────"
echo "${GREEN}${passed} passed${NC}, ${RED}${failed} failed${NC}"
exit "$failed"
