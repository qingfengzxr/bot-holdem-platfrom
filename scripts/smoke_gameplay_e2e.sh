#!/usr/bin/env bash
set -euo pipefail

# One-click gameplay smoke validation:
# 1) gateway/room local flow (fast, no anvil)
# 2) real JSON-RPC + anvil + chain-watcher ignored E2E
#
# Usage:
#   scripts/smoke_gameplay_e2e.sh
#   scripts/smoke_gameplay_e2e.sh --online

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "${ROOT_DIR}"

OFFLINE_FLAG="--offline"
if [[ "${1:-}" == "--online" ]]; then
  OFFLINE_FLAG=""
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found in PATH" >&2
  exit 1
fi

if ! command -v anvil >/dev/null 2>&1; then
  echo "error: anvil not found in PATH (install Foundry first)" >&2
  exit 1
fi

echo "==> gameplay smoke preflight (gateway/room local flow)"
cargo test ${OFFLINE_FLAG} -p app-server room_flow_works_via_rpc_gateway_handlers -- --nocapture

echo "==> gameplay smoke e2e (real json-rpc + anvil + chain-watcher)"
cargo test ${OFFLINE_FLAG} -p app-server rpc_and_chain_path_end_to_end_with_real_jsonrpc_and_anvil -- --ignored --nocapture

echo
echo "Smoke gameplay E2E passed."
