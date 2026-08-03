#!/bin/bash
# Build a fixture from THIS host (a real machine, not a container image):
# real dpkg status, real sources, and freshly fetched indices.
#
#   build-host-native.sh <outdir>
#
# Run it ON the host being measured — for S11, inside WSL Debian:
#   wsl -d Debian -- bash scripts/fixtures/build-host-native.sh /mnt/c/…/wsl-real
#
# Everything it writes goes to <outdir> and a scratch directory under /tmp.
# It needs no root, and it does NOT touch the system's own apt state: the
# indices are fetched into the scratch directory via Dir::State::lists, so
# /var/lib/apt/lists is left exactly as it was. That matters — a measurement
# that modified the machine it was measuring would invalidate its own criterion.
#
# Why fetch fresh indices at all: a host whose lists are months old reports zero
# pending updates, correctly, and a cell that compares zero against zero does not
# exercise the counting path. Fresh indices give a real host a real non-zero
# answer to be checked against.
set -e
DEST="$1"
S=/tmp/muninn-fixture
rm -rf "$S" "$DEST"
mkdir -p "$S/lists/partial" "$S/cache/archives/partial"
mkdir -p "$DEST/rootfs/var/lib/apt" "$DEST/rootfs/var/lib/dpkg" "$DEST/rootfs/etc" "$DEST/rootfs/usr/lib"

apt-get update -o Dir::State::lists="$S/lists" -o Dir::Cache="$S/cache" -o Debug::NoLocking=1 >/dev/null 2>&1

# Ground truth: this host's own apt, this host's own installed set, fresh indices.
apt-get -s dist-upgrade -o Dir::State::lists="$S/lists" -o Dir::Cache="$S/cache" \
        -o Debug::NoLocking=1 > "$DEST/ground-truth.txt" 2>/dev/null

cp -a /var/lib/dpkg/status "$DEST/rootfs/var/lib/dpkg/"
cp -aL /etc/apt            "$DEST/rootfs/etc/"
cp -a "$S/lists"           "$DEST/rootfs/var/lib/apt/lists"
cp -a /usr/lib/os-release  "$DEST/rootfs/usr/lib/" 2>/dev/null || true
cp -a /etc/os-release      "$DEST/rootfs/etc/"     2>/dev/null || true

total=$(grep -c '^Inst ' "$DEST/ground-truth.txt" || echo 0)
sec=$(grep '^Inst ' "$DEST/ground-truth.txt" | grep -c -- '-[Ss]ecurity' || echo 0)
{
  echo "image=WSL Debian (real host)"
  echo "state=fresh-lists"
  echo "apt=$(apt-get --version | head -1)"
  echo "os=$(. /etc/os-release; echo "$ID $VERSION_ID")"
  echo "total=$total"
  echo "security=$sec"
} > "$DEST/meta.txt"
cat "$DEST/meta.txt"
echo "dpkg status: $(stat -c %s "$DEST/rootfs/var/lib/dpkg/status") bytes"
echo "installed packages: $(grep -c '^Package: ' "$DEST/rootfs/var/lib/dpkg/status")"
