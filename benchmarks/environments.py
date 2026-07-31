"""Provisioning the isolated environments the Python runners need.

The two Python clients under test cannot share an interpreter: one is the
Rust-backed ``chuk_mcp_rs`` extension built from this working tree, the other
is a pinned pure-Python release from PyPI. Each gets its own virtualenv, built
with ``uv`` and cached under ``.bench/`` so repeat runs are cheap.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

from . import config

#: The tool used for every environment operation. Chosen over ``pip`` because
#: it resolves and installs fast enough that provisioning is not itself a
#: reason to skip running the benchmarks.
UV = "uv"


class ProvisioningError(RuntimeError):
    """An environment could not be prepared, so its runner must be skipped."""


def require_uv() -> None:
    if shutil.which(UV) is None:
        raise ProvisioningError(
            f"{UV!r} is not on PATH; install it from https://docs.astral.sh/uv/"
        )


def run(command: list[str], *, description: str) -> subprocess.CompletedProcess:
    """Run a provisioning command, surfacing its output only on failure."""
    completed = subprocess.run(command, capture_output=True, text=True)
    if completed.returncode != 0:
        raise ProvisioningError(
            f"{description} failed ({' '.join(command)}):\n{completed.stderr.strip()}"
        )
    return completed


def venv_python(venv: Path) -> Path:
    """The interpreter inside a virtualenv, on either layout."""
    candidates = (venv / "bin" / "python", venv / "Scripts" / "python.exe")
    for candidate in candidates:
        if candidate.exists():
            return candidate
    raise ProvisioningError(f"no interpreter found in {venv}")


def create_venv(name: str) -> Path:
    """An empty virtualenv under the cache directory, created if absent."""
    require_uv()
    venv = config.VENV_DIR / name
    if not venv.exists():
        run([UV, "venv", str(venv)], description=f"creating the {name} virtualenv")
    return venv


def install(venv: Path, requirements: list[str], *, description: str) -> None:
    run(
        [UV, "pip", "install", "--python", str(venv_python(venv)), *requirements],
        description=description,
    )


def build_bindings_wheel(venv: Path) -> Path:
    """Build the PyO3 extension from this working tree and return the wheel.

    Built with the same virtualenv's maturin so the wheel matches the
    interpreter that will import it.
    """
    install(venv, [config.MATURIN_REQUIREMENT], description="installing maturin")
    wheel_dir = config.WHEEL_DIR
    wheel_dir.mkdir(parents=True, exist_ok=True)

    run(
        [
            str(venv_python(venv)),
            "-m",
            "maturin",
            "build",
            config.CARGO_PROFILE_FLAG,
            "--manifest-path",
            str(config.BINDINGS_MANIFEST),
            "--interpreter",
            str(venv_python(venv)),
            "--out",
            str(wheel_dir),
        ],
        description="building the chuk-mcp-rs wheel",
    )

    wheels = sorted(wheel_dir.glob("*.whl"), key=lambda path: path.stat().st_mtime)
    if not wheels:
        raise ProvisioningError(f"maturin produced no wheel in {wheel_dir}")
    return wheels[-1]


def cargo_build(binaries: list[str]) -> None:
    """Build the release binaries the harness drives."""
    if shutil.which("cargo") is None:
        raise ProvisioningError("'cargo' is not on PATH")
    command = [
        "cargo",
        "build",
        config.CARGO_PROFILE_FLAG,
        "--manifest-path",
        str(config.CARGO_MANIFEST),
    ]
    for binary in binaries:
        command += ["--bin", binary]
    run(command, description="building the Rust binaries")


def report_skip(label: str, error: Exception) -> None:
    print(f"  skipped {label}: {error}", file=sys.stderr)
