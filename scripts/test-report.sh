#!/usr/bin/env bash
# scripts/test-report.sh
#
# Turn a captured `cargo llvm-cov --workspace` run (which executes the full test
# suite and prints the coverage table) into a human-readable Markdown report.
# Used by release.yml so every GitHub Release ships with proof of what was
# tested; runnable locally against any captured output for the same view.
#
# Usage: test-report.sh <test-output.txt> [<report.md> [<summary.md>]]
#   VERSION / COMMIT env vars, when set, are stamped into the report header.
#
# Writes:
#   report.md  (default test-report.md)  — per-suite table, full test lists,
#                                          coverage table
#   summary.md (default test-summary.md) — one compact table for release notes
# Exits non-zero (with a ::error:: annotation) if the input contains no
# `test result:` lines at all — an empty report would silently ship.

set -euo pipefail

INPUT="${1:-test-output.txt}"
REPORT="${2:-test-report.md}"
SUMMARY="${3:-test-summary.md}"

if [ ! -s "$INPUT" ]; then
  echo "::error::test-report.sh: input '$INPUT' is missing or empty" >&2
  exit 1
fi

INPUT="$INPUT" REPORT="$REPORT" SUMMARY="$SUMMARY" python3 - <<'PYEOF'
import os, re, sys

inp, report_path, summary_path = os.environ["INPUT"], os.environ["REPORT"], os.environ["SUMMARY"]
version = os.environ.get("VERSION", "")
commit = os.environ.get("COMMIT", "")

# Cargo colours its status lines — `Running`, `Compiling`, `Finished` — and does
# so even when its output is piped into a file, which is how this script is fed.
# The escape sequences sit *before* the word, so every anchored pattern below
# would miss. That is not hypothetical: it is what happened on v0.1.0, the first
# time release.yml ever ran. `Running` never matched, so no suite was ever
# opened, so every `test result:` line was skipped as belonging to nothing, and
# the run failed with "no test result lines found" against a log that plainly
# contained ten of them.
#
# Stripping here rather than asking the caller for CARGO_TERM_COLOR=never: the
# input is a captured log, and a parser that only works on logs captured a
# particular way is a parser that breaks on the next caller.
ansi_re = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")

# One suite = one `Running .../deps/<name>-<hash>` (or `Doc-tests <crate>`)
# section of cargo's output, closed by its `test result:` line.
suite_re = re.compile(r"^\s*Running\s+(?:unittests\s+)?(\S+)\s+\(.*?([A-Za-z0-9_]+)-[0-9a-f]+(?:\.exe)?\)\s*$")
doctest_re = re.compile(r"^\s*Doc-tests\s+(\S+)\s*$")
test_line_re = re.compile(r"^test\s+\S+.*\.\.\.\s+(ok|FAILED|ignored)\s*$")
result_re = re.compile(
    r"^test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored;"
    r" \d+ measured; \d+ filtered out; finished in ([0-9.]+)s"
)

suites = []          # {name, source, passed, failed, ignored, duration, lines[]}
current = None
coverage = []        # verbatim coverage-table lines
in_coverage = False
total_line_cov = ""

for raw in open(inp, encoding="utf-8", errors="replace"):
    line = ansi_re.sub("", raw.rstrip("\n"))

    if line.startswith("Filename"):
        in_coverage = True
    if in_coverage:
        coverage.append(line)
        parts = line.split()
        if parts and parts[0] == "TOTAL":
            in_coverage = False
            if len(parts) >= 10:
                total_line_cov = parts[9]
        continue

    m = suite_re.match(line)
    d = doctest_re.match(line)
    if m or d:
        current = {
            "name": m.group(2) if m else f"doc-tests {d.group(1)}",
            "source": m.group(1) if m else "documentation examples",
            "passed": 0, "failed": 0, "ignored": 0, "duration": "", "lines": [],
        }
        continue
    if current is None:
        continue
    if test_line_re.match(line.strip()):
        current["lines"].append(line.strip())
        continue
    r = result_re.match(line)
    if r:
        current["passed"] = int(r.group(2))
        current["failed"] = int(r.group(3))
        current["ignored"] = int(r.group(4))
        current["duration"] = r.group(5) + "s"
        suites.append(current)
        current = None

if not suites:
    print("::error::test-report.sh: no `test result:` lines found in input", file=sys.stderr)
    sys.exit(1)

passed = sum(s["passed"] for s in suites)
failed = sum(s["failed"] for s in suites)
ignored = sum(s["ignored"] for s in suites)
status = "✅ all tests passed" if failed == 0 else f"❌ {failed} test(s) FAILED"

with open(report_path, "w", encoding="utf-8", newline="\n") as f:
    title = f"# Test report{f' — v{version}' if version else ''}\n\n"
    f.write(title)
    if commit:
        f.write(f"Commit: `{commit}`\n\n")
    f.write(f"**{status}** — {passed} passed, {failed} failed, {ignored} ignored "
            f"across {len(suites)} suites.\n\n")
    f.write("| Suite | Source | Passed | Failed | Ignored | Duration |\n")
    f.write("|---|---|---:|---:|---:|---:|\n")
    for s in suites:
        f.write(f"| `{s['name']}` | `{s['source']}` | {s['passed']} | {s['failed']} | "
                f"{s['ignored']} | {s['duration']} |\n")
    if total_line_cov:
        f.write(f"\n## Coverage\n\n**Line coverage (workspace): {total_line_cov}** "
                f"(CI gate: ≥ 80%)\n\n```\n" + "\n".join(coverage) + "\n```\n")
    f.write("\n## All tests by suite\n\n")
    for s in suites:
        if not s["lines"]:
            continue
        f.write(f"<details>\n<summary><code>{s['name']}</code> — {s['passed']} passed"
                f"{', ' + str(s['failed']) + ' FAILED' if s['failed'] else ''}</summary>\n\n"
                "```\n" + "\n".join(s["lines"]) + "\n```\n\n</details>\n\n")

with open(summary_path, "w", encoding="utf-8", newline="\n") as f:
    f.write("\n## Test results\n\n")
    f.write("| Result | Tests | Suites | Line coverage |\n|---|---|---|---|\n")
    f.write(f"| {status} | {passed} passed / {failed} failed / {ignored} ignored | "
            f"{len(suites)} | {total_line_cov or 'n/a'} |\n\n")
    f.write("The full per-suite report is attached to this release as `test-report.md`.\n")

print(f"report: {report_path} ({len(suites)} suites, {passed} passed, {failed} failed, "
      f"coverage {total_line_cov or 'n/a'})")
PYEOF
