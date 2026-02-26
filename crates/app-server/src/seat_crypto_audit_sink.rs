use std::sync::Arc;

use audit_store::{AuditRepository, BehaviorEventRecord};
use chrono::Utc;
use seat_crypto::{HoleCardsDeliveryAuditEvent, HoleCardsDeliveryAuditSink};
use tokio::runtime::{Builder as TokioRuntimeBuilder, Handle as TokioHandle};

#[derive(Clone)]
pub struct AuditSeatCryptoDeliverySink<R: AuditRepository + ?Sized + 'static> {
    repo: Arc<R>,
}

impl<R: AuditRepository + ?Sized + 'static> AuditSeatCryptoDeliverySink<R> {
    #[must_use]
    pub fn new(repo: Arc<R>) -> Self {
        Self { repo }
    }
}

impl<R: AuditRepository + ?Sized + 'static> HoleCardsDeliveryAuditSink
    for AuditSeatCryptoDeliverySink<R>
{
    fn on_hole_cards_encrypted(&self, event: &HoleCardsDeliveryAuditEvent) -> Result<(), String> {
        let record = BehaviorEventRecord {
            behavior_event_id: uuid::Uuid::now_v7().to_string(),
            event_kind: "hole_cards_encrypted_delivery".to_string(),
            event_source: "seat_crypto".to_string(),
            room_id: Some(event.room_id),
            hand_id: Some(event.hand_id),
            seat_id: Some(event.seat_id),
            action_seq: None,
            related_attempt_id: None,
            related_tx_hash: None,
            severity: "info".to_string(),
            payload_json: serde_json::json!({
                "event_seq": event.event_seq,
                "key_id": event.key_id,
                "algorithm": event.algorithm,
                "ciphertext_len": event.ciphertext_len,
            }),
            occurred_at: Utc::now(),
            trace_id: poker_domain::TraceId::new(),
        };

        if let Ok(handle) = TokioHandle::try_current() {
            tokio::task::block_in_place(|| {
                handle.block_on(self.repo.insert_behavior_event(&record))
            })
            .map_err(|e| e.to_string())
        } else {
            let rt = TokioRuntimeBuilder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            rt.block_on(self.repo.insert_behavior_event(&record))
                .map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audit_store::InMemoryAuditRepository;
    use poker_domain::{HandId, RoomId};

    #[test]
    fn writes_seat_crypto_delivery_audit_event() {
        let repo = Arc::new(InMemoryAuditRepository::default());
        let sink = AuditSeatCryptoDeliverySink::new(repo.clone());
        sink.on_hole_cards_encrypted(&HoleCardsDeliveryAuditEvent {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            seat_id: 2,
            event_seq: 7,
            key_id: "seat-k1".to_string(),
            algorithm: "x25519+hkdf-sha256+chacha20poly1305".to_string(),
            ciphertext_len: 128,
        })
        .expect("write");

        let events = repo.behavior_events.lock().expect("lock");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_kind, "hole_cards_encrypted_delivery");
        assert_eq!(events[0].event_source, "seat_crypto");
        assert_eq!(events[0].seat_id, Some(2));
    }
}
