use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use audit_store::{AuditReadRepository, InMemoryAuditRepository};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use ledger_store::{
    ChainTxReadRepository, InMemoryChainTxRepository, InMemorySettlementPersistenceRepository,
    SettlementPersistenceReadRepository, SettlementPersistenceRepository,
    SettlementRecordStatusUpdate,
};
use poker_domain::RoomId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row};
use tracing::info;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub service: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsResponse {
    pub format: &'static str,
    pub body: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminStubResponse {
    pub ok: bool,
    pub resource: String,
    pub id: String,
    pub note: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminAuditRequestResponse {
    pub ok: bool,
    pub request_id: String,
    pub found: bool,
    pub record: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminAuditTxResponse {
    pub ok: bool,
    pub tx_hash: String,
    pub found: bool,
    pub verification: Option<Value>,
    pub tx_binding: Option<Value>,
    pub exception_credits: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminHandReplayResponse {
    pub ok: bool,
    pub hand_id: String,
    pub found: bool,
    pub events: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminSettlementHandResponse {
    pub ok: bool,
    pub hand_id: String,
    pub found: bool,
    pub plan: Option<Value>,
    pub records: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdminMarkManualReviewRequest {
    pub retry_count: Option<i32>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminMarkManualReviewResponse {
    pub ok: bool,
    pub tx_hash: String,
    pub settlement_status: &'static str,
    pub retry_count: i32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ShowdownSeatCardsInput {
    pub seat_id: u8,
    pub cards: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AdminUpsertShowdownInputRequest {
    pub room_id: String,
    pub board: String,
    pub revealed_hole_cards: Vec<ShowdownSeatCardsInput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminUpsertShowdownInputResponse {
    pub ok: bool,
    pub hand_id: String,
    pub accepted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoomAdminAggregate {
    pub room_id: RoomId,
    pub room_status: String,
    pub dealer_address: String,
    pub max_players: u16,
    pub occupied_seats: u16,
    pub latest_hand_id: Option<String>,
    pub latest_hand_no: Option<u64>,
    pub latest_hand_status: Option<String>,
    pub latest_hand_started_at: Option<DateTime<Utc>>,
    pub open_exception_credit_count: u64,
    pub open_exception_credit_total: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminRoomResponse {
    pub ok: bool,
    pub room_id: String,
    pub found: bool,
    pub room: Option<RoomAdminAggregate>,
}

#[async_trait]
pub trait RoomAdminReadRepository: Send + Sync {
    async fn get_room_admin_aggregate(
        &self,
        room_id: RoomId,
    ) -> Result<Option<RoomAdminAggregate>, String>;
}

#[async_trait]
pub trait ShowdownAdminRepository: Send + Sync {
    async fn upsert_showdown_input(
        &self,
        hand_id: poker_domain::HandId,
        payload: AdminUpsertShowdownInputRequest,
    ) -> Result<(), String>;
}

#[derive(Debug, Default)]
pub struct NoopShowdownAdminRepository;

#[async_trait]
impl ShowdownAdminRepository for NoopShowdownAdminRepository {
    async fn upsert_showdown_input(
        &self,
        _hand_id: poker_domain::HandId,
        _payload: AdminUpsertShowdownInputRequest,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryRoomAdminReadRepository {
    data: Arc<Mutex<HashMap<RoomId, RoomAdminAggregate>>>,
}

impl InMemoryRoomAdminReadRepository {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&self, aggregate: RoomAdminAggregate) -> Result<(), String> {
        self.data
            .lock()
            .map_err(|_| "room admin read lock poisoned".to_string())?
            .insert(aggregate.room_id, aggregate);
        Ok(())
    }
}

#[async_trait]
impl RoomAdminReadRepository for InMemoryRoomAdminReadRepository {
    async fn get_room_admin_aggregate(
        &self,
        room_id: RoomId,
    ) -> Result<Option<RoomAdminAggregate>, String> {
        let guard = self
            .data
            .lock()
            .map_err(|_| "room admin read lock poisoned".to_string())?;
        Ok(guard.get(&room_id).cloned())
    }
}

#[derive(Debug, Clone)]
pub struct PostgresRoomAdminReadRepository {
    pool: PgPool,
}

impl PostgresRoomAdminReadRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RoomAdminReadRepository for PostgresRoomAdminReadRepository {
    async fn get_room_admin_aggregate(
        &self,
        room_id: RoomId,
    ) -> Result<Option<RoomAdminAggregate>, String> {
        let row = sqlx::query(
            r#"
            SELECT
                r.room_id,
                r.room_status,
                r.dealer_address,
                r.max_players,
                COALESCE(seats.occupied_seats, 0) AS occupied_seats,
                lh.hand_id AS latest_hand_id,
                lh.hand_no AS latest_hand_no,
                lh.hand_status AS latest_hand_status,
                lh.started_at AS latest_hand_started_at,
                COALESCE(ec.open_count, 0) AS open_exception_credit_count,
                COALESCE(ec.open_total::text, '0') AS open_exception_credit_total
            FROM rooms r
            LEFT JOIN LATERAL (
                SELECT COUNT(*)::bigint AS occupied_seats
                FROM room_seats rs
                WHERE rs.room_id = r.room_id AND rs.agent_id IS NOT NULL
            ) seats ON TRUE
            LEFT JOIN LATERAL (
                SELECT h.hand_id, h.hand_no, h.hand_status, h.started_at
                FROM hands h
                WHERE h.room_id = r.room_id
                ORDER BY h.hand_no DESC
                LIMIT 1
            ) lh ON TRUE
            LEFT JOIN LATERAL (
                SELECT COUNT(*)::bigint AS open_count, COALESCE(SUM(amount), 0) AS open_total
                FROM exception_credits ec
                WHERE ec.room_id = r.room_id AND ec.status = 'open'
            ) ec ON TRUE
            WHERE r.room_id = $1
            LIMIT 1
            "#,
        )
        .bind(room_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| e.to_string())?;

        row.map(|row| {
            Ok(RoomAdminAggregate {
                room_id: RoomId(row.try_get("room_id").map_err(|e| e.to_string())?),
                room_status: row.try_get("room_status").map_err(|e| e.to_string())?,
                dealer_address: row.try_get("dealer_address").map_err(|e| e.to_string())?,
                max_players: u16::try_from(
                    row.try_get::<i16, _>("max_players")
                        .map_err(|e| e.to_string())?,
                )
                .unwrap_or_default(),
                occupied_seats: u16::try_from(
                    row.try_get::<i64, _>("occupied_seats")
                        .map_err(|e| e.to_string())?,
                )
                .unwrap_or_default(),
                latest_hand_id: row
                    .try_get::<Option<Uuid>, _>("latest_hand_id")
                    .map_err(|e| e.to_string())?
                    .map(|v| v.to_string()),
                latest_hand_no: row
                    .try_get::<Option<i64>, _>("latest_hand_no")
                    .map_err(|e| e.to_string())?
                    .and_then(|v| u64::try_from(v).ok()),
                latest_hand_status: row
                    .try_get("latest_hand_status")
                    .map_err(|e| e.to_string())?,
                latest_hand_started_at: row
                    .try_get("latest_hand_started_at")
                    .map_err(|e| e.to_string())?,
                open_exception_credit_count: u64::try_from(
                    row.try_get::<i64, _>("open_exception_credit_count")
                        .map_err(|e| e.to_string())?,
                )
                .unwrap_or_default(),
                open_exception_credit_total: row
                    .try_get("open_exception_credit_total")
                    .map_err(|e| e.to_string())?,
            })
        })
        .transpose()
    }
}

#[derive(Clone)]
pub struct OpsState {
    pub audit_read: Arc<dyn AuditReadRepository>,
    pub chain_tx_read: Arc<dyn ChainTxReadRepository>,
    pub room_admin_read: Arc<dyn RoomAdminReadRepository>,
    pub settlement_read: Arc<dyn SettlementPersistenceReadRepository>,
    pub settlement_write: Arc<dyn SettlementPersistenceRepository>,
    pub showdown_admin: Arc<dyn ShowdownAdminRepository>,
}

impl std::fmt::Debug for OpsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpsState").finish_non_exhaustive()
    }
}

pub fn build_router() -> Router {
    build_router_with_state(OpsState {
        audit_read: Arc::new(InMemoryAuditRepository::default()),
        chain_tx_read: Arc::new(InMemoryChainTxRepository::new()),
        room_admin_read: Arc::new(InMemoryRoomAdminReadRepository::new()),
        settlement_read: Arc::new(InMemorySettlementPersistenceRepository::new()),
        settlement_write: Arc::new(InMemorySettlementPersistenceRepository::new()),
        showdown_admin: Arc::new(NoopShowdownAdminRepository),
    })
}

pub fn build_router_with_state(state: OpsState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/admin/rooms/{id}", get(admin_room))
        .route("/admin/hands/{id}/replay", get(admin_hand_replay))
        .route("/admin/settlements/hand/{id}", get(admin_settlement_hand))
        .route(
            "/admin/settlements/hand/{id}/showdown-input",
            post(admin_upsert_showdown_input),
        )
        .route(
            "/admin/settlements/tx/{id}/manual-review",
            post(admin_mark_settlement_manual_review),
        )
        .route("/admin/audit/request/{id}", get(admin_audit_request))
        .route("/admin/audit/tx/{id}", get(admin_audit_tx))
        .with_state(state)
}

async fn admin_upsert_showdown_input(
    State(state): State<OpsState>,
    Path(id): Path<String>,
    Json(body): Json<AdminUpsertShowdownInputRequest>,
) -> Json<AdminUpsertShowdownInputResponse> {
    info!(
        route = "/admin/settlements/hand/:id/showdown-input",
        hand_id = %id,
        "ops http request"
    );
    let accepted = if let Ok(hand_uuid) = Uuid::parse_str(&id) {
        state
            .showdown_admin
            .upsert_showdown_input(poker_domain::HandId(hand_uuid), body)
            .await
            .is_ok()
    } else {
        false
    };
    Json(AdminUpsertShowdownInputResponse {
        ok: true,
        hand_id: id,
        accepted,
    })
}

async fn admin_mark_settlement_manual_review(
    State(state): State<OpsState>,
    Path(id): Path<String>,
    Json(body): Json<AdminMarkManualReviewRequest>,
) -> Json<AdminMarkManualReviewResponse> {
    info!(
        route = "/admin/settlements/tx/:id/manual-review",
        tx_hash = %id,
        "ops http request"
    );
    let retry_count = body.retry_count.unwrap_or(1).max(0);
    let reason = body
        .reason
        .unwrap_or_else(|| "manual review required (ops override)".to_string());
    let _ = state
        .settlement_write
        .update_settlement_record_status_by_tx_hash(&SettlementRecordStatusUpdate {
            tx_hash: id.clone(),
            settlement_status: "manual_review_required".to_string(),
            retry_count,
            error_detail: Some(reason),
            updated_at: Utc::now(),
        })
        .await;

    Json(AdminMarkManualReviewResponse {
        ok: true,
        tx_hash: id,
        settlement_status: "manual_review_required",
        retry_count,
    })
}

async fn health() -> Json<HealthResponse> {
    info!(route = "/health", "ops http request");
    Json(HealthResponse {
        ok: true,
        service: "ops-http",
    })
}

async fn metrics() -> Json<MetricsResponse> {
    info!(route = "/metrics", "ops http request");
    Json(MetricsResponse {
        format: "prometheus-text",
        body: "# metrics not implemented yet\n",
    })
}

async fn admin_room(
    State(state): State<OpsState>,
    Path(id): Path<String>,
) -> Json<AdminRoomResponse> {
    info!(route = "/admin/rooms/:id", room_id = %id, "ops http request");
    let room = Uuid::parse_str(&id).ok().map(RoomId).map(|rid| async move {
        state
            .room_admin_read
            .get_room_admin_aggregate(rid)
            .await
            .ok()
            .flatten()
    });
    let room = match room {
        Some(fut) => fut.await,
        None => None,
    };
    Json(AdminRoomResponse {
        ok: true,
        room_id: id,
        found: room.is_some(),
        room,
    })
}

async fn admin_hand_replay(
    State(state): State<OpsState>,
    Path(id): Path<String>,
) -> Json<AdminHandReplayResponse> {
    info!(route = "/admin/hands/:id/replay", hand_id = %id, "ops http request");
    let events = Uuid::parse_str(&id)
        .ok()
        .map(poker_domain::HandId)
        .map(|hand_id| async move {
            state
                .audit_read
                .list_hand_events(
                    hand_id,
                    audit_store::PageRequest {
                        limit: 500,
                        offset: 0,
                    },
                )
                .await
                .ok()
                .unwrap_or_default()
        });
    let events = match events {
        Some(fut) => fut.await,
        None => Vec::new(),
    };
    let events = events
        .into_iter()
        .filter_map(|e| serde_json::to_value(e).ok())
        .collect::<Vec<_>>();

    Json(AdminHandReplayResponse {
        ok: true,
        hand_id: id,
        found: !events.is_empty(),
        events,
    })
}

async fn admin_settlement_hand(
    State(state): State<OpsState>,
    Path(id): Path<String>,
) -> Json<AdminSettlementHandResponse> {
    info!(route = "/admin/settlements/hand/:id", hand_id = %id, "ops http request");
    let Some(hand_uuid) = Uuid::parse_str(&id).ok() else {
        return Json(AdminSettlementHandResponse {
            ok: true,
            hand_id: id,
            found: false,
            plan: None,
            records: Vec::new(),
        });
    };
    let hand_id = poker_domain::HandId(hand_uuid);
    let records = state
        .settlement_read
        .list_settlement_records_by_hand(hand_id)
        .await
        .ok()
        .unwrap_or_default();
    let plan = if let Some(first) = records.first() {
        state
            .settlement_read
            .get_settlement_plan(first.settlement_plan_id)
            .await
            .ok()
            .flatten()
            .and_then(|p| serde_json::to_value(p).ok())
    } else {
        None
    };
    let records_json = records
        .into_iter()
        .filter_map(|r| serde_json::to_value(r).ok())
        .collect::<Vec<_>>();

    Json(AdminSettlementHandResponse {
        ok: true,
        hand_id: id,
        found: plan.is_some() || !records_json.is_empty(),
        plan,
        records: records_json,
    })
}

async fn admin_audit_request(
    State(state): State<OpsState>,
    Path(id): Path<String>,
) -> Json<AdminAuditRequestResponse> {
    info!(route = "/admin/audit/request/:id", request_id = %id, "ops http request");
    let record = state
        .audit_read
        .get_action_attempt_by_request_id(&id)
        .await
        .ok()
        .flatten()
        .and_then(|r| serde_json::to_value(r).ok());

    Json(AdminAuditRequestResponse {
        ok: true,
        request_id: id,
        found: record.is_some(),
        record,
    })
}

async fn admin_audit_tx(
    State(state): State<OpsState>,
    Path(id): Path<String>,
) -> Json<AdminAuditTxResponse> {
    info!(route = "/admin/audit/tx/:id", tx_hash = %id, "ops http request");
    let verification = state
        .chain_tx_read
        .get_tx_verification(&id)
        .await
        .ok()
        .flatten()
        .and_then(|r| serde_json::to_value(r).ok());
    let tx_binding = state
        .chain_tx_read
        .get_tx_binding_by_tx_hash(&id)
        .await
        .ok()
        .flatten()
        .and_then(|r| serde_json::to_value(r).ok());
    let exception_credits = state
        .chain_tx_read
        .list_exception_credits(
            None,
            ledger_store::PageRequest {
                limit: 100,
                offset: 0,
            },
        )
        .await
        .ok()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.tx_hash.as_deref() == Some(id.as_str()))
        .filter_map(|r| serde_json::to_value(r).ok())
        .collect::<Vec<_>>();

    Json(AdminAuditTxResponse {
        ok: true,
        tx_hash: id,
        found: verification.is_some() || tx_binding.is_some() || !exception_credits.is_empty(),
        verification,
        tx_binding,
        exception_credits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use audit_store::{ActionAttemptRecord, AuditRepository};
    use chrono::Utc;
    use ledger_store::{
        ChainTxRepository, ExceptionCreditUpsert, InMemoryChainTxRepository,
        InMemorySettlementPersistenceRepository, SettlementPersistenceRepository,
        SettlementPlanRecordInsert, SettlementRecordInsert, TxBindingInsert, TxVerificationUpsert,
    };
    use poker_domain::{Chips, HandEvent, HandEventKind, RequestId, RoomId, TraceId};

    #[tokio::test]
    async fn admin_audit_request_reads_from_in_memory_repo() {
        let audit_repo = InMemoryAuditRepository::default();
        let req_id = RequestId::new();
        audit_repo
            .insert_action_attempt(&ActionAttemptRecord {
                attempt_id: RequestId::new().0.to_string(),
                request_id: req_id.0.to_string(),
                request_nonce: "n1".to_string(),
                agent_id: None,
                session_id: None,
                room_id: None,
                hand_id: None,
                seat_id: None,
                action_seq: None,
                method: "game.act".to_string(),
                action_type: Some("call".to_string()),
                request_payload_json: serde_json::json!({}),
                signature_verify_result: "ok".to_string(),
                replay_check_result: "ok".to_string(),
                idempotency_check_result: "ok".to_string(),
                validation_result: "ok".to_string(),
                router_result: "ok".to_string(),
                business_result_code: None,
                error_detail: None,
                received_at: Utc::now(),
                completed_at: Some(Utc::now()),
                trace_id: TraceId::new(),
            })
            .await
            .expect("insert");

        let resp = admin_audit_request(
            State(OpsState {
                audit_read: Arc::new(audit_repo),
                chain_tx_read: Arc::new(InMemoryChainTxRepository::new()),
                room_admin_read: Arc::new(InMemoryRoomAdminReadRepository::new()),
                settlement_read: Arc::new(InMemorySettlementPersistenceRepository::new()),
                settlement_write: Arc::new(InMemorySettlementPersistenceRepository::new()),
                showdown_admin: Arc::new(NoopShowdownAdminRepository),
            }),
            Path(req_id.0.to_string()),
        )
        .await
        .0;

        assert!(resp.found);
        assert!(resp.record.is_some());
    }

    #[tokio::test]
    async fn admin_audit_tx_reads_verification_and_exception_credit() {
        let tx_hash = "0xabc".to_string();
        let room_id = RoomId::new();
        let chain_repo = InMemoryChainTxRepository::new();
        chain_repo
            .upsert_tx_verification(&TxVerificationUpsert {
                tx_hash: tx_hash.clone(),
                room_id,
                hand_id: None,
                seat_id: Some(1),
                action_seq: Some(2),
                status: "matched".to_string(),
                confirmations: 1,
                failure_reason: None,
                observed_from: Some("0xfrom".to_string()),
                observed_to: Some("0xto".to_string()),
                observed_amount: Some(Chips(10)),
                verified_at: Utc::now(),
                trace_id: TraceId::new(),
            })
            .await
            .expect("tx verification");
        chain_repo
            .upsert_exception_credit(&ExceptionCreditUpsert {
                room_id,
                seat_id: Some(1),
                tx_hash: tx_hash.clone(),
                credit_type: "unmatched".to_string(),
                amount: Chips(10),
                status: "open".to_string(),
                reason: "mismatch".to_string(),
                updated_at: Utc::now(),
                trace_id: TraceId::new(),
            })
            .await
            .expect("exception credit");
        chain_repo
            .insert_tx_binding(&TxBindingInsert {
                tx_hash: tx_hash.clone(),
                room_id,
                hand_id: poker_domain::HandId::new(),
                seat_id: 1,
                action_seq: 2,
                expected_amount: Some(Chips(10)),
                binding_status: "submitted".to_string(),
                created_at: Utc::now(),
                trace_id: TraceId::new(),
            })
            .await
            .expect("tx binding");

        let resp = admin_audit_tx(
            State(OpsState {
                audit_read: Arc::new(InMemoryAuditRepository::default()),
                chain_tx_read: Arc::new(chain_repo),
                room_admin_read: Arc::new(InMemoryRoomAdminReadRepository::new()),
                settlement_read: Arc::new(InMemorySettlementPersistenceRepository::new()),
                settlement_write: Arc::new(InMemorySettlementPersistenceRepository::new()),
                showdown_admin: Arc::new(NoopShowdownAdminRepository),
            }),
            Path(tx_hash),
        )
        .await
        .0;

        assert!(resp.found);
        assert!(resp.verification.is_some());
        assert!(resp.tx_binding.is_some());
        assert_eq!(resp.exception_credits.len(), 1);
    }

    #[tokio::test]
    async fn admin_hand_replay_reads_hand_events() {
        let audit_repo = InMemoryAuditRepository::default();
        let hand_id = poker_domain::HandId::new();
        audit_repo
            .append_hand_events(&[HandEvent {
                room_id: RoomId::new(),
                hand_id,
                event_seq: 1,
                trace_id: TraceId::new(),
                occurred_at: Utc::now(),
                kind: HandEventKind::HandStarted,
            }])
            .await
            .expect("append hand event");

        let resp = admin_hand_replay(
            State(OpsState {
                audit_read: Arc::new(audit_repo),
                chain_tx_read: Arc::new(InMemoryChainTxRepository::new()),
                room_admin_read: Arc::new(InMemoryRoomAdminReadRepository::new()),
                settlement_read: Arc::new(InMemorySettlementPersistenceRepository::new()),
                settlement_write: Arc::new(InMemorySettlementPersistenceRepository::new()),
                showdown_admin: Arc::new(NoopShowdownAdminRepository),
            }),
            Path(hand_id.0.to_string()),
        )
        .await
        .0;

        assert!(resp.found);
        assert_eq!(resp.events.len(), 1);
    }

    #[tokio::test]
    async fn admin_room_reads_room_aggregate() {
        let room_repo = InMemoryRoomAdminReadRepository::new();
        let room_id = RoomId::new();
        room_repo
            .upsert(RoomAdminAggregate {
                room_id,
                room_status: "active".to_string(),
                dealer_address: "cfx:room".to_string(),
                max_players: 6,
                occupied_seats: 2,
                latest_hand_id: None,
                latest_hand_no: Some(12),
                latest_hand_status: Some("running".to_string()),
                latest_hand_started_at: Some(Utc::now()),
                open_exception_credit_count: 1,
                open_exception_credit_total: "10".to_string(),
            })
            .expect("upsert room");

        let resp = admin_room(
            State(OpsState {
                audit_read: Arc::new(InMemoryAuditRepository::default()),
                chain_tx_read: Arc::new(InMemoryChainTxRepository::new()),
                room_admin_read: Arc::new(room_repo),
                settlement_read: Arc::new(InMemorySettlementPersistenceRepository::new()),
                settlement_write: Arc::new(InMemorySettlementPersistenceRepository::new()),
                showdown_admin: Arc::new(NoopShowdownAdminRepository),
            }),
            Path(room_id.0.to_string()),
        )
        .await
        .0;

        assert!(resp.found);
        assert_eq!(resp.room.expect("room").occupied_seats, 2);
    }

    #[tokio::test]
    async fn admin_settlement_hand_reads_plan_and_records() {
        let repo = InMemorySettlementPersistenceRepository::new();
        let room_id = RoomId::new();
        let hand_id = poker_domain::HandId::new();
        let plan_id = Uuid::now_v7();
        repo.insert_settlement_plan(&SettlementPlanRecordInsert {
            settlement_plan_id: plan_id,
            room_id,
            hand_id,
            status: "created".to_string(),
            rake_amount: Chips(1),
            payout_total: Chips(9),
            payload_json: serde_json::json!({"payouts":[]}),
            created_at: Utc::now(),
            finalized_at: None,
        })
        .await
        .expect("plan");
        repo.insert_settlement_record(&SettlementRecordInsert {
            settlement_record_id: Uuid::now_v7(),
            settlement_plan_id: plan_id,
            room_id,
            hand_id,
            tx_hash: Some("0xtx".to_string()),
            settlement_status: "submitted".to_string(),
            retry_count: 0,
            error_detail: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
        .await
        .expect("record");

        let resp = admin_settlement_hand(
            State(OpsState {
                audit_read: Arc::new(InMemoryAuditRepository::default()),
                chain_tx_read: Arc::new(InMemoryChainTxRepository::new()),
                room_admin_read: Arc::new(InMemoryRoomAdminReadRepository::new()),
                settlement_read: Arc::new(repo.clone()),
                settlement_write: Arc::new(repo),
                showdown_admin: Arc::new(NoopShowdownAdminRepository),
            }),
            Path(hand_id.0.to_string()),
        )
        .await
        .0;

        assert!(resp.found);
        assert!(resp.plan.is_some());
        assert_eq!(resp.records.len(), 1);
    }

    #[tokio::test]
    async fn admin_mark_settlement_manual_review_updates_status_by_tx_hash() {
        let repo = InMemorySettlementPersistenceRepository::new();
        let room_id = RoomId::new();
        let hand_id = poker_domain::HandId::new();
        let plan_id = Uuid::now_v7();
        repo.insert_settlement_plan(&SettlementPlanRecordInsert {
            settlement_plan_id: plan_id,
            room_id,
            hand_id,
            status: "created".to_string(),
            rake_amount: Chips::ZERO,
            payout_total: Chips(5),
            payload_json: serde_json::json!({}),
            created_at: Utc::now(),
            finalized_at: None,
        })
        .await
        .expect("plan");
        repo.insert_settlement_record(&SettlementRecordInsert {
            settlement_record_id: Uuid::now_v7(),
            settlement_plan_id: plan_id,
            room_id,
            hand_id,
            tx_hash: Some("0xtxmark".to_string()),
            settlement_status: "failed".to_string(),
            retry_count: 1,
            error_detail: Some("failed".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
        .await
        .expect("record");

        let resp = admin_mark_settlement_manual_review(
            State(OpsState {
                audit_read: Arc::new(InMemoryAuditRepository::default()),
                chain_tx_read: Arc::new(InMemoryChainTxRepository::new()),
                room_admin_read: Arc::new(InMemoryRoomAdminReadRepository::new()),
                settlement_read: Arc::new(repo.clone()),
                settlement_write: Arc::new(repo.clone()),
                showdown_admin: Arc::new(NoopShowdownAdminRepository),
            }),
            Path("0xtxmark".to_string()),
            Json(AdminMarkManualReviewRequest {
                retry_count: Some(2),
                reason: Some("ops escalation".to_string()),
            }),
        )
        .await
        .0;

        assert!(resp.ok);
        assert_eq!(resp.settlement_status, "manual_review_required");
        let records = repo
            .list_settlement_records_by_hand(hand_id)
            .await
            .expect("list");
        assert!(records.iter().any(|r| {
            r.tx_hash.as_deref() == Some("0xtxmark")
                && r.settlement_status == "manual_review_required"
                && r.retry_count == 2
        }));
    }

    #[derive(Debug, Default)]
    struct InMemoryShowdownAdminRepo {
        writes: Arc<Mutex<Vec<(poker_domain::HandId, AdminUpsertShowdownInputRequest)>>>,
    }

    #[async_trait]
    impl ShowdownAdminRepository for InMemoryShowdownAdminRepo {
        async fn upsert_showdown_input(
            &self,
            hand_id: poker_domain::HandId,
            payload: AdminUpsertShowdownInputRequest,
        ) -> Result<(), String> {
            self.writes
                .lock()
                .map_err(|_| "lock poisoned".to_string())?
                .push((hand_id, payload));
            Ok(())
        }
    }

    #[tokio::test]
    async fn admin_upsert_showdown_input_writes_repo() {
        let repo = Arc::new(InMemoryShowdownAdminRepo::default());
        let hand_id = poker_domain::HandId::new();
        let room_id = RoomId::new();

        let resp = admin_upsert_showdown_input(
            State(OpsState {
                audit_read: Arc::new(InMemoryAuditRepository::default()),
                chain_tx_read: Arc::new(InMemoryChainTxRepository::new()),
                room_admin_read: Arc::new(InMemoryRoomAdminReadRepository::new()),
                settlement_read: Arc::new(InMemorySettlementPersistenceRepository::new()),
                settlement_write: Arc::new(InMemorySettlementPersistenceRepository::new()),
                showdown_admin: repo.clone(),
            }),
            Path(hand_id.0.to_string()),
            Json(AdminUpsertShowdownInputRequest {
                room_id: room_id.0.to_string(),
                board: "AhKhQhJhTh".to_string(),
                revealed_hole_cards: vec![ShowdownSeatCardsInput {
                    seat_id: 0,
                    cards: "2c3d".to_string(),
                }],
            }),
        )
        .await
        .0;

        assert!(resp.accepted);
        let writes = repo.writes.lock().expect("lock");
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, hand_id);
        assert_eq!(writes[0].1.room_id, room_id.0.to_string());
    }
}
