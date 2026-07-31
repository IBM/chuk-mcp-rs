"""Run the end-to-end benchmark comparison.

    python -m benchmarks                     # every runner, default workload
    python -m benchmarks --iterations 5000
    python -m benchmarks --runner rust-native --runner python-bindings

A runner whose environment cannot be prepared is skipped with a message
rather than failing the run: a machine without ``uv`` should still be able to
benchmark the Rust client.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from . import config, environments, report
from .environments import ProvisioningError, report_skip
from .runners import ALL_RUNNERS, Workload, by_label
from .runners.base import RunnerError
from .summaries import Summary, summarise

#: Exit status when no runner produced a result at all.
EXIT_NOTHING_RAN = 1


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--iterations",
        type=int,
        default=config.DEFAULT_ITERATIONS,
        help=f"timed tool calls per runner (default: {config.DEFAULT_ITERATIONS})",
    )
    parser.add_argument(
        "--warmup",
        type=int,
        default=config.DEFAULT_WARMUP,
        help=f"discarded calls before timing (default: {config.DEFAULT_WARMUP})",
    )
    parser.add_argument(
        "--runner",
        action="append",
        dest="runners",
        metavar="LABEL",
        help="run only this runner; repeatable. "
        f"One of: {', '.join(runner.label for runner in ALL_RUNNERS)}",
    )
    parser.add_argument(
        "--json",
        type=Path,
        metavar="PATH",
        help="also write the full results as JSON to PATH",
    )
    return parser.parse_args()


def main() -> int:
    options = parse_arguments()
    runners = by_label(options.runners) if options.runners else list(ALL_RUNNERS)

    workload = Workload(
        server_command=config.CARGO_TARGET_DIR / config.SERVER_BINARY_NAME,
        iterations=options.iterations,
        warmup=options.warmup,
    )

    print(
        f"Workload: {workload.iterations} × {workload.tool} "
        f"(after {workload.warmup} warm-up calls), "
        f"server: {config.SERVER_BINARY_NAME}\n",
        file=sys.stderr,
    )

    # Shared setup: the server every runner connects to, and the Rust driver.
    # Built and warmed once, before any runner is timed.
    try:
        environments.cargo_build(
            [config.RUST_DRIVER_BINARY_NAME, config.SERVER_BINARY_NAME]
        )
        workload.warm_server_binary()
    except (ProvisioningError, OSError) as error:
        print(f"Cannot prepare the shared benchmark server: {error}", file=sys.stderr)
        return EXIT_NOTHING_RAN

    summaries: list[Summary] = []
    for runner in runners:
        print(f"  {runner.label}: {runner.description}", file=sys.stderr)
        try:
            runner.prepare()
            summaries.append(summarise(runner.label, runner.run(workload)))
        except (ProvisioningError, RunnerError) as error:
            report_skip(runner.label, error)

    if not summaries:
        print("No runner produced a result.", file=sys.stderr)
        return EXIT_NOTHING_RAN

    print()
    print(report.render_table(summaries))

    if options.json:
        document = report.to_document(summaries, options.iterations, options.warmup)
        report.write_document(document, options.json)
        print(f"\nWrote {options.json}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
