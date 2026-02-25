use poker_domain::{Chips, HandId, RoomId, SeatId};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

#[derive(Debug, Error)]
pub enum ChainWatcherError {
    #[error("unsupported chain rpc")]
    UnsupportedRpc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationStatus {
    Pending,
    Matched,
    Late,
    Unmatched,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxVerificationInput {
    pub room_id: RoomId,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
    pub action_seq: Option<u32>,
    pub tx_hash: String,
    pub expected_to: String,
    pub expected_from: Option<String>,
    pub expected_amount: Option<Chips>,
    pub min_confirmations: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxVerificationResult {
    pub tx_hash: String,
    pub status: VerificationStatus,
    pub confirmations: u64,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Default)]
pub struct ChainWatcher;

impl ChainWatcher {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    pub async fn verify_tx(
        &self,
        input: TxVerificationInput,
    ) -> Result<TxVerificationResult, ChainWatcherError> {
        debug!(tx_hash = %input.tx_hash, "chain watcher placeholder verify");
        Ok(TxVerificationResult {
            tx_hash: input.tx_hash,
            status: VerificationStatus::Pending,
            confirmations: 0,
            failure_reason: None,
        })
    }
}
