#!/usr/bin/env bash
set -euo pipefail

# One-click smoke for batch-settlement path:
# 1) start local anvil
# 2) deploy BatchSettlement contract
# 3) run batch-path tests
# 4) boot app-server in batch mode and verify health/log marker
#
# Usage:
#   scripts/smoke_batch_settlement_e2e.sh
#   scripts/smoke_batch_settlement_e2e.sh --online

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
if ! command -v forge >/dev/null 2>&1; then
  echo "error: forge not found in PATH (install Foundry first)" >&2
  exit 1
fi
if ! command -v rg >/dev/null 2>&1; then
  echo "error: rg not found in PATH" >&2
  exit 1
fi

ANVIL_PORT="${ANVIL_PORT:-18545}"
RPC_URL="http://127.0.0.1:${ANVIL_PORT}"
DEPLOYER_PRIVATE_KEY="${DEPLOYER_PRIVATE_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}"
SETTLEMENT_FROM_ADDRESS="${SETTLEMENT_FROM_ADDRESS:-0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266}"

ANVIL_LOG="$(mktemp -t batch-settlement-anvil.XXXXXX.log)"
APP_LOG="$(mktemp -t batch-settlement-app.XXXXXX.log)"
ANVIL_PID=""
APP_PID=""

cleanup() {
  if [[ -n "${APP_PID}" ]]; then
    kill "${APP_PID}" >/dev/null 2>&1 || true
    wait "${APP_PID}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${ANVIL_PID}" ]]; then
    kill "${ANVIL_PID}" >/dev/null 2>&1 || true
    wait "${ANVIL_PID}" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

echo "==> starting anvil on :${ANVIL_PORT}"
anvil -p "${ANVIL_PORT}" --silent >"${ANVIL_LOG}" 2>&1 &
ANVIL_PID=$!

for _ in $(seq 1 30); do
  if curl -sS -X POST "${RPC_URL}" \
    -H "content-type: application/json" \
    -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done

if ! curl -sS -X POST "${RPC_URL}" \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' >/dev/null 2>&1; then
  echo "error: anvil did not become ready" >&2
  exit 1
fi

echo "==> deploying BatchSettlement contract"
DEPLOY_OUTPUT="$(
  RPC_URL="${RPC_URL}" \
  DEPLOYER_PRIVATE_KEY="${DEPLOYER_PRIVATE_KEY}" \
  "${ROOT_DIR}/scripts/deploy_batch_settlement.sh"
)"
echo "${DEPLOY_OUTPUT}"

BATCH_CONTRACT_ADDRESS="$(
  echo "${DEPLOY_OUTPUT}" | rg -o "BATCH_SETTLEMENT_CONTRACT=0x[a-fA-F0-9]{40}" | tail -n1 | cut -d'=' -f2
)"
if [[ -z "${BATCH_CONTRACT_ADDRESS}" ]]; then
  echo "error: failed to parse BATCH_SETTLEMENT_CONTRACT from deploy output" >&2
  exit 1
fi

echo "==> batch settlement unit/integration tests"
cargo test ${OFFLINE_FLAG} -p settlement batch_settlement_adapter_encodes_contract_call_data -- --nocapture
cargo test ${OFFLINE_FLAG} -p app-server orchestrates_batch_payouts_and_closes_when_confirmed -- --nocapture
cargo test ${OFFLINE_FLAG} -p app-server batch_payouts_failed_receipt_marks_manual_review_and_emits_alert -- --nocapture

echo "==> booting app-server in batch mode"
APP_SERVER__AUTO_SETTLEMENT_ENABLED=true \
SETTLEMENT_EVM_RPC_URL="${RPC_URL}" \
SETTLEMENT_FROM_ADDRESS="${SETTLEMENT_FROM_ADDRESS}" \
SETTLEMENT_BATCH_CONTRACT_ADDRESS="${BATCH_CONTRACT_ADDRESS}" \
cargo run ${OFFLINE_FLAG} -p app-server >"${APP_LOG}" 2>&1 &
APP_PID=$!

for _ in $(seq 1 60); do
  if rg -q "auto settlement loop enabled \(batch payout mode\)" "${APP_LOG}"; then
    break
  fi
  sleep 0.25
done

if ! rg -q "auto settlement loop enabled \(batch payout mode\)" "${APP_LOG}"; then
  echo "error: app-server did not enter batch payout mode" >&2
  echo "---- app-server log ----" >&2
  cat "${APP_LOG}" >&2
  exit 1
fi

for _ in $(seq 1 30); do
  if curl -sS "http://127.0.0.1:9100/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done
if ! curl -sS "http://127.0.0.1:9100/health" >/dev/null 2>&1; then
  echo "error: ops health endpoint not ready at 127.0.0.1:9100" >&2
  echo "---- app-server log ----" >&2
  cat "${APP_LOG}" >&2
  exit 1
fi

echo
echo "Smoke batch-settlement E2E passed."
echo "  batch_contract=${BATCH_CONTRACT_ADDRESS}"
