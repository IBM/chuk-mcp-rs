"""The clients under test.

Each runner prepares whatever it needs, then executes a driver process that
speaks the contract in ``benchmarks/README.md``. Adding a client to the
comparison means adding a runner here, not touching the orchestrator.
"""

from __future__ import annotations

from .base import Runner, Workload
from .python import BindingsRunner, PurePythonRunner
from .rust import RustNativeRunner

#: Every runner the harness knows about, in the order they are run.
ALL_RUNNERS: tuple[Runner, ...] = (
    RustNativeRunner(),
    BindingsRunner(),
    PurePythonRunner(),
)


def by_label(labels: list[str]) -> list[Runner]:
    """Select runners by label, rejecting unknown names rather than ignoring
    them: a typo that silently ran nothing would look like a passing run."""
    known = {runner.label: runner for runner in ALL_RUNNERS}
    unknown = [label for label in labels if label not in known]
    if unknown:
        raise KeyError(
            f"unknown runner(s) {', '.join(unknown)}; "
            f"choose from {', '.join(known)}"
        )
    return [known[label] for label in labels]


__all__ = [
    "ALL_RUNNERS",
    "BindingsRunner",
    "PurePythonRunner",
    "Runner",
    "RustNativeRunner",
    "Workload",
    "by_label",
]
