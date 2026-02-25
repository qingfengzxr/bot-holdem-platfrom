use async_trait::async_trait;
use chrono::{DateTime, Utc};
use poker_domain::{AgentId, HandEvent, HandId, RoomId, SeatId, SessionId, TraceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuditStoreError {
    #[error("not implemented")]
    NotImplemented,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionAttemptRecord {
    pub attempt_id: String,
    pub request_id: String,
    pub request_nonce: String,
    pub agent_id: Option<AgentId>,
    pub session_id: Option<SessionId>,
    pub room_id: Option<RoomId>,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
    pub action_seq: Option<u32>,
    pub method: String,
    pub action_type: Option<String>,
    pub request_payload_json: Value,
    pub signature_verify_result: String,
    pub replay_check_result: String,
    pub idempotency_check_result: String,
    pub validation_result: String,
    pub router_result: String,
    pub business_result_code: Option<String>,
    pub error_detail: Option<String>,
    pub received_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub trace_id: TraceId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorEventRecord {
    pub behavior_event_id: String,
    pub event_kind: String,
    pub event_source: String,
    pub room_id: Option<RoomId>,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
    pub action_seq: Option<u32>,
    pub related_attempt_id: Option<String>,
    pub related_tx_hash: Option<String>,
    pub severity: String,
    pub payload_json: Value,
    pub occurred_at: DateTime<Utc>,
    pub trace_id: TraceId,
}

#[async_trait]
pub trait AuditRepository: Send + Sync {
    async fn insert_action_attempt(
        &self,
        record: &ActionAttemptRecord,
    ) -> Result<(), AuditStoreError>;

    async fn insert_behavior_event(
        &self,
        record: &BehaviorEventRecord,
    ) -> Result<(), AuditStoreError>;

    async fn append_hand_events(&self, events: &[HandEvent]) -> Result<(), AuditStoreError>;
}

#[derive(Debug, Default)]
pub struct NoopAuditRepository;

#[async_trait]
impl AuditRepository for NoopAuditRepository {
    async fn insert_action_attempt(
        &self,
        _record: &ActionAttemptRecord,
    ) -> Result<(), AuditStoreError> {
        Ok(())
    }

    async fn insert_behavior_event(
        &self,
        _record: &BehaviorEventRecord,
    ) -> Result<(), AuditStoreError> {
        Ok(())
    }

    async fn append_hand_events(&self, _events: &[HandEvent]) -> Result<(), AuditStoreError> {
        Ok(())
    }
}
