use ledger_store::{LedgerDirection, LedgerEntryInsert, LedgerRepository, LedgerStoreError};
use poker_domain::{Chips, HandId, RoomId, SeatId, TraceId};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum SettlementError {
    #[error("ledger error: {0}")]
    Ledger(#[from] LedgerStoreError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettlementPayout {
    pub seat_id: SeatId,
    pub amount: Chips,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettlementPlan {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub rake_amount: Chips,
    pub payouts: Vec<SettlementPayout>,
}

impl SettlementPlan {
    pub fn payout_total(&self) -> Result<Chips, poker_domain::MoneyError> {
        self.payouts
            .iter()
            .try_fold(Chips::ZERO, |acc, p| acc.checked_add(p.amount))
    }
}

#[derive(Debug)]
pub struct SettlementService<L> {
    ledger_repo: L,
}

impl<L> SettlementService<L>
where
    L: LedgerRepository,
{
    #[must_use]
    pub fn new(ledger_repo: L) -> Self {
        Self { ledger_repo }
    }

    pub async fn record_plan(
        &self,
        plan: &SettlementPlan,
        trace_id: TraceId,
    ) -> Result<(), SettlementError> {
        let mut entries = Vec::with_capacity(plan.payouts.len() + 1);

        if plan.rake_amount != Chips::ZERO {
            entries.push(LedgerEntryInsert {
                entry_type: "rake_accrual".to_string(),
                room_id: Some(plan.room_id),
                hand_id: Some(plan.hand_id),
                seat_id: None,
                agent_id: None,
                asset_type: "CFX".to_string(),
                amount: plan.rake_amount,
                direction: LedgerDirection::Credit,
                account_scope: "room_rake".to_string(),
                related_tx_hash: None,
                related_event_id: None,
                related_attempt_id: None,
                remark: Some("rake accrual from settlement plan".to_string()),
                trace_id,
            });
        }

        for payout in &plan.payouts {
            entries.push(LedgerEntryInsert {
                entry_type: "settlement_payout".to_string(),
                room_id: Some(plan.room_id),
                hand_id: Some(plan.hand_id),
                seat_id: Some(payout.seat_id),
                agent_id: None,
                asset_type: "CFX".to_string(),
                amount: payout.amount,
                direction: LedgerDirection::Debit,
                account_scope: "room_pot".to_string(),
                related_tx_hash: None,
                related_event_id: None,
                related_attempt_id: None,
                remark: Some("planned payout".to_string()),
                trace_id,
            });
        }

        self.ledger_repo.insert_entries(&entries).await?;
        info!(entries = entries.len(), "settlement plan recorded");
        Ok(())
    }
}
