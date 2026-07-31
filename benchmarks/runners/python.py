"""The two Python clients: the PyO3 bindings, and the pure-Python baseline.

Both drive the same standalone driver script; they differ only in the
environment it runs under and the module it imports. Keeping the driver
shared is what makes the comparison fair — neither client gets a faster loop
than the other.
"""

from __future__ import annotations

from pathlib import Path

from .. import config, environments
from .base import Runner, Workload


class _PythonRunner(Runner):
    """Shared machinery for a runner that executes the Python driver."""

    #: Virtualenv directory name under ``.bench/venvs``.
    venv_name: str
    #: Module the driver imports.
    module: str

    def __init__(self) -> None:
        self._python: Path | None = None

    def _interpreter(self) -> Path:
        if self._python is None:
            raise RuntimeError(f"{self.label}: prepare() has not been called")
        return self._python

    def command(self, workload: Workload) -> list[str]:
        return [
            str(self._interpreter()),
            str(config.PYTHON_DRIVER),
            config.Flag.MODULE,
            self.module,
            *workload.driver_flags(),
        ]


class BindingsRunner(_PythonRunner):
    label = "python-bindings"
    description = "chuk_mcp_rs — the Rust core called through PyO3"
    venv_name = "bindings"
    module = config.BINDINGS_MODULE

    def prepare(self) -> None:
        venv = environments.create_venv(self.venv_name)
        wheel = environments.build_bindings_wheel(venv)
        environments.install(
            venv, [str(wheel), "--reinstall"], description="installing the wheel"
        )
        self._python = environments.venv_python(venv)


class PurePythonRunner(_PythonRunner):
    label = "pure-python"
    description = f"{config.PURE_PYTHON_REQUIREMENT} — the last release before the Rust core"
    venv_name = "pure-python"
    module = config.PURE_PYTHON_MODULE

    def prepare(self) -> None:
        venv = environments.create_venv(self.venv_name)
        environments.install(
            venv,
            [config.PURE_PYTHON_REQUIREMENT],
            description=f"installing {config.PURE_PYTHON_REQUIREMENT}",
        )
        self._python = environments.venv_python(venv)
