use async_trait::async_trait;
use audit_store::{AuditRepository, BehaviorEventRecord};
use chain_watcher::{ChainWatcherError, TxVerificationCallback, VerificationCallbackSink};
use chrono::Utc;
use ledger_store::{ChainTxRepository, ExceptionCreditUpsert, TxVerificationUpsert};
use table_service::ChainTxVerifiedCallback;
use tracing::warn;

use crate::room_service_port::AppRoomService;

#[derive(Debug, Clone)]
pub struct ChainVerificationAlert {
    pub room_id: poker_domain::RoomId,
    pub tx_hash: String,
    pub status: String,
    pub reason: Option<String>,
}

#[async_trait]
pub trait ChainAlertSink: Send + Sync {
    async fn emit(&self, alert: ChainVerificationAlert) -> Result<(), String>;
}

#[derive(Debug, Default)]
pub struct NoopChainAlertSink;

#[async_trait]
impl ChainAlertSink for NoopChainAlertSink {
    async fn emit(&self, _alert: ChainVerificationAlert) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct AppChainCallbackSink {
    room_service: AppRoomService,
    chain_tx_repo: std::sync::Arc<dyn ChainTxRepository>,
    alert_sink: std::sync::Arc<dyn ChainAlertSink>,
    audit_repo: Option<std::sync::Arc<dyn AuditRepository>>,
}

impl AppChainCallbackSink {
    #[must_use]
    pub fn new(
        room_service: AppRoomService,
        chain_tx_repo: std::sync::Arc<dyn ChainTxRepository>,
    ) -> Self {
        Self {
            room_service,
            chain_tx_repo,
            alert_sink: std::sync::Arc::new(NoopChainAlertSink),
            audit_repo: None,
        }
    }

    #[must_use]
    pub fn with_alert_sink(mut self, alert_sink: std::sync::Arc<dyn ChainAlertSink>) -> Self {
        self.alert_sink = alert_sink;
        self
    }

    #[must_use]
    pub fn with_audit_repo(mut self, audit_repo: std::sync::Arc<dyn AuditRepository>) -> Self {
        self.audit_repo = Some(audit_repo);
        self
    }
}

#[async_trait]
impl VerificationCallbackSink for AppChainCallbackSink {
    async fn on_verification_result(
        &self,
        callback: TxVerificationCallback,
    ) -> Result<(), ChainWatcherError> {
        self.chain_tx_repo
            .upsert_tx_verification(&TxVerificationUpsert {
                tx_hash: callback.tx_hash.clone(),
                room_id: callback.room_id,
                hand_id: callback.hand_id,
                seat_id: callback.seat_id,
                action_seq: callback.action_seq,
                status: format!("{:?}", callback.status).to_lowercase(),
                confirmations: callback.confirmations,
                failure_reason: callback.failure_reason.clone(),
                observed_from: None,
                observed_to: None,
                observed_amount: None,
                verified_at: Utc::now(),
                trace_id: poker_domain::TraceId::new(),
            })
            .await
            .map_err(|e| ChainWatcherError::CallbackSink(e.to_string()))?;

        if let Some(audit_repo) = &self.audit_repo {
            let status_str = format!("{:?}", callback.status).to_lowercase();
            let severity = match callback.status {
                chain_watcher::VerificationStatus::Matched => "info",
                chain_watcher::VerificationStatus::Pending => "warn",
                chain_watcher::VerificationStatus::Late
                | chain_watcher::VerificationStatus::Unmatched
                | chain_watcher::VerificationStatus::Failed => "error",
            }
            .to_string();
            audit_repo
                .insert_behavior_event(&BehaviorEventRecord {
                    behavior_event_id: uuid::Uuid::now_v7().to_string(),
                    event_kind: "chain_tx_verification_callback".to_string(),
                    event_source: "chain_watcher".to_string(),
                    room_id: Some(callback.room_id),
                    hand_id: callback.hand_id,
                    seat_id: callback.seat_id,
                    action_seq: callback.action_seq,
                    related_attempt_id: None,
                    related_tx_hash: Some(callback.tx_hash.clone()),
                    severity,
                    payload_json: serde_json::json!({
                        "tx_hash": callback.tx_hash,
                        "status": status_str,
                        "confirmations": callback.confirmations,
                        "failure_reason": callback.failure_reason,
                    }),
                    occurred_at: Utc::now(),
                    trace_id: poker_domain::TraceId::new(),
                })
                .await
                .map_err(|e| ChainWatcherError::CallbackSink(e.to_string()))?;
        }

        if matches!(
            callback.status,
            chain_watcher::VerificationStatus::Late
                | chain_watcher::VerificationStatus::Unmatched
                | chain_watcher::VerificationStatus::Failed
        ) {
            let alert = ChainVerificationAlert {
                room_id: callback.room_id,
                tx_hash: callback.tx_hash.clone(),
                status: format!("{:?}", callback.status).to_lowercase(),
                reason: callback.failure_reason.clone(),
            };
            if let Err(err) = self.alert_sink.emit(alert.clone()).await {
                warn!(error = %err, tx_hash = %alert.tx_hash, "failed to emit chain verification alert");
            }
        }

        if matches!(
            callback.status,
            chain_watcher::VerificationStatus::Late | chain_watcher::VerificationStatus::Unmatched
        ) {
            self.chain_tx_repo
                .upsert_exception_credit(&ExceptionCreditUpsert {
                    room_id: callback.room_id,
                    seat_id: callback.seat_id,
                    tx_hash: callback.tx_hash.clone(),
                    credit_type: format!("{:?}", callback.status).to_lowercase(),
                    amount: poker_domain::Chips::ZERO,
                    status: "open".to_string(),
                    reason: callback
                        .failure_reason
                        .clone()
                        .unwrap_or_else(|| "chain_callback".to_string()),
                    updated_at: Utc::now(),
                    trace_id: poker_domain::TraceId::new(),
                })
                .await
                .map_err(|e| ChainWatcherError::CallbackSink(e.to_string()))?;
        }

        let room = self
            .room_service
            .ensure_room(callback.room_id)
            .map_err(|e| ChainWatcherError::CallbackSink(e.to_string()))?;

        room.chain_tx_verified(ChainTxVerifiedCallback {
            tx_hash: callback.tx_hash,
            status: format!("{:?}", callback.status).to_lowercase(),
            confirmations: callback.confirmations,
            hand_id: callback.hand_id,
            seat_id: callback.seat_id,
            action_seq: callback.action_seq,
            failure_reason: callback.failure_reason,
        })
        .await
        .map_err(ChainWatcherError::CallbackSink)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use audit_store::InMemoryAuditRepository;
    use chain_watcher::{TxVerificationCallback, VerificationStatus};
    use ledger_store::InMemoryChainTxRepository;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct InMemoryChainAlertSink {
        alerts: Mutex<Vec<ChainVerificationAlert>>,
    }

    #[async_trait]
    impl ChainAlertSink for InMemoryChainAlertSink {
        async fn emit(&self, alert: ChainVerificationAlert) -> Result<(), String> {
            self.alerts
                .lock()
                .map_err(|_| "lock poisoned".to_string())?
                .push(alert);
            Ok(())
        }
    }

    #[tokio::test]
    async fn callback_sink_persists_verification_and_exception_credit() {
        let room_service = AppRoomService::new();
        let repo = std::sync::Arc::new(InMemoryChainTxRepository::new());
        let audit_repo = Arc::new(InMemoryAuditRepository::default());
        let alert_sink = Arc::new(InMemoryChainAlertSink::default());
        let sink = AppChainCallbackSink::new(room_service, repo.clone())
            .with_alert_sink(alert_sink.clone())
            .with_audit_repo(audit_repo.clone());

        sink.on_verification_result(TxVerificationCallback {
            room_id: poker_domain::RoomId::new(),
            hand_id: None,
            seat_id: Some(1),
            action_seq: Some(1),
            tx_hash: "0xlate".to_string(),
            status: VerificationStatus::Unmatched,
            confirmations: 3,
            failure_reason: Some("value_mismatch".to_string()),
        })
        .await
        .expect("callback");

        assert_eq!(repo.tx_verifications_len(), 1);
        assert_eq!(repo.exception_credits_len(), 1);
        assert_eq!(alert_sink.alerts.lock().expect("lock").len(), 1);
        let behavior_events = audit_repo.behavior_events.lock().expect("lock");
        assert_eq!(behavior_events.len(), 1);
        assert_eq!(
            behavior_events[0].event_kind,
            "chain_tx_verification_callback"
        );
    }

    #[tokio::test]
    async fn callback_sink_matched_persists_verification_only() {
        let room_service = AppRoomService::new();
        let repo = std::sync::Arc::new(InMemoryChainTxRepository::new());
        let audit_repo = Arc::new(InMemoryAuditRepository::default());
        let sink = AppChainCallbackSink::new(room_service, repo.clone())
            .with_audit_repo(audit_repo.clone());

        sink.on_verification_result(TxVerificationCallback {
            room_id: poker_domain::RoomId::new(),
            hand_id: Some(poker_domain::HandId::new()),
            seat_id: Some(1),
            action_seq: Some(1),
            tx_hash: "0xok".to_string(),
            status: VerificationStatus::Matched,
            confirmations: 3,
            failure_reason: None,
        })
        .await
        .expect("callback");

        assert_eq!(repo.tx_verifications_len(), 1);
        assert_eq!(repo.exception_credits_len(), 0);
        assert_eq!(audit_repo.behavior_events.lock().expect("lock").len(), 1);
    }

    #[tokio::test]
    async fn callback_sink_failed_emits_alert_without_exception_credit() {
        let room_service = AppRoomService::new();
        let repo = std::sync::Arc::new(InMemoryChainTxRepository::new());
        let alert_sink = Arc::new(InMemoryChainAlertSink::default());
        let sink = AppChainCallbackSink::new(room_service, repo.clone())
            .with_alert_sink(alert_sink.clone());

        sink.on_verification_result(TxVerificationCallback {
            room_id: poker_domain::RoomId::new(),
            hand_id: Some(poker_domain::HandId::new()),
            seat_id: Some(1),
            action_seq: Some(2),
            tx_hash: "0xfail".to_string(),
            status: VerificationStatus::Failed,
            confirmations: 0,
            failure_reason: Some("tx_failed".to_string()),
        })
        .await
        .expect("callback");

        assert_eq!(repo.tx_verifications_len(), 1);
        assert_eq!(repo.exception_credits_len(), 0);
        let alerts = alert_sink.alerts.lock().expect("lock");
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].status, "failed");
    }
}
