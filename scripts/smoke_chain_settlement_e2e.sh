#!/usr/bin/env bash
set -euo pipefail

# One-click chain-settlement smoke validation.
# Focuses on settlement orchestration and auto-settlement flow tests.
#
# Usage:
#   scripts/smoke_chain_settlement_e2e.sh
#   scripts/smoke_chain_settlement_e2e.sh --online

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

echo "==> chain-settlement smoke: direct payout confirmed path"
cargo test ${OFFLINE_FLAG} -p app-server orchestrates_direct_payouts_and_closes_when_confirmed -- --nocapture

echo "==> chain-settlement smoke: direct payout failed receipt/manual review"
cargo test ${OFFLINE_FLAG} -p app-server direct_payouts_failed_receipt_does_not_close_hand_and_updates_record_status -- --nocapture

echo "==> chain-settlement smoke: settlement alert emission"
cargo test ${OFFLINE_FLAG} -p app-server direct_payout_failures_emit_settlement_alerts -- --nocapture

echo "==> chain-settlement smoke: settlement behavior audit emission"
cargo test ${OFFLINE_FLAG} -p app-server direct_payout_flow_emits_settlement_behavior_audits -- --nocapture

echo "==> chain-settlement smoke: data-layer consistency"
cargo test ${OFFLINE_FLAG} -p app-server direct_payout_failure_persists_manual_review_and_audit_behavior_chain -- --nocapture

echo "==> chain-settlement smoke: auto-settlement showdown input source"
cargo test ${OFFLINE_FLAG} -p app-server auto_settlement_accepts_showdown_input_from_admin_repo_and_uses_bound_addresses -- --nocapture

echo
echo "Smoke chain-settlement E2E passed."
