#!/usr/bin/env bash
# Build a simulated host: a real Debian or Ubuntu rootfs whose apt and dpkg
# state is exported to a directory, together with the ground truth that host
# reports about itself.
#
# The export mirrors the layout of a real `/:/hostfs:ro` mount, including
# /usr/lib/os-release — because /etc/os-release is a symlink to it, and a
# fixture that flattened that would hide a real deployment failure mode.
#
#   build-host.sh <image> <outdir> [state]
#
# state:
#   stale   (default) the image as published — usually has pending updates
#   fresh   fully upgraded first — the "nothing pending" case
#   oldlists  package lists backdated 30 days — the "nobody ran update" case
#
# Writes to <outdir>:
#   rootfs/           the mountable host filesystem subset
#   ground-truth.txt  what the host itself answers
#   meta.txt          image, state, apt version, os id

set -euo pipefail

IMAGE="${1:?usage: build-host.sh <image> <outdir> [state]}"
OUTDIR="${2:?usage: build-host.sh <image> <outdir> [state]}"
STATE="${3:-stale}"

mkdir -p "$OUTDIR"
rm -rf "${OUTDIR:?}/rootfs" "$OUTDIR/ground-truth.txt" "$OUTDIR/meta.txt"

# Docker needs a native path on Windows; Git Bash would otherwise hand it an
# MSYS path it cannot resolve.
host_path() {
    if [ -n "${MSYSTEM:-}" ]; then
        (cd "$1" && pwd -W)
    else
        (cd "$1" && pwd)
    fi
}
abs_out="$(host_path "$OUTDIR")"

prepare=""
case "$STATE" in
    fresh)
        prepare='apt-get upgrade -y -qq >/dev/null 2>&1 || true'
        ;;
    oldlists)
        # Backdate the indices without changing their content: the answer stays
        # correct, only the freshness signal changes.
        prepare='find /var/lib/apt/lists -maxdepth 1 -type f -exec touch -d "30 days ago" {} +'
        ;;
    stale)
        prepare=':'
        ;;
    *)
        echo "unknown state: $STATE" >&2
        exit 2
        ;;
esac

docker run --rm -v "${abs_out}:/export" "$IMAGE" bash -c "
set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null 2>&1 || true
$prepare

mkdir -p /export/rootfs/var/lib /export/rootfs/etc /export/rootfs/usr/lib
cp -a /var/lib/dpkg      /export/rootfs/var/lib/
cp -a /var/lib/apt       /export/rootfs/var/lib/
cp -a /etc/apt           /export/rootfs/etc/
# The symlink and its target, exactly as a real root mount would present them.
cp -a /etc/os-release     /export/rootfs/etc/     2>/dev/null || true
cp -a /usr/lib/os-release /export/rootfs/usr/lib/ 2>/dev/null || true

# Ground truth: what this host says about itself, from inside itself.
apt-get -s dist-upgrade -o APT::Get::Show-Versions=false > /export/ground-truth.txt 2>/dev/null || true

{
  echo \"image=$IMAGE\"
  echo \"state=$STATE\"
  echo \"apt=\$(apt-get --version | head -1)\"
  echo \"os=\$(. /etc/os-release; echo \\\"\$ID \$VERSION_ID\\\")\"
  echo \"total=\$(grep -c '^Inst ' /export/ground-truth.txt || echo 0)\"
  echo \"security=\$(grep '^Inst ' /export/ground-truth.txt | grep -c -- '-[Ss]ecurity' || echo 0)\"
} > /export/meta.txt
" >/dev/null 2>&1

cat "$OUTDIR/meta.txt"
