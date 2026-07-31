#!/usr/bin/env bash
#
# The full conformance run: our own two-era rule suite, then the official
# @modelcontextprotocol/conformance scenarios.
#
# Stage 1  in-repo suite    — every rule, both eras, client and server.
# Stage 2  official client  — the reference scenarios our client passes.
# Stage 3  known gaps       — reference scenarios we do not pass yet. Opt-in
#                             (--gaps); reported, never fatal.
#
# Server-side reference scenarios are not run: the official suite drives a
# server over `--url`, and this crate's server has no HTTP serving mode yet
# (stdio only). Stage 1 covers the server's behaviour in the meantime.
#
# The draft (2026-07-28) client scenarios upstream are auth-only, so the modern
# era is covered by stage 1 rather than by the reference suite.
#
# Usage: scripts/run-conformance.sh [--gaps]
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFORMANCE="@modelcontextprotocol/conformance@0.1.16"

# Scenarios our client passes at every version below. These block the run.
CLIENT_SCENARIOS=("initialize" "tools_call")
CLIENT_VERSIONS=("2025-06-18" "2025-11-25")

# Scenarios that exist only at a single version, as "scenario:version".
# Also blocking.
PINNED_SCENARIOS=("elicitation-sep1034-client-defaults:2025-11-25")

# Scenarios needing client features we have not built yet, as
# "scenario:version:what is missing". Reported, never fatal — the point is to
# keep the gap visible rather than to fail a build over known work.
KNOWN_GAPS=(
  "sse-retry:2025-11-25:honouring the SSE retry field and sending Last-Event-ID on reconnect"
)

RUN_GAPS=0
for arg in "$@"; do
  case "$arg" in
    --gaps) RUN_GAPS=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

failures=0

# --- Stage 1: the in-repo rule suite --------------------------------------

echo "== in-repo conformance suite (both eras, client + server) =="
if cargo test --quiet --manifest-path "$ROOT/Cargo.toml" \
     -p chuk-mcp --test conformance -- --nocapture; then
  echo "  PASS  in-repo suite"
else
  echo "  FAIL  in-repo suite"
  failures=$((failures + 1))
fi
echo

# --- Stage 2: the official client scenarios -------------------------------

echo "Building the conformance client runner..."
cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin chuk-mcp-conformance-client
BIN="$ROOT/target/debug/chuk-mcp-conformance-client"
echo

for ver in "${CLIENT_VERSIONS[@]}"; do
  for sc in "${CLIENT_SCENARIOS[@]}"; do
    echo "== official client: ${sc} @ ${ver} =="
    if npx --yes "$CONFORMANCE" client \
        --command "$BIN" --scenario "$sc" --spec-version "$ver"; then
      echo "  PASS  ${sc} @ ${ver}"
    else
      echo "  FAIL  ${sc} @ ${ver}"
      failures=$((failures + 1))
    fi
  done
done

for entry in "${PINNED_SCENARIOS[@]}"; do
  IFS=':' read -r sc ver <<< "$entry"
  echo "== official client: ${sc} @ ${ver} =="
  if npx --yes "$CONFORMANCE" client \
      --command "$BIN" --scenario "$sc" --spec-version "$ver"; then
    echo "  PASS  ${sc} @ ${ver}"
  else
    echo "  FAIL  ${sc} @ ${ver}"
    failures=$((failures + 1))
  fi
done

# --- Stage 3: known gaps ---------------------------------------------------

echo
if [ "$RUN_GAPS" -eq 1 ]; then
  echo "== known gaps (reported, non-blocking) =="
  for entry in "${KNOWN_GAPS[@]}"; do
    IFS=':' read -r sc ver missing <<< "$entry"
    echo "-- ${sc} @ ${ver} — needs: ${missing}"
    if npx --yes "$CONFORMANCE" client \
        --command "$BIN" --scenario "$sc" --spec-version "$ver" >/dev/null 2>&1; then
      echo "  NOW PASSING — promote it into CLIENT_SCENARIOS"
    else
      echo "  still failing, as expected"
    fi
  done
else
  echo "Known gaps not run (pass --gaps to check them):"
  for entry in "${KNOWN_GAPS[@]}"; do
    IFS=':' read -r sc ver missing <<< "$entry"
    echo "  ${sc} @ ${ver} — needs: ${missing}"
  done
fi

# --- Summary ---------------------------------------------------------------

blocking=$(( ${#CLIENT_VERSIONS[@]} * ${#CLIENT_SCENARIOS[@]} + ${#PINNED_SCENARIOS[@]} + 1 ))
echo
if [ "$failures" -eq 0 ]; then
  echo "Conformance: all ${blocking} blocking check(s) passed."
else
  echo "Conformance: ${failures} of ${blocking} blocking check(s) failed."
fi
exit "$failures"
