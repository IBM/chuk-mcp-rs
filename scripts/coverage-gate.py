#!/usr/bin/env python3
"""Enforce a *per-file* line-coverage floor.

`cargo llvm-cov --fail-under-lines N` compares the crate **total** against N,
so a new file at 50% passes as long as the rest of the crate carries it. This
script checks every file independently, which is the property we actually want.

Usage:
    cargo llvm-cov --package chuk-mcp --ignore-filename-regex 'bin/' \
        --json --output-path coverage.json
    python3 scripts/coverage-gate.py coverage.json --min-lines 90

Exits 1 if any file is below the floor. Files excluded by
--ignore-filename-regex never reach the JSON, so exclusions stay declared in
one place: the cargo invocation.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# Passing files this close to the floor are reported but not failed: one
# uncovered line away from breaking CI is worth seeing before it happens.
TIGHT_MARGIN = 2.0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path, help="cargo llvm-cov --json output")
    parser.add_argument(
        "--min-lines",
        type=float,
        default=90.0,
        help="minimum line coverage percent per file (default: 90)",
    )
    args = parser.parse_args()

    try:
        report = json.loads(args.report.read_text())
    except FileNotFoundError:
        print(f"coverage-gate: no such report: {args.report}", file=sys.stderr)
        return 2
    except json.JSONDecodeError as exc:
        print(f"coverage-gate: {args.report} is not valid JSON: {exc}", file=sys.stderr)
        return 2

    try:
        files = report["data"][0]["files"]
    except (KeyError, IndexError) as exc:
        print(f"coverage-gate: unexpected report structure: {exc}", file=sys.stderr)
        return 2

    if not files:
        # An empty file list means the filters matched nothing. Treating that as
        # a pass would make a mistyped --ignore-filename-regex look like success.
        print("coverage-gate: report contains no files — check the filters", file=sys.stderr)
        return 2

    root = Path.cwd()
    rows = []
    for entry in files:
        lines = entry["summary"]["lines"]
        count = lines["count"]
        if count == 0:
            continue  # nothing coverable; percent would be meaningless
        try:
            name = str(Path(entry["filename"]).relative_to(root))
        except ValueError:
            name = entry["filename"]
        rows.append((lines["percent"], count, lines["covered"], name))

    rows.sort()
    failed = [r for r in rows if r[0] < args.min_lines]
    tight = [r for r in rows if args.min_lines <= r[0] < args.min_lines + TIGHT_MARGIN]

    width = max(len(r[3]) for r in rows)
    print(f"Per-file line coverage (floor: {args.min_lines:.0f}%)\n")
    for percent, count, covered, name in rows:
        mark = "FAIL" if percent < args.min_lines else "ok"
        print(f"  {mark:<4}  {name:<{width}}  {percent:6.2f}%  ({covered}/{count} lines)")

    total = sum(r[1] for r in rows)
    total_covered = sum(r[2] for r in rows)
    print(f"\n  total: {100.0 * total_covered / total:.2f}%  ({total_covered}/{total} lines)")

    if tight:
        print(
            f"\nWithin {TIGHT_MARGIN:.0f} points of the floor "
            f"— a small regression will fail CI:"
        )
        for percent, count, covered, name in tight:
            slack = covered - int(-(-args.min_lines * count // 100))
            print(f"  {name} at {percent:.2f}% ({slack} line(s) of slack)")

    if failed:
        print(f"\n{len(failed)} file(s) below {args.min_lines:.0f}% line coverage:", file=sys.stderr)
        for percent, count, covered, name in failed:
            need = int(-(-args.min_lines * count // 100)) - covered
            print(
                f"  {name}: {percent:.2f}% — {need} more covered line(s) needed",
                file=sys.stderr,
            )
        return 1

    print(f"\nAll {len(rows)} files at or above {args.min_lines:.0f}%.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
