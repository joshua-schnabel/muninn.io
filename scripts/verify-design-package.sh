#!/usr/bin/env bash
# Verification suite for the WP0 design package.
#
# Checks the things a documentation-and-schema deliverable can actually be wrong
# about: does the workspace build, do the example configs parse, does the target
# Telegraf format really work, does the documentation describe options that
# exist, and is the pinned Telegraf the binary we think it is.
#
# CI runs this as the `reference` job with VERIFY_SKIP_CARGO=1, because `check`,
# `test` and `supply-chain` are the same gates on their own runners there. Run
# it whole locally:
#   bash scripts/verify-design-package.sh
#
# Requires: cargo, python3, docker, curl, sha256sum.

set -uo pipefail

TELEGRAF_VERSION="1.39.2"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

# Git Bash on Windows rewrites container-side paths like /ref into Windows paths
# before docker ever sees them, so `--config /ref/x.conf` arrives as
# `C:/x.conf`. Disable that, and hand docker a native path for the host side.
# Both are no-ops elsewhere.
DOCKER_ROOT="$ROOT"
if [ -n "${MSYSTEM:-}" ]; then
  export MSYS_NO_PATHCONV=1
  DOCKER_ROOT="$(pwd -W)"
fi

RED=$'\033[0;31m'; GREEN=$'\033[0;32m'; YELLOW=$'\033[1;33m'; NC=$'\033[0m'
failures=0

pass() { echo "${GREEN}✓${NC} $*"; }
fail() { echo "${RED}✗${NC} $*"; failures=$((failures + 1)); }
info() { echo "${YELLOW}→${NC} $*"; }

# ── 1. The workspace builds and the gates are clean ──────────────────────────
# VERIFY_SKIP_CARGO=1 in CI, where `check`, `test` and `supply-chain` have
# already run these on their own runners. Skipping is not the same as not
# running them: the point of this step is that a local run gates everything at
# once. Anywhere else, leaving it out would be a way to pass by omission.
if [ "${VERIFY_SKIP_CARGO:-0}" = "1" ]; then
  info "1/8  cargo gates — skipped (VERIFY_SKIP_CARGO=1; CI runs them as separate jobs)"
else
  info "1/8  cargo gates"
  cargo fmt --all -- --check          >/dev/null 2>&1 && pass "cargo fmt"    || fail "cargo fmt"
  cargo metadata --locked --format-version 1 >/dev/null 2>&1 \
    && pass "cargo metadata --locked" || fail "cargo metadata --locked (Cargo.lock out of date?)"
  cargo clippy --workspace --all-targets --all-features -- -D warnings >/dev/null 2>&1 \
    && pass "cargo clippy -D warnings" || fail "cargo clippy"
  cargo test --workspace --locked     >/dev/null 2>&1 && pass "cargo test"   || fail "cargo test"
fi

# ── 2. The example configurations are valid YAML ─────────────────────────────
info "2/8  example configurations parse"
python3 - <<'PY' && pass "config/*.yaml parse" || fail "config/*.yaml"
import yaml, pathlib, sys
ok = True
for p in sorted(pathlib.Path('config').glob('*.yaml')):
    try:
        d = yaml.safe_load(p.read_text(encoding='utf-8'))
        assert d.get('version') == 1, f'{p.name}: missing or unexpected version'
        outs = [k for k, v in (d.get('outputs') or {}).items() if v.get('enabled')]
        assert outs, f'{p.name}: no output enabled — muninn would refuse to start'
    except Exception as e:
        ok = False
        print(f'  {p.name}: {e}')
sys.exit(0 if ok else 1)
PY

# ── 3. The reference config is real, valid Telegraf ──────────────────────────
# The primary acceptance criterion: the format the renderer targets is proven
# before the renderer exists.
info "3/8  reference config accepted by Telegraf ${TELEGRAF_VERSION}"
if docker run --rm -v "$DOCKER_ROOT/docs/reference:/ref:ro" "telegraf:${TELEGRAF_VERSION}" \
     telegraf config check --strict-env-handling --config /ref/telegraf.reference.conf >/dev/null 2>&1
then
  pass "telegraf config check accepts telegraf.reference.conf"
else
  fail "telegraf config check REJECTED telegraf.reference.conf"
fi

# ── 4. The ordering fixtures still demonstrate what ADR-0007 claims ──────────
# Both must pass validation — that is the point, the mistake is invisible to it.
# The difference only shows up in the metrics actually emitted.
info "4/8  sub-table ordering fixtures (ADR-0007)"
for f in ordering-correct ordering-broken; do
  docker run --rm -v "$DOCKER_ROOT/docs/reference:/ref:ro" "telegraf:${TELEGRAF_VERSION}" \
    telegraf config check --strict-env-handling --config "/ref/${f}.conf" >/dev/null 2>&1 \
    && pass "${f}.conf passes config check (expected — validation cannot see this)" \
    || fail "${f}.conf no longer passes config check; ADR-0007's premise has changed"
done

count_disk() {
  docker run --rm -v "$DOCKER_ROOT/docs/reference:/ref:ro" "telegraf:${TELEGRAF_VERSION}" \
    telegraf --config "/ref/$1.conf" --test 2>/dev/null | grep -c '^> disk,'
}
correct_n=$(count_disk ordering-correct)
broken_n=$(count_disk ordering-broken)
if [ "$broken_n" -gt "$correct_n" ]; then
  pass "ordering matters: correct=${correct_n} disk metrics, broken=${broken_n}"
else
  fail "ordering no longer changes behaviour (correct=${correct_n}, broken=${broken_n}) — recheck ADR-0007"
fi

# ── 5. Every option the renderer can emit exists upstream ─────────────────
# Catches the Telegraf plugin surface drifting away from the version actually
# shipped (risk R5).
#
# The options are read out of the **renderer's own source**, not out of
# docs/reference/telegraf.reference.conf. That reference is rendered from the
# shipped example, so it only ever contained the options that example happens to
# switch on — and the example enables neither the docker module nor either
# `inputs.exec` block, and leaves every TLS and basic-auth key null. The whole
# `inputs.docker` block, both exec blocks, and all six credential and TLS
# options were therefore never checked against upstream at all, which is exactly
# the drift this gate exists to catch walking past it (N-03).
#
# Reading the source covers what the renderer *can* emit rather than what one
# configuration does emit, and cannot fall behind the example again. The
# alternative — a second reference config that enables everything — would have
# needed a committed artefact verified by a real `telegraf config check`, and an
# unverified reference is worse than none: it becomes a test that agrees with
# whatever the code happens to do.
info "5/8  plugin options exist in Telegraf ${TELEGRAF_VERSION}"
TELEGRAF_VERSION="$TELEGRAF_VERSION" python3 - <<'PY' && pass "every plugin option exists upstream" || fail "unknown plugin option(s)"
import re, os, pathlib, sys, urllib.request

ver = 'v' + os.environ['TELEGRAF_VERSION']
base = f'https://raw.githubusercontent.com/influxdata/telegraf/{ver}/plugins'

# `PluginInstance::input("cpu", ...)` opens a plugin; every `.scalar(...)`,
# `.scalar_opt(...)` and `.list(...)` until the next one belongs to it.
INSTANCE = re.compile(r'PluginInstance::(input|output)\(\s*"([A-Za-z0-9_]+)"')
OPTION = re.compile(r'\.(?:scalar|scalar_opt|list)\(\s*"([A-Za-z0-9_]+)"')

pairs = set()
for src in sorted(pathlib.Path('crates/muninn-modules/src').rglob('*.rs')):
    text = src.read_text(encoding='utf-8')
    cut = text.find('#[cfg(test)]')          # a test's fixture is not a rendered option
    if cut >= 0:
        text = text[:cut]
    plugin = None
    for line in text.splitlines():
        m = INSTANCE.search(line)
        if m:
            plugin = f'{m.group(1)}s/{m.group(2)}'
        if plugin:
            for opt in OPTION.findall(line):
                pairs.add((plugin, opt))

if not pairs:
    print('  found no plugin options at all — the source layout changed and this',
          'gate is now checking nothing')
    sys.exit(1)

# Options every plugin of that kind accepts, listed in Telegraf's
# docs/CONFIGURATION.md rather than in any one plugin's sample.conf — so a
# sample-only check reports them missing. Verified against the pinned release's
# "Input Plugin Common Parameters" section.
#
# Deliberately only the ones muninn actually renders. A full transcription would
# be a list nobody re-checks, and the point of this gate is that an option
# muninn emits has been seen somewhere upstream.
COMMON = {
    'inputs': {'interval', 'alias', 'precision', 'name_override', 'tags'},
    'outputs': {'alias'},
}

cache, missing = {}, []
for plug, opt in sorted(pairs):
    if opt in COMMON.get(plug.split('/')[0], ()):
        continue
    if plug not in cache:
        try:
            cache[plug] = urllib.request.urlopen(f'{base}/{plug}/sample.conf', timeout=30).read().decode()
        except Exception as e:
            print(f'  could not fetch {plug}: {e}')
            cache[plug] = ''
    if not re.search(rf'^\s*#?\s*{re.escape(opt)}\s*=', cache[plug], re.M):
        missing.append(f'{plug}.{opt}')

print(f'  checked {len(pairs)} options across {len(cache)} plugins')
for m in missing:
    print('  not found:', m)
sys.exit(1 if missing else 0)
PY

# ── 6. The pinned Telegraf checksums match upstream and ADR-0011 ─────────────
info "6/8  Telegraf tarball checksums"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
(
  cd "$tmp"
  for arch in amd64 arm64; do
    url="https://dl.influxdata.com/telegraf/releases/telegraf-${TELEGRAF_VERSION}_linux_${arch}.tar.gz"
    curl -sfL -o "telegraf-${TELEGRAF_VERSION}_linux_${arch}.tar.gz" "$url"
    curl -sfL "${url}.DIGESTS" >> DIGESTS.all
  done
  sha256sum -c DIGESTS.all >/dev/null 2>&1
) && pass "tarballs match upstream DIGESTS" || fail "tarball checksum mismatch"

grep -oE '^[0-9a-f]{64}' "$tmp/DIGESTS.all" 2>/dev/null | sort > "$tmp/upstream.txt"
grep -oE '[0-9a-f]{64}' docs/adr/0011-telegraf-pinning.md | sort > "$tmp/adr.txt"
if diff -q "$tmp/adr.txt" "$tmp/upstream.txt" >/dev/null 2>&1; then
  pass "ADR-0011 records the upstream checksums exactly"
else
  fail "ADR-0011 checksums differ from upstream"
fi

# ── 7. Every relative documentation link resolves, fragment included ─────────
info "7/8  documentation cross-references"
python3 - <<'PY' && pass "all relative markdown links resolve" || fail "broken markdown link(s)"
# Paths *and* heading fragments.
#
# Only the path was checked, and `#anchor` was dropped before the check — so a
# link to a heading that had been renamed, or never existed, reported success.
# Two such links were in the tree when this was written: ci-cd.md pointed at a
# hardening heading that had been reworded, and security-audit.md used the
# `{#id}` syntax, which GitHub-flavoured Markdown does not read as a custom
# heading ID at all. Neither had ever resolved (F-16).
#
# The slug rules are GitHub's, because GitHub is where these are read:
# lowercase, drop everything that is not a word character, whitespace or a
# hyphen, then spaces to hyphens. Repeats get `-1`, `-2`. An em dash therefore
# leaves the two spaces around it and produces a double hyphen, which is why
# real anchors in this repository look like `#f-01--secret-values-...`.
import collections, re, pathlib, sys, urllib.parse

def slugs(text):
    seen, out = collections.Counter(), set()
    for line in text.splitlines():
        m = re.match(r'^(#{1,6})\s+(.*?)\s*$', line)
        if not m:
            continue
        t = m.group(2)
        t = re.sub(r'\[([^\]]*)\]\([^)]*\)', r'\1', t)  # [text](url) -> text
        t = t.replace('`', '').lower()
        t = re.sub(r'[^\w\s-]', '', t).replace(' ', '-')
        n = seen[t]
        seen[t] += 1
        out.add(t if n == 0 else f'{t}-{n}')
    return out

anchors, bad, checked, frags = {}, [], 0, 0
for md in sorted(pathlib.Path('.').rglob('*.md')):
    if 'target' in md.parts:
        continue
    text = md.read_text(encoding='utf-8')
    for m in re.finditer(r'\[[^\]]*\]\(([^)]+)\)', text):
        link = m.group(1).strip()
        if link.startswith(('http://', 'https://', 'mailto:')):
            continue
        path, _, frag = link.partition('#')

        target = md if not path else (md.parent / urllib.parse.unquote(path))
        if path:
            checked += 1
            if not target.resolve().exists():
                bad.append(f'{md.as_posix()} -> {link}  (no such file)')
                continue
        # A same-document link has no path and is still worth checking.
        if not frag or target.suffix != '.md':
            continue
        key = target.resolve()
        if key not in anchors:
            anchors[key] = slugs(key.read_text(encoding='utf-8'))
        frags += 1
        if urllib.parse.unquote(frag) not in anchors[key]:
            bad.append(f'{md.as_posix()} -> {link}  (no such heading)')

print(f'  checked {checked} relative links and {frags} heading fragments')
for b in bad:
    print('  broken:', b)
sys.exit(1 if bad else 0)
PY

# ── 8. The release named in prose is the release the changelog cut ──────────
#
# Three pages tell a reader which version is current, and each does it by naming
# the number. AGENTS.md §7 forbids exactly that for good reason — a number
# copied into a sentence is wrong the morning after the next release, and
# nothing fails when it goes stale. F-15 cleaned this up once already, and it
# came back the day 1.0.0 shipped: all three still said `0.1.0`, and the README
# additionally still called it "a `0.x` release" while versioning.md had been
# rewritten to promise 1.x semantics.
#
# Naming the version is a deliberate choice, so the drift is made mechanical
# instead of remembered. The authority is CHANGELOG.md, read through
# changelog-version.sh — which is the same extraction the version gate and both
# release workflows use, and validates SemVer before printing.
info "8/8  the release named in prose"
expected="$(bash scripts/changelog-version.sh 2>/dev/null || true)"
if [ -z "$expected" ]; then
  fail "could not read the current version from CHANGELOG.md"
else
  drifted=""
  # One line per page, each the sentence that states what is current. Anchored
  # on its own wording rather than on "any version-shaped string in the file",
  # so a historical mention elsewhere on the page is not a false positive.
  check_prose() { # <file> <grep -E pattern for the status sentence>
    line="$(grep -nE "$2" "$1" | head -1)" || true
    if [ -z "$line" ]; then
      drifted="${drifted}\n  $1: the status sentence was not found — reword the pattern in this check"
      return
    fi
    found="$(printf '%s' "$line" | grep -oE '`[0-9]+\.[0-9]+\.[0-9]+`' | head -1 | tr -d '`')"
    if [ "$found" != "$expected" ]; then
      drifted="${drifted}\n  $1:${line%%:*} says '${found:-none}', CHANGELOG.md says '$expected'"
    fi
  }
  check_prose README.md            '^\*\*`[0-9].*` is the current release\.\*\*'
  check_prose AGENTS.md            '^\*\*Status: released\. `[0-9]'
  check_prose docs/CONTRIBUTING.md '^muninn is feature-complete and released'

  if [ -z "$drifted" ]; then
    pass "README, AGENTS.md and CONTRIBUTING.md all name $expected"
  else
    fail "the release named in prose has drifted:$(printf '%b' "$drifted")"
  fi
fi

echo
if [ "$failures" -eq 0 ]; then
  echo "${GREEN}Design package verification passed.${NC}"
else
  echo "${RED}${failures} check(s) failed.${NC}"
fi
exit "$failures"
