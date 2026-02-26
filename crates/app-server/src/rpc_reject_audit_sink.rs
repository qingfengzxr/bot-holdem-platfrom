use std::sync::Arc;

use async_trait::async_trait;
use audit_store::{AuditRepository, BehaviorEventRecord};
use chrono::Utc;
use poker_domain::TraceId;
use rpc_gateway::{RequestRejectAuditEvent, RequestRejectAuditSink, RequestRejectReason};

#[derive(Clone)]
pub struct AuditRpcRejectSink<R: AuditRepository + ?Sized + 'static> {
    repo: Arc<R>,
}

impl<R: AuditRepository + ?Sized + 'static> AuditRpcRejectSink<R> {
    #[must_use]
    pub fn new(repo: Arc<R>) -> Self {
        Self { repo }
    }

    fn map_reason(reason: RequestRejectReason) -> (&'static str, &'static str) {
        match reason {
            RequestRejectReason::RequestExpiredOrInvalidWindow => ("request_rejected", "warn"),
            RequestRejectReason::MissingOrInvalidSignature => {
                ("request_rejected_signature", "warn")
            }
            RequestRejectReason::InvalidKeyBindingProof => ("request_rejected_key_binding", "warn"),
            RequestRejectReason::ReplayRejected => ("request_rejected_replay", "warn"),
        }
    }
}

#[async_trait]
impl<R: AuditRepository + ?Sized + 'static> RequestRejectAuditSink for AuditRpcRejectSink<R> {
    async fn on_request_rejected(&self, event: RequestRejectAuditEvent) -> Result<(), String> {
        let (event_kind, severity) = Self::map_reason(event.reason);
        self.repo
            .insert_behavior_event(&BehaviorEventRecord {
                behavior_event_id: uuid::Uuid::now_v7().to_string(),
                event_kind: event_kind.to_string(),
                event_source: "rpc_gateway".to_string(),
                room_id: event.room_id,
                hand_id: event.hand_id,
                seat_id: event.seat_id,
                action_seq: event.action_seq,
                related_attempt_id: event.request_id.map(|r| r.0.to_string()),
                related_tx_hash: None,
                severity: severity.to_string(),
                payload_json: serde_json::json!({
                    "method": event.method,
                    "reason": format!("{:?}", event.reason),
                    "request_id": event.request_id.map(|r| r.0.to_string()),
                    "request_nonce": event.request_nonce,
                    "detail": event.detail,
                }),
                occurred_at: Utc::now(),
                trace_id: event.trace_id,
            })
            .await
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audit_store::InMemoryAuditRepository;
    use poker_domain::{ActionSeq, HandId, RequestId, RoomId};

    #[tokio::test]
    async fn writes_rpc_reject_event_to_behavior_audit() {
        let repo = Arc::new(InMemoryAuditRepository::default());
        let sink = AuditRpcRejectSink::new(repo.clone());
        let req_id = RequestId::new();

        sink.on_request_rejected(RequestRejectAuditEvent {
            method: "game.act".to_string(),
            reason: RequestRejectReason::RequestExpiredOrInvalidWindow,
            trace_id: TraceId::new(),
            request_id: Some(req_id),
            request_nonce: Some("n-1".to_string()),
            room_id: Some(RoomId::new()),
            hand_id: Some(HandId::new()),
            seat_id: Some(2),
            action_seq: Some(1 as ActionSeq),
            detail: Some("expired".to_string()),
        })
        .await
        .expect("write");

        let events = repo.behavior_events.lock().expect("lock");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_source, "rpc_gateway");
        assert_eq!(events[0].event_kind, "request_rejected");
        let req_id_str = req_id.0.to_string();
        assert_eq!(
            events[0].related_attempt_id.as_deref(),
            Some(req_id_str.as_str())
        );
    }

    #[tokio::test]
    async fn maps_replay_rejection_to_replay_event_kind() {
        let repo = Arc::new(InMemoryAuditRepository::default());
        let sink = AuditRpcRejectSink::new(repo.clone());

        sink.on_request_rejected(RequestRejectAuditEvent {
            method: "game.act".to_string(),
            reason: RequestRejectReason::ReplayRejected,
            trace_id: TraceId::new(),
            request_id: None,
            request_nonce: Some("dup-nonce".to_string()),
            room_id: None,
            hand_id: None,
            seat_id: None,
            action_seq: None,
            detail: Some("replay request detected".to_string()),
        })
        .await
        .expect("write");

        let events = repo.behavior_events.lock().expect("lock");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_kind, "request_rejected_replay");
        assert_eq!(events[0].event_source, "rpc_gateway");
    }
}
