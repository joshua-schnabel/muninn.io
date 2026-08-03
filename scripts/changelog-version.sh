#!/usr/bin/env bash
#
# Read the topmost released version from CHANGELOG.md and print it.
#
# The version is validated against SemVer 2.0.0 *before* it is printed, so the
# only thing a caller can ever receive is `x.y.z[-prerelease][+build]`. That
# matters because the value is a workflow input: CHANGELOG.md is editable by
# anyone who can land a commit, and an unvalidated heading such as
# `## [0.1.2$(...)]` would otherwise reach a shell as command substitution.
# Validating here — rather than in one gate job — keeps every consumer safe,
# including pushes to `dev`, where the release version gate is a no-op.
#
# Usage: scripts/changelog-version.sh [--allow-unreleased] [path/to/CHANGELOG.md]
# Prints the version on stdout; exits non-zero with a message on stderr if the
# file has no versioned entry or the entry is not valid SemVer.
#
# --allow-unreleased falls back to the workspace version in Cargo.toml when the
# changelog holds only `## [Unreleased]`. That is muninn's state until the first
# release is cut, and pushes to `dev` have to keep producing a `:x.y.z-dev`
# image throughout it. The fallback is deliberately NOT the default: the release
# path must never invent a version the changelog does not document, and the
# version gate calls this without the flag for exactly that reason.

set -euo pipefail

ALLOW_UNRELEASED=0
if [ "${1:-}" = "--allow-unreleased" ]; then
  ALLOW_UNRELEASED=1
  shift
fi

CHANGELOG="${1:-CHANGELOG.md}"
CARGO_TOML="$(dirname "$CHANGELOG")/Cargo.toml"

if [ ! -f "$CHANGELOG" ]; then
  echo "::error::$CHANGELOG not found" >&2
  exit 1
fi

# `## [1.2.3] - 2026-01-01` → `1.2.3`. The leading digit class skips
# `## [Unreleased]`, which must never be treated as a release. The `|| true`
# keeps a no-match `grep` (exit 1) from tripping `set -e`/`pipefail` before the
# explicit error below can explain what is wrong.
version="$(grep -m1 '^## \[[0-9]' "$CHANGELOG" | sed 's/## \[\(.*\)\].*/\1/' || true)"

if [ -z "$version" ] && [ "$ALLOW_UNRELEASED" = "1" ]; then
  # The first `version = "..."` under [workspace.package]. Every crate inherits
  # it with `version.workspace = true`, so there is one number to find.
  version="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' \
    "$CARGO_TOML" 2>/dev/null | head -1 || true)"
  if [ -n "$version" ]; then
    echo "::notice::$CHANGELOG has no released entry yet; using the workspace version $version" >&2
  fi
fi

if [ -z "$version" ]; then
  echo "::error::no versioned entry (## [x.y.z]) found in $CHANGELOG" >&2
  exit 1
fi

# SemVer 2.0.0: x.y.z with optional -prerelease and +build metadata.
if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'; then
  echo "::error::'$version' in $CHANGELOG is not a valid SemVer version" >&2
  exit 1
fi

printf '%s\n' "$version"
