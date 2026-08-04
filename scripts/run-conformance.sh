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
# Stage 4  official server  — the reference server scenarios, all of them.
#                             Blocking: every scenario passes, so anything that
#                             stops passing is a regression rather than news.
# Stage 5  modern server    — the same, at 2026-07-28.
#
# The 2026-07-28 client scenarios run in stage 2b. Only the authorization ones
# are skipped, and only because this library implements no OAuth.
#
# Usage: scripts/run-conformance.sh [--gaps]
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFORMANCE="@modelcontextprotocol/conformance@0.1.16"
# The 2026-07-28 scenarios exist only in the 0.2.0 prerelease line. Pinned
# rather than floating on `next`: a prerelease that adds a scenario overnight
# should turn up as a deliberate bump, not as a red build nobody changed.
CONFORMANCE_MODERN="@modelcontextprotocol/conformance@0.2.0-alpha.10"

# Every 2026-07-28 server scenario this build passes. The suite's default run
# only reaches some of them, so they are named — and a scenario missing from
# this list is a scenario nobody is running.
MODERN_SERVER_SCENARIOS=(
  server-stateless
  caching
  http-header-validation
  http-custom-header-server-validation
  json-schema-2020-12
  sep-2164-resource-not-found
  completion-complete
  tools-list
  tools-call-simple-text
  tools-call-image
  tools-call-audio
  tools-call-embedded-resource
  tools-call-mixed-content
  tools-call-error
  tools-call-with-progress
  server-sse-multiple-streams
  resources-list
  resources-read-text
  resources-read-binary
  resources-templates-read
  prompts-list
  prompts-get-simple
  prompts-get-with-args
  prompts-get-embedded-resource
  prompts-get-with-image
  dns-rebinding-protection
  input-required-result-basic-elicitation
  input-required-result-basic-sampling
  input-required-result-basic-list-roots
  input-required-result-request-state
  input-required-result-multiple-input-requests
  input-required-result-multi-round
  input-required-result-missing-input-response
  input-required-result-non-tool-request
  input-required-result-result-type
  input-required-result-unsupported-methods
  input-required-result-tampered-state
  input-required-result-capability-check
  input-required-result-ignore-extra-params
  input-required-result-validate-input
)

# Scenarios our client passes at every version below. These block the run.
CLIENT_SCENARIOS=("initialize" "tools_call")
CLIENT_VERSIONS=("2025-06-18" "2025-11-25")

# Scenarios that exist only at a single version, as "scenario:version".
# Also blocking.
PINNED_SCENARIOS=(
  "elicitation-sep1034-client-defaults:2025-11-25"
  "sse-retry:2025-11-25"
)

# The 2026-07-28 client scenarios this build passes, run against the prerelease
# suite — including authorization. The only ones absent are the auth
# *extensions* (DPoP, client credentials, JWT bearer), which are separate
# optional specifications this library does not implement.
MODERN_CLIENT_SCENARIOS=(
  tools_call
  request-metadata
  sep-2322-client-request-state
  http-standard-headers
  http-custom-headers
  http-invalid-tool-headers
  json-schema-ref-no-deref
  auth/metadata-default
  auth/metadata-var1
  auth/metadata-var2
  auth/metadata-var3
  auth/basic-cimd
  auth/pre-registration
  auth/resource-mismatch
  auth/scope-from-www-authenticate
  auth/scope-from-scopes-supported
  auth/scope-omitted-when-undefined
  auth/scope-step-up
  auth/scope-retry-limit
  auth/token-endpoint-auth-basic
  auth/token-endpoint-auth-post
  auth/token-endpoint-auth-none
  auth/offline-access-scope
  auth/offline-access-not-supported
  auth/authorization-server-migration
  auth/metadata-issuer-mismatch
  auth/iss-supported
  auth/iss-not-advertised
  auth/iss-supported-missing
  auth/iss-wrong-issuer
  auth/iss-unexpected
  auth/iss-normalized
)

# The authorization scenarios that exist at the legacy revision too. Run
# there as well because the two eras reach the token through different
# transports, and a fix to one says nothing about the other.
LEGACY_AUTH_SCENARIOS=(
  auth/metadata-default
  auth/basic-cimd
  auth/pre-registration
  auth/scope-step-up
  auth/scope-retry-limit
  auth/token-endpoint-auth-basic
  auth/token-endpoint-auth-post
  auth/token-endpoint-auth-none
)

# Scenarios needing client features we have not built yet, as
# "scenario:version:what is missing". Reported, never fatal — the point is to
# keep the gap visible rather than to fail a build over known work.
# Empty: every client scenario the suite offers at a version we support now
# passes. Entries take the form "scenario:version:what is missing".
KNOWN_GAPS=()

SERVER_BIND="127.0.0.1:8930"

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

echo
echo "== official client scenarios (2026-07-28) =="
for sc in "${MODERN_CLIENT_SCENARIOS[@]}"; do
  # Captured, then matched. The verdict is the suite's own summary line: its
  # exit status also reflects warnings, and `pipefail` would turn a scenario
  # that passed with a warning into a failed build.
  out="$(npx --yes "$CONFORMANCE_MODERN" client \
           --command "$BIN" --scenario "$sc" --spec-version 2026-07-28 2>&1 || true)"
  if printf '%s\n' "$out" | grep -qE "^Passed: [0-9]+/[0-9]+, 0 failed"; then
    echo "  PASS  ${sc} @ 2026-07-28"
  else
    echo "  FAIL  ${sc} @ 2026-07-28"
    printf '%s\n' "$out" | grep -E "FAILURE|Error:" | head -3
    failures=$((failures + 1))
  fi
done

echo
echo "== official client scenarios (authorization @ 2025-11-25) =="
for sc in "${LEGACY_AUTH_SCENARIOS[@]}"; do
  out="$(npx --yes "$CONFORMANCE_MODERN" client \
           --command "$BIN" --scenario "$sc" --spec-version 2025-11-25 2>&1 || true)"
  if printf '%s\n' "$out" | grep -qE "^Passed: [0-9]+/[0-9]+, 0 failed"; then
    echo "  PASS  ${sc} @ 2025-11-25"
  else
    echo "  FAIL  ${sc} @ 2025-11-25"
    printf '%s\n' "$out" | grep -E "FAILURE|Error:" | head -3
    failures=$((failures + 1))
  fi
done

# --- Stage 3: known gaps ---------------------------------------------------

echo
if [ "$RUN_GAPS" -eq 1 ]; then
  echo "== known gaps (reported, non-blocking) =="
  for entry in ${KNOWN_GAPS[@]+"${KNOWN_GAPS[@]}"}; do
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
  if [ ${#KNOWN_GAPS[@]} -eq 0 ]; then
    echo "No known client-scenario gaps."
  else
    echo "Known gaps not run (pass --gaps to check them):"
  fi
  for entry in ${KNOWN_GAPS[@]+"${KNOWN_GAPS[@]}"}; do
    IFS=':' read -r sc ver missing <<< "$entry"
    echo "  ${sc} @ ${ver} — needs: ${missing}"
  done
fi

# --- Stage 4: the official server scenarios --------------------------------

echo
echo "== official server scenarios =="
cargo build --quiet --manifest-path "$ROOT/Cargo.toml" --bin chuk-mcp-conformance-server
"$ROOT/target/debug/chuk-mcp-conformance-server" "$SERVER_BIND" >/dev/null 2>&1 &
server_pid=$!
sleep 2

if npx --yes "$CONFORMANCE" server \
     --url "http://${SERVER_BIND}/mcp" --spec-version 2025-11-25 2>&1 | tail -3; then
  echo "  PASS  official server scenarios (legacy)"
else
  echo "  FAIL  official server scenarios (legacy)"
  failures=$((failures + 1))
fi

kill "$server_pid" 2>/dev/null
wait "$server_pid" 2>/dev/null

# --- Stage 5: the official server scenarios at 2026-07-28 ------------------
#
# A separate stage because the modern scenarios live in a prerelease of the
# suite, and because the default run does not select them: several are only
# reached by naming them, so the list below is explicit rather than a wildcard.
# Anything not named here is not being checked.

echo
echo "== official server scenarios (2026-07-28) =="
"$ROOT/target/debug/chuk-mcp-conformance-server" "$SERVER_BIND" >/dev/null 2>&1 &
server_pid=$!
sleep 2

modern_failures=0
for sc in "${MODERN_SERVER_SCENARIOS[@]}"; do
  out="$(npx --yes "$CONFORMANCE_MODERN" server \
           --url "http://${SERVER_BIND}/mcp" --spec-version 2026-07-28 \
           --scenario "$sc" 2>&1 || true)"
  if printf '%s\n' "$out" | grep -qE "^Passed: [0-9]+/[0-9]+, 0 failed"; then
    echo "  PASS  ${sc}"
  else
    echo "  FAIL  ${sc}"
    printf '%s\n' "$out" | grep -E "FAILURE|Error:" | head -3
    modern_failures=$((modern_failures + 1))
  fi
done

if [ "$modern_failures" -eq 0 ]; then
  echo "  PASS  all ${#MODERN_SERVER_SCENARIOS[@]} modern server scenarios"
else
  failures=$((failures + 1))
fi

kill "$server_pid" 2>/dev/null
wait "$server_pid" 2>/dev/null

# --- Summary ---------------------------------------------------------------

# The in-repo suite, the client scenarios, and the two server suites.
blocking=$(( ${#CLIENT_VERSIONS[@]} * ${#CLIENT_SCENARIOS[@]} + ${#PINNED_SCENARIOS[@]} \
             + ${#MODERN_CLIENT_SCENARIOS[@]} + ${#LEGACY_AUTH_SCENARIOS[@]} + 3 ))
echo
if [ "$failures" -eq 0 ]; then
  echo "Conformance: all ${blocking} blocking check(s) passed."
else
  echo "Conformance: ${failures} of ${blocking} blocking check(s) failed."
fi
exit "$failures"
