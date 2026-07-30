#!/usr/bin/env bash
#
# Runs the official MCP conformance suite (@modelcontextprotocol/conformance)
# against chuk-mcp's client, via the `chuk-mcp-conformance-client` runner binary.
#
# Scope today: the *core client* scenarios (initialize, tools_call) under the
# stateful date versions 2025-06-18 and 2025-11-25. The draft (2026-07-28)
# *client* scenarios are auth-only (Phase 5), and modern *server* conformance
# needs the not-yet-built modern chuk server — both are tracked in the roadmap.
#
# Usage: scripts/run-conformance.sh
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFORMANCE="@modelcontextprotocol/conformance@0.1.16"
SCENARIOS=("initialize" "tools_call")
VERSIONS=("2025-06-18" "2025-11-25")

echo "Building the conformance client runner..."
cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin chuk-mcp-conformance-client
BIN="$ROOT/target/debug/chuk-mcp-conformance-client"

failures=0
for ver in "${VERSIONS[@]}"; do
  for sc in "${SCENARIOS[@]}"; do
    echo "== conformance client: ${sc} @ ${ver} =="
    if npx --yes "$CONFORMANCE" client \
        --command "$BIN" --scenario "$sc" --spec-version "$ver"; then
      echo "  PASS  ${sc} @ ${ver}"
    else
      echo "  FAIL  ${sc} @ ${ver}"
      failures=$((failures + 1))
    fi
  done
done

echo
if [ "$failures" -eq 0 ]; then
  echo "Conformance: all $(( ${#VERSIONS[@]} * ${#SCENARIOS[@]} )) client scenarios passed."
else
  echo "Conformance: ${failures} scenario(s) failed."
fi
exit "$failures"
