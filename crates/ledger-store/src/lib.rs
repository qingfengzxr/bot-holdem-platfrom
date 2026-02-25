use async_trait::async_trait;
use poker_domain::{AgentId, Chips, HandId, RoomId, SeatId, TraceId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LedgerStoreError {
    #[error("not implemented")]
    NotImplemented,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LedgerDirection {
    Debit,
    Credit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntryInsert {
    pub entry_type: String,
    pub room_id: Option<RoomId>,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
    pub agent_id: Option<AgentId>,
    pub asset_type: String,
    pub amount: Chips,
    pub direction: LedgerDirection,
    pub account_scope: String,
    pub related_tx_hash: Option<String>,
    pub related_event_id: Option<String>,
    pub related_attempt_id: Option<String>,
    pub remark: Option<String>,
    pub trace_id: TraceId,
}

#[async_trait]
pub trait LedgerRepository: Send + Sync {
    async fn insert_entries(&self, entries: &[LedgerEntryInsert]) -> Result<(), LedgerStoreError>;
}

#[derive(Debug, Default)]
pub struct NoopLedgerRepository;

#[async_trait]
impl LedgerRepository for NoopLedgerRepository {
    async fn insert_entries(&self, _entries: &[LedgerEntryInsert]) -> Result<(), LedgerStoreError> {
        Ok(())
    }
}
