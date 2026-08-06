#!/usr/bin/env bash
#
# Set the workspace version in Cargo.toml *and* Cargo.lock.
#
# Both, always, because they are one fact stored twice: every CI job runs
# `--locked`, and a Cargo.toml whose version the lock file does not know fails
# with
#
#   error: cannot update the lock file ... because --locked was passed
#
# before a single test runs. That is what the first automated bump produced —
# the version bump and the lock are not two changes, and treating them as two is
# how a release PR arrives red.
#
# Deliberately not `cargo update --workspace`, which would do the same job:
# the two callers are `release.yml`'s prepare-dev and `release-dispatch.yml`,
# both of which hold a write token, and "no job runs cargo with a write token"
# is a security property this repository shipped rather than a style preference
# (docs/ci-cd.md, "What can reach a credential"). Resolution executes no build
# script, so the rule would arguably permit it — but a documented invariant that
# holds except where someone reasoned it away is not an invariant. This needs no
# toolchain, no network and no registry index.
#
# Which packages are the workspace's: those with no `source =` line in the lock.
# Registry packages all carry one, so the distinction is a property of the file
# rather than a hard-coded list of crate names to keep in step.
#
# Usage: set-workspace-version.sh <x.y.z> [<repo root>]

set -euo pipefail

VERSION="${1:-}"
ROOT="${2:-.}"

if [ -z "$VERSION" ]; then
  echo "::error::set-workspace-version.sh: no version given" >&2
  exit 1
fi

# Same SemVer shape scripts/changelog-version.sh enforces. The callers take this
# from a changelog heading or a workflow input, so it is validated here too
# rather than trusted to have been validated upstream.
if ! printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'; then
  echo "::error::'$VERSION' is not a valid SemVer version" >&2
  exit 1
fi

TOML="$ROOT/Cargo.toml"
LOCK="$ROOT/Cargo.lock"

for f in "$TOML" "$LOCK"; do
  if [ ! -f "$f" ]; then
    echo "::error::$f not found" >&2
    exit 1
  fi
done

# Scoped to [workspace.package], where the one version lives. An unscoped
# substitution would also rewrite dependency version pins.
sed -i "/^\[workspace.package\]/,/^\[/s/^version = \".*\"/version = \"${VERSION}\"/" "$TOML"

VERSION="$VERSION" python3 - "$LOCK" <<'PYEOF'
import os, sys

version = os.environ["VERSION"]
path = sys.argv[1]

with open(path, encoding="utf-8", newline="") as f:
    text = f.read()

# Cargo.lock is a sequence of [[package]] blocks separated by blank lines. A
# block is the workspace's own iff it has no `source = ` line; rewriting is
# therefore decided per block, not per line.
blocks = text.split("\n\n")
changed = 0
for i, block in enumerate(blocks):
    lines = block.split("\n")
    if not any(line.startswith("[[package]]") for line in lines):
        continue
    if any(line.startswith("source = ") for line in lines):
        continue
    for j, line in enumerate(lines):
        if line.startswith("version = "):
            new = f'version = "{version}"'
            if lines[j] != new:
                lines[j] = new
                changed += 1
            break
    blocks[i] = "\n".join(lines)

with open(path, "w", encoding="utf-8", newline="") as f:
    f.write("\n\n".join(blocks))

print(f"{path}: {changed} workspace package version(s) set to {version}")
PYEOF

echo "Cargo.toml and Cargo.lock now carry version $VERSION"
