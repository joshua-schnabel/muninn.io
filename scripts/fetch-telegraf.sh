#!/usr/bin/env bash
# Fetch the pinned Telegraf binary, verified against the checksum the Dockerfile
# already carries.
#
#   bash scripts/fetch-telegraf.sh <destination-directory> [arch]
#
# Prints the path to the extracted binary on stdout. `arch` defaults to the
# host's, as Docker names it (amd64 / arm64).
#
# # Why this exists
#
# CI needs the Telegraf binary to run muninn's unix-only tests — the ones that
# are silently absent on the maintainer's Windows machine. It used to get it
# with `docker create telegraf:1.39.2 && docker cp`, which pulls a *mutable
# tag*: whoever controls that tag controls the binary the whole test suite runs
# against, and neither Dependabot nor any gate here would notice it move
# (dependabot-core#5819 — the same limitation security.yml records for its
# pinned Semgrep image).
#
# The fix is not a second pin to keep current by hand. ADR-0011 already pins
# this exact tarball by SHA-256, in the Dockerfile, per architecture — so this
# reads the version and the checksums from *there* and verifies against them.
# Bumping Telegraf stays what the ADR says it is: three lines in the Dockerfile,
# visible in the diff. There is one source of truth and this is downstream of
# it.
#
# The Dockerfile is parsed rather than duplicated for the same reason ci.yml
# already greps TELEGRAF_VERSION out of it: a second copy of the number is a
# second thing to keep in step, and the one that drifts is never the one you
# are looking at.

set -euo pipefail

DEST="${1:-}"
if [ -z "$DEST" ]; then
    echo "usage: $0 <destination-directory> [arch]" >&2
    exit 2
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DOCKERFILE="${TELEGRAF_DOCKERFILE:-$ROOT/Dockerfile}"

if [ ! -f "$DOCKERFILE" ]; then
    echo "::error::$DOCKERFILE not found — this script reads the pin from it" >&2
    exit 1
fi

# `ARG NAME=value` → value. First match wins; the Dockerfile declares each once.
arg_from_dockerfile() {
    sed -n "s/^[[:space:]]*ARG[[:space:]]\+$1=\([^[:space:]]*\).*/\1/p" "$DOCKERFILE" | head -1
}

VERSION="$(arg_from_dockerfile TELEGRAF_VERSION)"
if [ -z "$VERSION" ]; then
    echo "::error::could not read TELEGRAF_VERSION from $DOCKERFILE" >&2
    exit 1
fi

ARCH="${2:-}"
if [ -z "$ARCH" ]; then
    case "$(uname -m)" in
        x86_64|amd64)  ARCH=amd64 ;;
        aarch64|arm64) ARCH=arm64 ;;
        *) echo "::error::unsupported architecture $(uname -m)" >&2; exit 1 ;;
    esac
fi

case "$ARCH" in
    amd64) SHA="$(arg_from_dockerfile TELEGRAF_SHA256_AMD64)" ;;
    arm64) SHA="$(arg_from_dockerfile TELEGRAF_SHA256_ARM64)" ;;
    # Exactly the Dockerfile's own rule: a new architecture fails loudly rather
    # than proceeding with an unverified binary.
    *) echo "::error::no pinned Telegraf checksum for arch=$ARCH" >&2; exit 1 ;;
esac

if [ -z "$SHA" ]; then
    echo "::error::could not read the $ARCH checksum from $DOCKERFILE" >&2
    exit 1
fi

mkdir -p "$DEST"
DEST="$(cd "$DEST" && pwd)"

FILE="telegraf-${VERSION}_linux_${ARCH}.tar.gz"
URL="https://dl.influxdata.com/telegraf/releases/${FILE}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "fetching Telegraf ${VERSION} (${ARCH})" >&2
curl -fsSL --retry 3 --retry-delay 2 -o "${WORK}/${FILE}" "$URL"

# The whole point of the script. `sha256sum -c` exits non-zero on a mismatch and
# `set -e` turns that into a failed job — an unverified binary must never reach
# the tests, because a test suite that passes against the wrong binary is worse
# than one that does not run.
echo "${SHA}  ${WORK}/${FILE}" | sha256sum -c - >&2

tar -xzf "${WORK}/${FILE}" -C "$WORK"
# The tarball lays out ./telegraf-<version>/usr/bin/telegraf.
install -m 0755 "${WORK}/telegraf-${VERSION}/usr/bin/telegraf" "${DEST}/telegraf"

"${DEST}/telegraf" version >&2
printf '%s\n' "${DEST}/telegraf"
