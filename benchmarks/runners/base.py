"""The runner interface and the workload every runner is given."""

from __future__ import annotations

import json
import subprocess
from abc import ABC, abstractmethod
from dataclasses import dataclass
from pathlib import Path

from .. import config


#: How long the server may take to exit once its stdin is closed. Generous:
#: this is a one-off warm-up, not something any measurement depends on.
_SERVER_WARMUP_TIMEOUT_SECONDS = 10


class RunnerError(RuntimeError):
    """A driver failed to produce a result."""


@dataclass(frozen=True)
class Workload:
    """What every runner is asked to do.

    Identical across runners by construction — the orchestrator builds one and
    hands the same instance to each — so no runner can quietly measure a
    cheaper job than its neighbours.
    """

    server_command: Path
    iterations: int
    warmup: int
    tool: str = config.WORKLOAD_TOOL
    argument_name: str = config.WORKLOAD_ARGUMENT_NAME
    argument_value: str = config.WORKLOAD_ARGUMENT_VALUE

    def warm_server_binary(self) -> None:
        """Execute the server once and discard it.

        The first execution of a freshly built binary pays for page-cache
        misses and, on macOS, signature validation. Whichever runner went
        first would otherwise absorb that cost in its handshake figure and
        look slow to connect for reasons that have nothing to do with its
        client.
        """
        process = subprocess.Popen(
            [str(self.server_command)],
            stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if process.stdin is not None:
            process.stdin.close()
        process.wait(timeout=_SERVER_WARMUP_TIMEOUT_SECONDS)

    def driver_flags(self) -> list[str]:
        """The workload half of a driver's command line."""
        return [
            config.Flag.SERVER_COMMAND,
            str(self.server_command),
            config.Flag.ITERATIONS,
            str(self.iterations),
            config.Flag.WARMUP,
            str(self.warmup),
            config.Flag.TOOL,
            self.tool,
            config.Flag.ARGUMENT_NAME,
            self.argument_name,
            config.Flag.ARGUMENT_VALUE,
            self.argument_value,
        ]


class Runner(ABC):
    """One client under test."""

    #: Short name used to select the runner and to label its row.
    label: str
    #: One line explaining what is actually being measured.
    description: str

    def prepare(self) -> None:
        """Build or install whatever this runner's driver needs. May be slow.

        Shared setup — the Rust binaries every runner depends on — is the
        orchestrator's job, so a runner with nothing of its own to do leaves
        this alone.
        """

    @abstractmethod
    def command(self, workload: Workload) -> list[str]:
        """The driver invocation for this workload."""

    def run(self, workload: Workload) -> dict:
        """Execute the driver and return its parsed report."""
        completed = subprocess.run(
            self.command(workload), capture_output=True, text=True
        )
        if completed.returncode != 0:
            raise RunnerError(
                f"{self.label}: driver exited {completed.returncode}\n"
                f"{completed.stderr.strip()}"
            )
        try:
            return json.loads(completed.stdout)
        except json.JSONDecodeError as error:
            raise RunnerError(
                f"{self.label}: driver output was not JSON ({error}):\n"
                f"{completed.stdout.strip()[:500]}"
            ) from error
