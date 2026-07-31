"""Rendering benchmark results.

Two outputs: a Markdown table for the README and terminal, and a JSON document
for machine comparison between runs.
"""

from __future__ import annotations

import json
import platform
import sys
from pathlib import Path

from . import config
from .summaries import Summary

#: Column headings, in render order. The baseline column is filled in against
#: the slowest runner, so the table reads the same however many runners ran.
_HEADINGS = ("Client", "Handshake", "Mean/call", "Calls/sec", "vs slowest")


def _percentile_headings() -> tuple[str, ...]:
    return tuple(f"p{rank}" for rank in config.REPORTED_PERCENTILES)


def render_table(summaries: list[Summary]) -> str:
    """A Markdown table, fastest first."""
    if not summaries:
        return "_No runners completed._"

    ordered = sorted(summaries, key=lambda summary: summary.mean_seconds)
    slowest = ordered[-1].mean_seconds

    headings = _HEADINGS + _percentile_headings()
    rows = [
        "| " + " | ".join(headings) + " |",
        "| " + " | ".join("---" for _ in headings) + " |",
    ]
    for summary in ordered:
        cells = [
            summary.label,
            f"{summary.handshake_milliseconds:.1f} ms",
            f"{summary.mean_microseconds():.1f} µs",
            f"{summary.calls_per_second:,.0f}",
            f"{slowest / summary.mean_seconds:.1f}×",
        ]
        cells += [
            f"{summary.percentile_microseconds(rank):.1f} µs"
            for rank in config.REPORTED_PERCENTILES
        ]
        rows.append("| " + " | ".join(cells) + " |")
    return "\n".join(rows)


def environment() -> dict:
    """The facts that make a number reproducible — or explain why it is not."""
    return {
        "platform": platform.platform(),
        "processor": platform.processor() or platform.machine(),
        "python": sys.version.split()[0],
    }


def to_document(summaries: list[Summary], iterations: int, warmup: int) -> dict:
    return {
        "environment": environment(),
        "workload": {
            "tool": config.WORKLOAD_TOOL,
            "iterations": iterations,
            "warmup": warmup,
        },
        "runners": [
            {
                "label": summary.label,
                "runner": summary.runner,
                "iterations": summary.iterations,
                "handshake_seconds": summary.handshake_seconds,
                "mean_seconds": summary.mean_seconds,
                "calls_per_second": summary.calls_per_second,
                "percentile_seconds": {
                    str(rank): value
                    for rank, value in summary.percentile_seconds.items()
                },
            }
            for summary in sorted(summaries, key=lambda summary: summary.mean_seconds)
        ],
    }


def write_document(document: dict, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(document, indent=2) + "\n")
