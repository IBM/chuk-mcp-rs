"""Summarising raw driver timings.

Every runner's numbers go through this module, so a difference in the table is
a difference in the client and never a difference in how the client was
measured.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from . import config


@dataclass(frozen=True)
class Summary:
    """What one runner achieved on one workload."""

    runner: str
    label: str
    iterations: int
    handshake_seconds: float
    mean_seconds: float
    percentile_seconds: dict[int, float] = field(default_factory=dict)

    @property
    def calls_per_second(self) -> float:
        return 1.0 / self.mean_seconds if self.mean_seconds > 0 else float("inf")

    @property
    def handshake_milliseconds(self) -> float:
        return self.handshake_seconds * config.MILLISECONDS_PER_SECOND

    def mean_microseconds(self) -> float:
        return self.mean_seconds * config.MICROSECONDS_PER_SECOND

    def percentile_microseconds(self, percentile: int) -> float:
        return self.percentile_seconds[percentile] * config.MICROSECONDS_PER_SECOND


def percentile(sorted_samples: list[float], percentile_rank: int) -> float:
    """Nearest-rank percentile of an already-sorted sample list.

    Nearest-rank rather than an interpolating definition: every reported value
    is then a latency that actually occurred, which is the useful reading for
    a tail figure.
    """
    if not sorted_samples:
        raise ValueError("cannot take a percentile of an empty sample")
    rank = max(1, (percentile_rank * len(sorted_samples) + 99) // 100)
    return sorted_samples[min(rank, len(sorted_samples)) - 1]


def summarise(label: str, payload: dict) -> Summary:
    """Turn one driver's JSON output into a [`Summary`]."""
    call_seconds = payload[config.ResultKey.CALL_SECONDS]
    if not call_seconds:
        raise ValueError(f"{label}: driver reported no timed calls")

    ordered = sorted(call_seconds)
    return Summary(
        runner=payload[config.ResultKey.RUNNER],
        label=label,
        iterations=len(call_seconds),
        handshake_seconds=payload[config.ResultKey.HANDSHAKE_SECONDS],
        mean_seconds=sum(call_seconds) / len(call_seconds),
        percentile_seconds={
            rank: percentile(ordered, rank) for rank in config.REPORTED_PERCENTILES
        },
    )
