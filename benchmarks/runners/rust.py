"""The native Rust client — the floor the other runners are measured against.

Its driver binary is built by the orchestrator along with the demo server, so
this runner has nothing of its own to prepare.
"""

from __future__ import annotations

from .. import config
from .base import Runner, Workload


class RustNativeRunner(Runner):
    label = "rust-native"
    description = "chuk-mcp crate, release build, no Python involved"

    def command(self, workload: Workload) -> list[str]:
        driver = config.CARGO_TARGET_DIR / config.RUST_DRIVER_BINARY_NAME
        return [str(driver), *workload.driver_flags()]
