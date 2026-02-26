use std::sync::Arc;

use async_trait::async_trait;
use audit_store::{AuditRepository, BehaviorEventRecord};

use crate::settlement_orchestrator::SettlementAuditSink;

#[derive(Clone)]
pub struct AuditSettlementBehaviorSink<R: AuditRepository + ?Sized + 'static> {
    repo: Arc<R>,
}

impl<R: AuditRepository + ?Sized + 'static> AuditSettlementBehaviorSink<R> {
    #[must_use]
    pub fn new(repo: Arc<R>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl<R: AuditRepository + ?Sized + 'static> SettlementAuditSink for AuditSettlementBehaviorSink<R> {
    async fn emit(&self, event: BehaviorEventRecord) -> Result<(), String> {
        self.repo
            .insert_behavior_event(&event)
            .await
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audit_store::InMemoryAuditRepository;
    use chrono::Utc;
    use poker_domain::{RoomId, TraceId};

    #[tokio::test]
    async fn forwards_behavior_event_to_audit_repo() {
        let repo = Arc::new(InMemoryAuditRepository::default());
        let sink = AuditSettlementBehaviorSink::new(repo.clone());
        sink.emit(BehaviorEventRecord {
            behavior_event_id: "evt1".to_string(),
            event_kind: "settlement_payout_submitted".to_string(),
            event_source: "settlement_orchestrator".to_string(),
            room_id: Some(RoomId::new()),
            hand_id: None,
            seat_id: Some(1),
            action_seq: None,
            related_attempt_id: None,
            related_tx_hash: Some("0xtx".to_string()),
            severity: "info".to_string(),
            payload_json: serde_json::json!({"tx_hash":"0xtx"}),
            occurred_at: Utc::now(),
            trace_id: TraceId::new(),
        })
        .await
        .expect("emit");

        let events = repo.behavior_events.lock().expect("lock");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_kind, "settlement_payout_submitted");
    }
}
