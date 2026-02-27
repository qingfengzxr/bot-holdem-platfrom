#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
CONTRACT_DIR="${ROOT_DIR}/contracts"

if ! command -v forge >/dev/null 2>&1; then
  echo "error: forge not found in PATH (install Foundry first)." >&2
  exit 1
fi

if [[ -z "${DEPLOYER_PRIVATE_KEY:-}" ]]; then
  echo "error: DEPLOYER_PRIVATE_KEY is required." >&2
  exit 1
fi

if [[ -z "${RPC_URL:-}" ]]; then
  echo "error: RPC_URL is required." >&2
  exit 1
fi

if [[ ! -f "${CONTRACT_DIR}/lib/forge-std/src/Script.sol" ]]; then
  echo "==> Installing forge-std"
  if forge install --help 2>/dev/null | rg -q -- "--no-commit"; then
    (cd "${CONTRACT_DIR}" && forge install foundry-rs/forge-std --no-commit)
  else
    (cd "${CONTRACT_DIR}" && forge install foundry-rs/forge-std)
  fi
fi

echo "==> Building contract"
(cd "${CONTRACT_DIR}" && forge build)

echo "==> Deploying BatchSettlement"
OUTPUT="$(cd "${CONTRACT_DIR}" && forge script script/DeployBatchSettlement.s.sol:DeployBatchSettlement --rpc-url "${RPC_URL}" --broadcast --private-key "${DEPLOYER_PRIVATE_KEY}" -vvvv)"
echo "${OUTPUT}"

ADDR="$(
  echo "${OUTPUT}" \
    | rg -o "0x[a-fA-F0-9]{40}" \
    | tail -n1
)"
if [[ -z "${ADDR}" ]]; then
  echo "error: failed to parse deployed address from forge output." >&2
  exit 1
fi

echo
echo "Batch settlement contract deployed:"
echo "  BATCH_SETTLEMENT_CONTRACT=${ADDR}"
