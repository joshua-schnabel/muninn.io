#!/usr/bin/env bash
# Run the test suite on Linux, in a container.
#
# Why this exists: development happens on Windows and the artefact is a Linux
# container, and the tests that matter most are the ones that cannot run on the
# development machine — SIGTERM handling, file permissions, reaping a child.
# Those are `#[cfg(unix)]`, so on Windows they compile and are silently absent.
#
# That gap is not theoretical. The SIGTERM test caught a real bug the first time
# it ran here: signal handlers were installed inside the supervise loop, leaving
# a window during startup where SIGTERM had its default disposition and killed
# muninn instead of shutting it down. Nothing on Windows could have found it.
#
#   bash scripts/test-linux.sh                 # whole workspace
#   bash scripts/test-linux.sh -p muninn       # anything after the script name
#                                              # is passed to cargo test
#
# Requires Docker. The Telegraf binary is taken from the pinned image, so the
# tests run against the same version the artefact ships.

set -euo pipefail

TELEGRAF_VERSION="1.39.2"
RUST_IMAGE="rust:1.88-slim"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

[ -n "${MSYSTEM:-}" ] && export MSYS_NO_PATHCONV=1
native() { if [ -n "${MSYSTEM:-}" ]; then (cd "$1" && pwd -W); else (cd "$1" && pwd); fi; }

WORK="${MUNINN_LINUX_WORK:-${TMPDIR:-/tmp}/muninn-linux-test}"
mkdir -p "$WORK/telegraf" "$WORK/target"

# Take the Telegraf binary out of the pinned image rather than downloading it
# again: same bytes the artefact will ship, and no second thing to keep in step.
if [ ! -f "$WORK/telegraf/telegraf" ]; then
    echo "→ extracting telegraf ${TELEGRAF_VERSION} from the pinned image"
    cid=$(docker create "telegraf:${TELEGRAF_VERSION}")
    # docker cp needs a native destination: under Git Bash a /tmp/... path is an
    # MSYS path docker cannot resolve, and it fails with a confusing "directory
    # C:\tmp\... does not exist".
    docker cp "${cid}:/usr/bin/telegraf" "$(native "$WORK/telegraf")/telegraf" >/dev/null
    docker rm -f "$cid" >/dev/null
fi

# Build and test parallelism, deliberately modest.
#
# rustc holds a lot of memory per codegen job, and this runs *alongside* whatever
# the developer's machine is already doing — an editor, a host cargo build,
# Docker Desktop's own VM. Left unbounded it has taken the machine down: the
# container gets OOM-killed with exit 137, and on a bad day so does the host.
# Two jobs is slower and finishes.
JOBS="${MUNINN_LINUX_JOBS:-2}"

# A separate target directory: sharing one with the host build means cargo
# rebuilds the whole workspace on every switch between platforms.
echo "→ running the test suite on Linux (${RUST_IMAGE}, ${JOBS} jobs)"
docker run --rm \
    -v "$(native "$ROOT"):/work" \
    -v "$(native "$WORK/telegraf"):/tg" \
    -v "$(native "$WORK/target"):/target" \
    -w /work \
    -e CARGO_TARGET_DIR=/target \
    -e MUNINN_TELEGRAF_BIN=/tg/telegraf \
    -e CARGO_BUILD_JOBS="$JOBS" \
    "$RUST_IMAGE" \
    bash -c "
        set -e
        chmod +x /tg/telegraf
        # procps for pkill, which one lifecycle test uses to kill the child
        # Telegraf without killing muninn.
        apt-get update -qq >/dev/null 2>&1
        apt-get install -y -qq procps >/dev/null 2>&1
        cargo test ${*:---workspace} --locked -- --test-threads=2
    "
