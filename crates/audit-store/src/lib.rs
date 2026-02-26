use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use poker_domain::{AgentId, HandEvent, HandId, RoomId, SeatId, SessionId, TraceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum AuditStoreError {
    #[error("not implemented")]
    NotImplemented,
    #[error("store lock poisoned")]
    LockPoisoned,
    #[error("database error: {0}")]
    Database(String),
    #[error("invalid uuid in field {field}: {value}")]
    InvalidUuid { field: &'static str, value: String },
    #[error("serialization error: {0}")]
    Serialization(String),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxEventRecord {
    pub outbox_event_id: String,
    pub topic: String,
    pub partition_key: String,
    pub payload_json: Value,
    pub status: String,
    pub attempts: u32,
    pub available_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PageRequest {
    pub limit: usize,
    pub offset: usize,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            limit: 100,
            offset: 0,
        }
    }
}

#[async_trait]
pub trait AuditReadRepository: Send + Sync {
    async fn get_action_attempt_by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<ActionAttemptRecord>, AuditStoreError>;

    async fn list_behavior_events(
        &self,
        room_id: Option<RoomId>,
        hand_id: Option<HandId>,
        page: PageRequest,
    ) -> Result<Vec<BehaviorEventRecord>, AuditStoreError>;

    async fn list_hand_events(
        &self,
        hand_id: HandId,
        page: PageRequest,
    ) -> Result<Vec<HandEvent>, AuditStoreError>;
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

    async fn insert_outbox_event(&self, record: &OutboxEventRecord) -> Result<(), AuditStoreError>;

    async fn fetch_pending_outbox_events(
        &self,
        limit: usize,
    ) -> Result<Vec<OutboxEventRecord>, AuditStoreError>;

    async fn mark_outbox_event_delivered(
        &self,
        outbox_event_id: &str,
        delivered_at: DateTime<Utc>,
    ) -> Result<(), AuditStoreError>;
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

    async fn insert_outbox_event(
        &self,
        _record: &OutboxEventRecord,
    ) -> Result<(), AuditStoreError> {
        Ok(())
    }

    async fn fetch_pending_outbox_events(
        &self,
        _limit: usize,
    ) -> Result<Vec<OutboxEventRecord>, AuditStoreError> {
        Ok(Vec::new())
    }

    async fn mark_outbox_event_delivered(
        &self,
        _outbox_event_id: &str,
        _delivered_at: DateTime<Utc>,
    ) -> Result<(), AuditStoreError> {
        Ok(())
    }
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryAuditRepository {
    pub action_attempts: Arc<Mutex<Vec<ActionAttemptRecord>>>,
    pub behavior_events: Arc<Mutex<Vec<BehaviorEventRecord>>>,
    pub hand_events: Arc<Mutex<Vec<HandEvent>>>,
    pub outbox_events: Arc<Mutex<Vec<OutboxEventRecord>>>,
}

#[derive(Debug, Clone)]
pub struct PostgresAuditRepository {
    pool: PgPool,
}

impl PostgresAuditRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

fn row_to_action_attempt_record(
    row: &sqlx::postgres::PgRow,
) -> Result<ActionAttemptRecord, AuditStoreError> {
    Ok(ActionAttemptRecord {
        attempt_id: row
            .try_get::<Uuid, _>("attempt_id")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .to_string(),
        request_id: row
            .try_get::<Uuid, _>("request_id")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .to_string(),
        request_nonce: row
            .try_get("request_nonce")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        agent_id: row
            .try_get::<Option<Uuid>, _>("agent_id")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .map(AgentId),
        session_id: row
            .try_get::<Option<Uuid>, _>("session_id")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .map(SessionId),
        room_id: row
            .try_get::<Option<Uuid>, _>("room_id")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .map(RoomId),
        hand_id: row
            .try_get::<Option<Uuid>, _>("hand_id")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .map(HandId),
        seat_id: row
            .try_get::<Option<i16>, _>("seat_id")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .and_then(|v| u8::try_from(v).ok()),
        action_seq: row
            .try_get::<Option<i32>, _>("action_seq")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .and_then(|v| u32::try_from(v).ok()),
        method: row
            .try_get("method")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        action_type: row
            .try_get("action_type")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        request_payload_json: row
            .try_get("request_payload_json")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        signature_verify_result: row
            .try_get("signature_verify_result")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        replay_check_result: row
            .try_get("replay_check_result")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        idempotency_check_result: row
            .try_get("idempotency_check_result")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        validation_result: row
            .try_get("validation_result")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        router_result: row
            .try_get("router_result")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        business_result_code: row
            .try_get("business_result_code")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        error_detail: row
            .try_get("error_detail")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        received_at: row
            .try_get("received_at")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        completed_at: row
            .try_get("completed_at")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        trace_id: TraceId(
            row.try_get("trace_id")
                .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        ),
    })
}

fn row_to_behavior_event_record(
    row: &sqlx::postgres::PgRow,
) -> Result<BehaviorEventRecord, AuditStoreError> {
    Ok(BehaviorEventRecord {
        behavior_event_id: row
            .try_get::<Uuid, _>("behavior_event_id")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .to_string(),
        event_kind: row
            .try_get("event_kind")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        event_source: row
            .try_get("event_source")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        room_id: row
            .try_get::<Option<Uuid>, _>("room_id")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .map(RoomId),
        hand_id: row
            .try_get::<Option<Uuid>, _>("hand_id")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .map(HandId),
        seat_id: row
            .try_get::<Option<i16>, _>("seat_id")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .and_then(|v| u8::try_from(v).ok()),
        action_seq: row
            .try_get::<Option<i32>, _>("action_seq")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .and_then(|v| u32::try_from(v).ok()),
        related_attempt_id: row
            .try_get::<Option<Uuid>, _>("related_attempt_id")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?
            .map(|v| v.to_string()),
        related_tx_hash: row
            .try_get("related_tx_hash")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        severity: row
            .try_get("severity")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        payload_json: row
            .try_get("payload_json")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        occurred_at: row
            .try_get("occurred_at")
            .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        trace_id: TraceId(
            row.try_get("trace_id")
                .map_err(|e| AuditStoreError::Database(e.to_string()))?,
        ),
    })
}

fn parse_uuid_field(field: &'static str, value: &str) -> Result<Uuid, AuditStoreError> {
    Uuid::parse_str(value).map_err(|_| AuditStoreError::InvalidUuid {
        field,
        value: value.to_string(),
    })
}

fn opt_i16(v: Option<SeatId>) -> Option<i16> {
    v.map(i16::from)
}

fn opt_i32(v: Option<u32>) -> Option<i32> {
    v.and_then(|n| i32::try_from(n).ok())
}

fn hand_event_type(kind: &poker_domain::HandEventKind) -> &'static str {
    match kind {
        poker_domain::HandEventKind::HandStarted => "HandStarted",
        poker_domain::HandEventKind::TurnStarted { .. } => "TurnStarted",
        poker_domain::HandEventKind::ActionAccepted(_) => "ActionAccepted",
        poker_domain::HandEventKind::StreetChanged { .. } => "StreetChanged",
        poker_domain::HandEventKind::PotUpdated { .. } => "PotUpdated",
        poker_domain::HandEventKind::HandClosed => "HandClosed",
    }
}

#[async_trait]
impl AuditRepository for PostgresAuditRepository {
    async fn insert_action_attempt(
        &self,
        record: &ActionAttemptRecord,
    ) -> Result<(), AuditStoreError> {
        let attempt_id = parse_uuid_field("attempt_id", &record.attempt_id)?;
        let request_id = parse_uuid_field("request_id", &record.request_id)?;
        sqlx::query(
            r#"
            INSERT INTO audit_action_attempts (
                attempt_id, request_id, request_nonce, agent_id, session_id, room_id, hand_id, seat_id, action_seq,
                method, action_type, request_payload_json, signature_verify_result, replay_check_result,
                idempotency_check_result, validation_result, router_result, business_result_code,
                error_detail, received_at, completed_at, trace_id
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9,
                $10, $11, $12, $13, $14,
                $15, $16, $17, $18,
                $19, $20, $21, $22
            )
            ON CONFLICT (request_id) DO UPDATE SET
                signature_verify_result = EXCLUDED.signature_verify_result,
                replay_check_result = EXCLUDED.replay_check_result,
                idempotency_check_result = EXCLUDED.idempotency_check_result,
                validation_result = EXCLUDED.validation_result,
                router_result = EXCLUDED.router_result,
                business_result_code = EXCLUDED.business_result_code,
                error_detail = EXCLUDED.error_detail,
                completed_at = EXCLUDED.completed_at
            "#,
        )
        .bind(attempt_id)
        .bind(request_id)
        .bind(&record.request_nonce)
        .bind(record.agent_id.map(|v| v.0))
        .bind(record.session_id.map(|v| v.0))
        .bind(record.room_id.map(|v| v.0))
        .bind(record.hand_id.map(|v| v.0))
        .bind(opt_i16(record.seat_id))
        .bind(opt_i32(record.action_seq))
        .bind(&record.method)
        .bind(&record.action_type)
        .bind(&record.request_payload_json)
        .bind(&record.signature_verify_result)
        .bind(&record.replay_check_result)
        .bind(&record.idempotency_check_result)
        .bind(&record.validation_result)
        .bind(&record.router_result)
        .bind(&record.business_result_code)
        .bind(&record.error_detail)
        .bind(record.received_at)
        .bind(record.completed_at)
        .bind(record.trace_id.0)
        .execute(&self.pool)
        .await
        .map_err(|e| AuditStoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn insert_behavior_event(
        &self,
        record: &BehaviorEventRecord,
    ) -> Result<(), AuditStoreError> {
        let behavior_event_id = parse_uuid_field("behavior_event_id", &record.behavior_event_id)?;
        let related_attempt_id = record
            .related_attempt_id
            .as_deref()
            .map(|v| parse_uuid_field("related_attempt_id", v))
            .transpose()?;
        sqlx::query(
            r#"
            INSERT INTO audit_behavior_events (
                behavior_event_id, event_kind, event_source, room_id, hand_id, seat_id, action_seq,
                related_attempt_id, related_tx_hash, severity, payload_json, occurred_at, trace_id
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7,
                $8, $9, $10, $11, $12, $13
            )
            "#,
        )
        .bind(behavior_event_id)
        .bind(&record.event_kind)
        .bind(&record.event_source)
        .bind(record.room_id.map(|v| v.0))
        .bind(record.hand_id.map(|v| v.0))
        .bind(opt_i16(record.seat_id))
        .bind(opt_i32(record.action_seq))
        .bind(related_attempt_id)
        .bind(&record.related_tx_hash)
        .bind(&record.severity)
        .bind(&record.payload_json)
        .bind(record.occurred_at)
        .bind(record.trace_id.0)
        .execute(&self.pool)
        .await
        .map_err(|e| AuditStoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn append_hand_events(&self, events: &[HandEvent]) -> Result<(), AuditStoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?;
        for event in events {
            let event_payload = serde_json::to_value(&event.kind)
                .map_err(|e| AuditStoreError::Serialization(e.to_string()))?;
            sqlx::query(
                r#"
                INSERT INTO hand_events (
                    hand_event_id, room_id, hand_id, event_seq, event_type, event_payload_json, trace_id
                ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT (hand_id, event_seq) DO NOTHING
                "#,
            )
            .bind(Uuid::now_v7())
            .bind(event.room_id.0)
            .bind(event.hand_id.0)
            .bind(i32::try_from(event.event_seq).unwrap_or(i32::MAX))
            .bind(hand_event_type(&event.kind))
            .bind(event_payload)
            .bind(event.trace_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn insert_outbox_event(&self, record: &OutboxEventRecord) -> Result<(), AuditStoreError> {
        let outbox_event_id = parse_uuid_field("outbox_event_id", &record.outbox_event_id)?;
        sqlx::query(
            r#"
            INSERT INTO outbox_events (
                outbox_event_id, topic, partition_key, payload_json, status, attempts,
                available_at, delivered_at, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (outbox_event_id) DO UPDATE SET
                topic = EXCLUDED.topic,
                partition_key = EXCLUDED.partition_key,
                payload_json = EXCLUDED.payload_json,
                status = EXCLUDED.status,
                attempts = EXCLUDED.attempts,
                available_at = EXCLUDED.available_at,
                delivered_at = EXCLUDED.delivered_at
            "#,
        )
        .bind(outbox_event_id)
        .bind(&record.topic)
        .bind(&record.partition_key)
        .bind(&record.payload_json)
        .bind(&record.status)
        .bind(i32::try_from(record.attempts).unwrap_or(i32::MAX))
        .bind(record.available_at)
        .bind(record.delivered_at)
        .bind(record.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| AuditStoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn fetch_pending_outbox_events(
        &self,
        limit: usize,
    ) -> Result<Vec<OutboxEventRecord>, AuditStoreError> {
        let rows = sqlx::query(
            r#"
            SELECT outbox_event_id, topic, partition_key, payload_json, status, attempts,
                   available_at, delivered_at, created_at
            FROM outbox_events
            WHERE status = 'pending' AND available_at <= NOW()
            ORDER BY created_at ASC
            LIMIT $1
            "#,
        )
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AuditStoreError::Database(e.to_string()))?;

        rows.into_iter()
            .map(|row| {
                let attempts_i32: i32 = row
                    .try_get("attempts")
                    .map_err(|e| AuditStoreError::Database(e.to_string()))?;
                Ok(OutboxEventRecord {
                    outbox_event_id: row
                        .try_get::<Uuid, _>("outbox_event_id")
                        .map_err(|e| AuditStoreError::Database(e.to_string()))?
                        .to_string(),
                    topic: row
                        .try_get("topic")
                        .map_err(|e| AuditStoreError::Database(e.to_string()))?,
                    partition_key: row
                        .try_get("partition_key")
                        .map_err(|e| AuditStoreError::Database(e.to_string()))?,
                    payload_json: row
                        .try_get("payload_json")
                        .map_err(|e| AuditStoreError::Database(e.to_string()))?,
                    status: row
                        .try_get("status")
                        .map_err(|e| AuditStoreError::Database(e.to_string()))?,
                    attempts: u32::try_from(attempts_i32).unwrap_or_default(),
                    available_at: row
                        .try_get("available_at")
                        .map_err(|e| AuditStoreError::Database(e.to_string()))?,
                    delivered_at: row
                        .try_get("delivered_at")
                        .map_err(|e| AuditStoreError::Database(e.to_string()))?,
                    created_at: row
                        .try_get("created_at")
                        .map_err(|e| AuditStoreError::Database(e.to_string()))?,
                })
            })
            .collect()
    }

    async fn mark_outbox_event_delivered(
        &self,
        outbox_event_id: &str,
        delivered_at: DateTime<Utc>,
    ) -> Result<(), AuditStoreError> {
        let outbox_event_id = parse_uuid_field("outbox_event_id", outbox_event_id)?;
        sqlx::query(
            r#"
            UPDATE outbox_events
            SET status = 'delivered', delivered_at = $2
            WHERE outbox_event_id = $1
            "#,
        )
        .bind(outbox_event_id)
        .bind(delivered_at)
        .execute(&self.pool)
        .await
        .map_err(|e| AuditStoreError::Database(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl AuditRepository for InMemoryAuditRepository {
    async fn insert_action_attempt(
        &self,
        record: &ActionAttemptRecord,
    ) -> Result<(), AuditStoreError> {
        self.action_attempts
            .lock()
            .map_err(|_| AuditStoreError::LockPoisoned)?
            .push(record.clone());
        Ok(())
    }

    async fn insert_behavior_event(
        &self,
        record: &BehaviorEventRecord,
    ) -> Result<(), AuditStoreError> {
        self.behavior_events
            .lock()
            .map_err(|_| AuditStoreError::LockPoisoned)?
            .push(record.clone());
        Ok(())
    }

    async fn append_hand_events(&self, events: &[HandEvent]) -> Result<(), AuditStoreError> {
        self.hand_events
            .lock()
            .map_err(|_| AuditStoreError::LockPoisoned)?
            .extend_from_slice(events);
        Ok(())
    }

    async fn insert_outbox_event(&self, record: &OutboxEventRecord) -> Result<(), AuditStoreError> {
        self.outbox_events
            .lock()
            .map_err(|_| AuditStoreError::LockPoisoned)?
            .push(record.clone());
        Ok(())
    }

    async fn fetch_pending_outbox_events(
        &self,
        limit: usize,
    ) -> Result<Vec<OutboxEventRecord>, AuditStoreError> {
        let guard = self
            .outbox_events
            .lock()
            .map_err(|_| AuditStoreError::LockPoisoned)?;
        Ok(guard
            .iter()
            .filter(|rec| rec.status == "pending")
            .take(limit)
            .cloned()
            .collect())
    }

    async fn mark_outbox_event_delivered(
        &self,
        outbox_event_id: &str,
        delivered_at: DateTime<Utc>,
    ) -> Result<(), AuditStoreError> {
        let mut guard = self
            .outbox_events
            .lock()
            .map_err(|_| AuditStoreError::LockPoisoned)?;
        if let Some(rec) = guard
            .iter_mut()
            .find(|rec| rec.outbox_event_id == outbox_event_id)
        {
            rec.status = "delivered".to_string();
            rec.delivered_at = Some(delivered_at);
        }
        Ok(())
    }
}

#[async_trait]
impl AuditReadRepository for InMemoryAuditRepository {
    async fn get_action_attempt_by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<ActionAttemptRecord>, AuditStoreError> {
        let guard = self
            .action_attempts
            .lock()
            .map_err(|_| AuditStoreError::LockPoisoned)?;
        Ok(guard.iter().find(|r| r.request_id == request_id).cloned())
    }

    async fn list_behavior_events(
        &self,
        room_id: Option<RoomId>,
        hand_id: Option<HandId>,
        page: PageRequest,
    ) -> Result<Vec<BehaviorEventRecord>, AuditStoreError> {
        let guard = self
            .behavior_events
            .lock()
            .map_err(|_| AuditStoreError::LockPoisoned)?;
        Ok(guard
            .iter()
            .filter(|r| room_id.is_none_or(|id| r.room_id == Some(id)))
            .filter(|r| hand_id.is_none_or(|id| r.hand_id == Some(id)))
            .skip(page.offset)
            .take(page.limit)
            .cloned()
            .collect())
    }

    async fn list_hand_events(
        &self,
        hand_id: HandId,
        page: PageRequest,
    ) -> Result<Vec<HandEvent>, AuditStoreError> {
        let guard = self
            .hand_events
            .lock()
            .map_err(|_| AuditStoreError::LockPoisoned)?;
        Ok(guard
            .iter()
            .filter(|r| r.hand_id == hand_id)
            .skip(page.offset)
            .take(page.limit)
            .cloned()
            .collect())
    }
}

#[async_trait]
impl AuditReadRepository for PostgresAuditRepository {
    async fn get_action_attempt_by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<ActionAttemptRecord>, AuditStoreError> {
        let request_id = parse_uuid_field("request_id", request_id)?;
        let row = sqlx::query("SELECT * FROM audit_action_attempts WHERE request_id = $1 LIMIT 1")
            .bind(request_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| AuditStoreError::Database(e.to_string()))?;
        row.as_ref().map(row_to_action_attempt_record).transpose()
    }

    async fn list_behavior_events(
        &self,
        room_id: Option<RoomId>,
        hand_id: Option<HandId>,
        page: PageRequest,
    ) -> Result<Vec<BehaviorEventRecord>, AuditStoreError> {
        let rows = sqlx::query(
            r#"
            SELECT * FROM audit_behavior_events
            WHERE ($1::uuid IS NULL OR room_id = $1)
              AND ($2::uuid IS NULL OR hand_id = $2)
            ORDER BY occurred_at DESC
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(room_id.map(|v| v.0))
        .bind(hand_id.map(|v| v.0))
        .bind(i64::try_from(page.limit).unwrap_or(i64::MAX))
        .bind(i64::try_from(page.offset).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AuditStoreError::Database(e.to_string()))?;
        rows.iter().map(row_to_behavior_event_record).collect()
    }

    async fn list_hand_events(
        &self,
        hand_id: HandId,
        page: PageRequest,
    ) -> Result<Vec<HandEvent>, AuditStoreError> {
        let rows = sqlx::query(
            r#"
            SELECT room_id, hand_id, event_seq, event_payload_json, trace_id, created_at
            FROM hand_events
            WHERE hand_id = $1
            ORDER BY event_seq ASC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(hand_id.0)
        .bind(i64::try_from(page.limit).unwrap_or(i64::MAX))
        .bind(i64::try_from(page.offset).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AuditStoreError::Database(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let payload: Value = row
                .try_get("event_payload_json")
                .map_err(|e| AuditStoreError::Database(e.to_string()))?;
            let kind = serde_json::from_value(payload)
                .map_err(|e| AuditStoreError::Serialization(e.to_string()))?;
            out.push(HandEvent {
                room_id: RoomId(
                    row.try_get("room_id")
                        .map_err(|e| AuditStoreError::Database(e.to_string()))?,
                ),
                hand_id: HandId(
                    row.try_get("hand_id")
                        .map_err(|e| AuditStoreError::Database(e.to_string()))?,
                ),
                event_seq: u32::try_from(
                    row.try_get::<i32, _>("event_seq")
                        .map_err(|e| AuditStoreError::Database(e.to_string()))?,
                )
                .unwrap_or_default(),
                trace_id: TraceId(
                    row.try_get("trace_id")
                        .map_err(|e| AuditStoreError::Database(e.to_string()))?,
                ),
                occurred_at: row
                    .try_get("created_at")
                    .map_err(|e| AuditStoreError::Database(e.to_string()))?,
                kind,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use poker_domain::{HandEventKind, RoomId};

    #[tokio::test]
    async fn in_memory_outbox_fetch_and_mark_delivered() {
        let repo = InMemoryAuditRepository::default();
        let record = OutboxEventRecord {
            outbox_event_id: "evt-1".to_string(),
            topic: "hand.events".to_string(),
            partition_key: "p1".to_string(),
            payload_json: serde_json::json!({}),
            status: "pending".to_string(),
            attempts: 0,
            available_at: Utc::now(),
            delivered_at: None,
            created_at: Utc::now(),
        };
        repo.insert_outbox_event(&record).await.expect("insert");

        let pending = repo.fetch_pending_outbox_events(10).await.expect("fetch");
        assert_eq!(pending.len(), 1);
        repo.mark_outbox_event_delivered("evt-1", Utc::now())
            .await
            .expect("mark");
        let pending = repo.fetch_pending_outbox_events(10).await.expect("fetch");
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn in_memory_append_hand_events_stores_records() {
        let repo = InMemoryAuditRepository::default();
        let event = HandEvent {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            event_seq: 1,
            trace_id: TraceId::new(),
            occurred_at: Utc::now(),
            kind: HandEventKind::HandStarted,
        };
        repo.append_hand_events(&[event]).await.expect("append");
        let stored = repo.hand_events.lock().expect("lock");
        assert_eq!(stored.len(), 1);
    }
}
