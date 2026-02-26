use std::sync::Arc;

use audit_store::{AuditRepository, BehaviorEventRecord, OutboxEventRecord};
use chrono::Utc;
use serde_json::to_value;
use table_service::{RoomBehaviorEvent, RoomEventSink};
use uuid::Uuid;

#[derive(Clone)]
pub struct AuditRoomEventSink<R: AuditRepository + ?Sized + 'static> {
    repo: Arc<R>,
}

impl<R: AuditRepository + ?Sized + 'static> AuditRoomEventSink<R> {
    #[must_use]
    pub fn new(repo: Arc<R>) -> Self {
        Self { repo }
    }
}

impl<R: AuditRepository + ?Sized + 'static> RoomEventSink for AuditRoomEventSink<R> {
    fn append_hand_events(&self, events: &[poker_domain::HandEvent]) -> Result<(), String> {
        let repo = self.repo.clone();
        let events_vec = events.to_vec();
        tokio::spawn(async move {
            if let Err(err) = repo.append_hand_events(&events_vec).await {
                tracing::warn!(error = %err, count = events_vec.len(), "append_hand_events failed");
                return;
            }

            for event in &events_vec {
                let payload_json = match to_value(event) {
                    Ok(v) => v,
                    Err(err) => {
                        tracing::warn!(error = %err, "serialize hand event for outbox failed");
                        continue;
                    }
                };
                let now = Utc::now();
                let record = OutboxEventRecord {
                    outbox_event_id: Uuid::now_v7().to_string(),
                    topic: "hand.events".to_string(),
                    partition_key: event.room_id.0.to_string(),
                    payload_json,
                    status: "pending".to_string(),
                    attempts: 0,
                    available_at: now,
                    delivered_at: None,
                    created_at: now,
                };
                if let Err(err) = repo.insert_outbox_event(&record).await {
                    tracing::warn!(error = %err, "insert hand outbox event failed");
                }
            }
        });
        Ok(())
    }

    fn record_behavior_event(&self, event: &RoomBehaviorEvent) -> Result<(), String> {
        let repo = self.repo.clone();
        let event = event.clone();
        tokio::spawn(async move {
            let payload_json = match to_value(&event.kind) {
                Ok(v) => v,
                Err(err) => {
                    tracing::warn!(error = %err, "serialize behavior event failed");
                    return;
                }
            };

            let (event_kind, seat_id, action_seq, related_tx_hash) = match &event.kind {
                poker_domain::AuditBehaviorEventKind::SeatJoined { seat_id } => {
                    ("seat_joined".to_string(), Some(*seat_id), None, None)
                }
                poker_domain::AuditBehaviorEventKind::SeatLeft { seat_id } => {
                    ("seat_left".to_string(), Some(*seat_id), None, None)
                }
                poker_domain::AuditBehaviorEventKind::SeatAddressBound { seat_id } => {
                    ("seat_address_bound".to_string(), Some(*seat_id), None, None)
                }
                poker_domain::AuditBehaviorEventKind::SeatSessionKeysBound { seat_id, .. } => (
                    "seat_session_keys_bound".to_string(),
                    Some(*seat_id),
                    None,
                    None,
                ),
                poker_domain::AuditBehaviorEventKind::TurnTimeoutAutoFold {
                    seat_id,
                    action_seq,
                } => (
                    "turn_timeout_auto_fold".to_string(),
                    Some(*seat_id),
                    Some(*action_seq),
                    None,
                ),
                poker_domain::AuditBehaviorEventKind::ChainTxVerificationCallback {
                    seat_id,
                    action_seq,
                    tx_hash,
                    ..
                } => (
                    "chain_tx_verification_callback".to_string(),
                    *seat_id,
                    *action_seq,
                    Some(tx_hash.clone()),
                ),
            };

            let record = BehaviorEventRecord {
                behavior_event_id: Uuid::now_v7().to_string(),
                event_kind,
                event_source: "table_service".to_string(),
                room_id: Some(event.room_id),
                hand_id: None,
                seat_id,
                action_seq,
                related_attempt_id: None,
                related_tx_hash,
                severity: "info".to_string(),
                payload_json,
                occurred_at: Utc::now(),
                trace_id: event.trace_id,
            };
            if let Err(err) = repo.insert_behavior_event(&record).await {
                tracing::warn!(error = %err, "insert behavior event failed");
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audit_store::InMemoryAuditRepository;
    use chrono::Utc;
    use poker_domain::{HandEvent, HandEventKind, HandId, RoomId, TraceId};

    #[tokio::test]
    async fn append_hand_events_writes_hand_events_and_outbox() {
        let repo = Arc::new(InMemoryAuditRepository::default());
        let sink = AuditRoomEventSink::new(repo.clone());
        let event = HandEvent {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            event_seq: 1,
            trace_id: TraceId::new(),
            occurred_at: Utc::now(),
            kind: HandEventKind::HandStarted,
        };

        sink.append_hand_events(&[event]).expect("sink append");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert_eq!(repo.hand_events.lock().expect("hand events").len(), 1);
        assert_eq!(repo.outbox_events.lock().expect("outbox").len(), 1);
    }
}
