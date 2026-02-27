#!/usr/bin/env bash
set -euo pipefail

# Deploy BatchSettlement to local Anvil default account.
# Requires anvil running at 127.0.0.1:8545.

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

export RPC_URL="${RPC_URL:-http://127.0.0.1:8545}"
export DEPLOYER_PRIVATE_KEY="${DEPLOYER_PRIVATE_KEY:-0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80}"

"${ROOT_DIR}/scripts/deploy_batch_settlement.sh"

