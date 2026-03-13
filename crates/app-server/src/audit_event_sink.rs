use std::sync::Arc;

use audit_store::{AuditRepository, BehaviorEventRecord, OutboxEventRecord};
use chrono::Utc;
use poker_domain::{HandSnapshot, HandStatus, Street};
use serde_json::to_value;
use sqlx::PgPool;
use table_service::{RoomBehaviorEvent, RoomEventSink, SeatPrivateEvent};
use uuid::Uuid;

#[derive(Clone)]
pub struct AuditRoomEventSink<R: AuditRepository + ?Sized + 'static> {
    repo: Arc<R>,
    db_pool: Option<PgPool>,
}

impl<R: AuditRepository + ?Sized + 'static> AuditRoomEventSink<R> {
    #[must_use]
    pub fn new(repo: Arc<R>) -> Self {
        Self {
            repo,
            db_pool: None,
        }
    }

    #[must_use]
    pub fn with_db_pool(mut self, db_pool: Option<PgPool>) -> Self {
        self.db_pool = db_pool;
        self
    }

    async fn upsert_hand_snapshot_row(
        pool: &PgPool,
        snapshot: &HandSnapshot,
    ) -> Result<(), String> {
        let hand_status = match snapshot.status {
            HandStatus::Created => "Created",
            HandStatus::Running => "Running",
            HandStatus::Showdown => "Showdown",
            HandStatus::Settling => "Settling",
            HandStatus::Settled => "Settled",
            HandStatus::Aborted => "Aborted",
        };
        let street = match snapshot.street {
            Street::Preflop => "Preflop",
            Street::Flop => "Flop",
            Street::Turn => "Turn",
            Street::River => "River",
            Street::Showdown => "Showdown",
        };
        let action_seq_next = i32::try_from(snapshot.next_action_seq).unwrap_or(i32::MAX);
        let hand_no = i64::try_from(snapshot.hand_no).unwrap_or(i64::MAX);
        let acting_seat_id = snapshot.acting_seat_id.map(i16::from);
        let pot_total = snapshot.pot_total.as_u128().to_string();

        sqlx::query(
            r#"
            INSERT INTO hands (
                hand_id, room_id, hand_no, button_seat_id, hand_status, street, acting_seat_id,
                action_seq_next, pot_total, rake_accrued, incoming_total, settlement_pending_total,
                ended_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7,
                $8, CAST($9 AS NUMERIC), CAST($10 AS NUMERIC), CAST($11 AS NUMERIC), CAST($12 AS NUMERIC),
                CASE WHEN $13 THEN NOW() ELSE NULL END
            )
            ON CONFLICT (hand_id) DO UPDATE SET
                room_id = EXCLUDED.room_id,
                hand_no = EXCLUDED.hand_no,
                hand_status = EXCLUDED.hand_status,
                street = EXCLUDED.street,
                acting_seat_id = EXCLUDED.acting_seat_id,
                action_seq_next = EXCLUDED.action_seq_next,
                pot_total = EXCLUDED.pot_total,
                ended_at = CASE
                    WHEN EXCLUDED.hand_status IN ('Settled', 'Aborted') THEN COALESCE(hands.ended_at, NOW())
                    ELSE NULL
                END
            "#,
        )
        .bind(snapshot.hand_id.0)
        .bind(snapshot.room_id.0)
        .bind(hand_no)
        .bind(0_i16)
        .bind(hand_status)
        .bind(street)
        .bind(acting_seat_id)
        .bind(action_seq_next)
        .bind(pot_total)
        .bind("0")
        .bind("0")
        .bind("0")
        .bind(matches!(snapshot.status, HandStatus::Settled | HandStatus::Aborted))
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
        tracing::debug!(
            room_id = %snapshot.room_id.0,
            hand_id = %snapshot.hand_id.0,
            action_seq = snapshot.next_action_seq,
            status = ?snapshot.status,
            street = ?snapshot.street,
            "hand snapshot persisted"
        );
        Ok(())
    }
}

impl<R: AuditRepository + ?Sized + 'static> RoomEventSink for AuditRoomEventSink<R> {
    fn upsert_hand_snapshot(&self, snapshot: &HandSnapshot) -> Result<(), String> {
        let Some(pool) = self.db_pool.clone() else {
            tracing::debug!(
                room_id = %snapshot.room_id.0,
                hand_id = %snapshot.hand_id.0,
                "skip hand snapshot persistence: db pool disabled"
            );
            return Ok(());
        };
        let snapshot = snapshot.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(Self::upsert_hand_snapshot_row(&pool, &snapshot))
        })
    }

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

    fn append_private_events(&self, events: &[SeatPrivateEvent]) -> Result<(), String> {
        let repo = self.repo.clone();
        let events_vec = events.to_vec();
        tokio::spawn(async move {
            for event in &events_vec {
                let now = Utc::now();
                let record = OutboxEventRecord {
                    outbox_event_id: Uuid::now_v7().to_string(),
                    topic: "seat.events".to_string(),
                    partition_key: format!("{}:{}", event.room_id.0, event.seat_id),
                    payload_json: serde_json::json!({
                        "room_id": event.room_id,
                        "hand_id": event.hand_id,
                        "seat_id": event.seat_id,
                        "event_name": event.event_name,
                        "payload": event.payload,
                    }),
                    status: "pending".to_string(),
                    attempts: 0,
                    available_at: now,
                    delivered_at: None,
                    created_at: now,
                };
                if let Err(err) = repo.insert_outbox_event(&record).await {
                    tracing::warn!(error = %err, "insert seat private outbox event failed");
                }
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
