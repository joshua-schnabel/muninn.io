#!/usr/bin/env bash
# Verification suite for the WP0 design package.
#
# Checks the things a documentation-and-schema deliverable can actually be wrong
# about: does the workspace build, do the example configs parse, does the target
# Telegraf format really work, does the documentation describe options that
# exist, and is the pinned Telegraf the binary we think it is.
#
# WP12 lifts checks 3, 4 and 5 into CI. Until then, run this by hand:
#   bash scripts/verify-design-package.sh
#
# Requires: cargo, python3, docker, curl, sha256sum.

set -uo pipefail

TELEGRAF_VERSION="1.39.2"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

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
info "1/7  cargo gates"
cargo fmt --all -- --check          >/dev/null 2>&1 && pass "cargo fmt"    || fail "cargo fmt"
cargo metadata --locked --format-version 1 >/dev/null 2>&1 \
  && pass "cargo metadata --locked" || fail "cargo metadata --locked (Cargo.lock out of date?)"
cargo clippy --workspace --all-targets --all-features -- -D warnings >/dev/null 2>&1 \
  && pass "cargo clippy -D warnings" || fail "cargo clippy"
cargo test --workspace --locked     >/dev/null 2>&1 && pass "cargo test"   || fail "cargo test"

# ── 2. The example configurations are valid YAML ─────────────────────────────
info "2/7  example configurations parse"
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
info "3/7  reference config accepted by Telegraf ${TELEGRAF_VERSION}"
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
info "4/7  sub-table ordering fixtures (ADR-0007)"
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

# ── 5. Documented plugin options exist upstream ──────────────────────────────
# Catches documentation drifting away from the Telegraf version actually shipped
# (risk R5).
info "5/7  plugin options exist in Telegraf ${TELEGRAF_VERSION}"
TELEGRAF_VERSION="$TELEGRAF_VERSION" python3 - <<'PY' && pass "every plugin option exists upstream" || fail "unknown plugin option(s)"
import re, os, pathlib, sys, urllib.request
ver = "v" + os.environ["TELEGRAF_VERSION"]
base = f"https://raw.githubusercontent.com/influxdata/telegraf/{ver}/plugins"
conf = pathlib.Path('docs/reference/telegraf.reference.conf').read_text(encoding='utf-8')

pairs, plugin, in_sub = set(), None, False
for line in conf.splitlines():
    s = line.strip()
    if not s or s.startswith('#'):
        continue
    m = re.match(r'\[\[(inputs|outputs)\.(\w+)\]\]', s)
    if m:
        plugin, in_sub = f'{m.group(1)}/{m.group(2)}', False
        continue
    # Sub-table keys are tag names, not plugin options — skip them.
    if re.match(r'\[(inputs|outputs)\.\w+\.\w+\]', s):
        in_sub = True
        continue
    if s == '[agent]':
        plugin, in_sub = None, False
        continue
    m = re.match(r'(\w+)\s*=', s)
    if m and plugin and not in_sub:
        pairs.add((plugin, m.group(1)))

cache, missing = {}, []
for plug, opt in sorted(pairs):
    if plug not in cache:
        try:
            cache[plug] = urllib.request.urlopen(f'{base}/{plug}/sample.conf', timeout=30).read().decode()
        except Exception as e:
            print(f'  could not fetch {plug}: {e}')
            cache[plug] = ''
    if not re.search(rf'^\s*#?\s*{re.escape(opt)}\s*=', cache[plug], re.M):
        missing.append(f'{plug}.{opt}')
for m in missing:
    print('  not found:', m)
sys.exit(1 if missing else 0)
PY

# ── 6. The pinned Telegraf checksums match upstream and ADR-0011 ─────────────
info "6/7  Telegraf tarball checksums"
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

# ── 7. Every relative documentation link resolves ────────────────────────────
info "7/7  documentation cross-references"
python3 - <<'PY' && pass "all relative markdown links resolve" || fail "broken markdown link(s)"
import re, pathlib, sys, urllib.parse
bad, checked = [], 0
for md in sorted(pathlib.Path('.').rglob('*.md')):
    if 'target' in md.parts:
        continue
    for m in re.finditer(r'\[[^\]]*\]\(([^)]+)\)', md.read_text(encoding='utf-8')):
        link = m.group(1).strip()
        if link.startswith(('http://', 'https://', 'mailto:', '#')):
            continue
        path = link.partition('#')[0]
        if not path:
            continue
        checked += 1
        if not (md.parent / urllib.parse.unquote(path)).resolve().exists():
            bad.append(f'{md.as_posix()} -> {link}')
print(f'  checked {checked} relative links')
for b in bad:
    print('  broken:', b)
sys.exit(1 if bad else 0)
PY

echo
if [ "$failures" -eq 0 ]; then
  echo "${GREEN}Design package verification passed.${NC}"
else
  echo "${RED}${failures} check(s) failed.${NC}"
fi
exit "$failures"
