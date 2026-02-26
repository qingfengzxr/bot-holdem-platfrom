use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use poker_domain::{AgentId, Chips, HandId, RoomId, SeatId, TraceId};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::{PgPool, Row, types::Json};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum LedgerStoreError {
    #[error("not implemented")]
    NotImplemented,
    #[error("store lock poisoned")]
    LockPoisoned,
    #[error("duplicate tx binding for hand/action")]
    DuplicateTxBinding,
    #[error("duplicate tx hash binding")]
    DuplicateTxHashBinding,
    #[error("database error: {0}")]
    Database(String),
    #[error("expected_amount is required for tx_binding")]
    MissingExpectedAmount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LedgerDirection {
    Debit,
    Credit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntryInsert {
    pub entry_type: String,
    pub room_id: Option<RoomId>,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
    pub agent_id: Option<AgentId>,
    pub asset_type: String,
    pub amount: Chips,
    pub direction: LedgerDirection,
    pub account_scope: String,
    pub related_tx_hash: Option<String>,
    pub related_event_id: Option<String>,
    pub related_attempt_id: Option<String>,
    pub remark: Option<String>,
    pub trace_id: TraceId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxVerificationUpsert {
    pub tx_hash: String,
    pub room_id: RoomId,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
    pub action_seq: Option<u32>,
    pub status: String,
    pub confirmations: u64,
    pub failure_reason: Option<String>,
    pub observed_from: Option<String>,
    pub observed_to: Option<String>,
    pub observed_amount: Option<Chips>,
    pub verified_at: DateTime<Utc>,
    pub trace_id: TraceId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxBindingInsert {
    pub tx_hash: String,
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub seat_id: SeatId,
    pub action_seq: u32,
    pub expected_amount: Option<Chips>,
    pub binding_status: String,
    pub created_at: DateTime<Utc>,
    pub trace_id: TraceId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExceptionCreditUpsert {
    pub room_id: RoomId,
    pub seat_id: Option<SeatId>,
    pub tx_hash: String,
    pub credit_type: String,
    pub amount: Chips,
    pub status: String,
    pub reason: String,
    pub updated_at: DateTime<Utc>,
    pub trace_id: TraceId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxVerificationRecord {
    pub tx_hash: String,
    pub chain_id: String,
    pub from_address: Option<String>,
    pub to_address: Option<String>,
    pub value: Option<Chips>,
    pub tx_status: String,
    pub confirmations: u64,
    pub verification_status: String,
    pub failure_reason: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxBindingRecord {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub action_seq: u32,
    pub seat_id: SeatId,
    pub expected_amount: Chips,
    pub tx_hash: String,
    pub binding_status: String,
    pub bound_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExceptionCreditRecord {
    pub room_id: RoomId,
    pub seat_id: SeatId,
    pub hand_id: Option<HandId>,
    pub tx_hash: Option<String>,
    pub credit_kind: String,
    pub amount: Chips,
    pub status: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomSigningKeyRecord {
    pub room_id: RoomId,
    pub address: String,
    pub encrypted_private_key: Vec<u8>,
    pub cipher_alg: String,
    pub key_version: i32,
    pub kek_id: String,
    pub nonce: Vec<u8>,
    pub aad: Vec<u8>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub rotated_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettlementPlanRecordInsert {
    pub settlement_plan_id: Uuid,
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub status: String,
    pub rake_amount: Chips,
    pub payout_total: Chips,
    pub payload_json: JsonValue,
    pub created_at: DateTime<Utc>,
    pub finalized_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettlementRecordInsert {
    pub settlement_record_id: Uuid,
    pub settlement_plan_id: Uuid,
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub tx_hash: Option<String>,
    pub settlement_status: String,
    pub retry_count: i32,
    pub error_detail: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettlementRecordStatusUpdate {
    pub tx_hash: String,
    pub settlement_status: String,
    pub retry_count: i32,
    pub error_detail: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementShowdownSeatCardsRecord {
    pub seat_id: SeatId,
    pub cards: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementShowdownInputRecord {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub board_cards: String,
    pub revealed_hole_cards: Vec<SettlementShowdownSeatCardsRecord>,
    pub source: String,
    pub updated_at: DateTime<Utc>,
    pub trace_id: TraceId,
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
pub trait LedgerRepository: Send + Sync {
    async fn insert_entries(&self, entries: &[LedgerEntryInsert]) -> Result<(), LedgerStoreError>;
}

#[async_trait]
pub trait ChainTxRepository: Send + Sync {
    async fn upsert_tx_verification(
        &self,
        record: &TxVerificationUpsert,
    ) -> Result<(), LedgerStoreError>;

    async fn insert_tx_binding(&self, record: &TxBindingInsert) -> Result<(), LedgerStoreError>;

    async fn upsert_exception_credit(
        &self,
        record: &ExceptionCreditUpsert,
    ) -> Result<(), LedgerStoreError>;
}

#[async_trait]
pub trait LedgerReadRepository: Send + Sync {
    async fn list_ledger_entries_by_room_hand(
        &self,
        room_id: Option<RoomId>,
        hand_id: Option<HandId>,
        page: PageRequest,
    ) -> Result<Vec<LedgerEntryInsert>, LedgerStoreError>;
}

#[async_trait]
pub trait ChainTxReadRepository: Send + Sync {
    async fn get_tx_verification(
        &self,
        tx_hash: &str,
    ) -> Result<Option<TxVerificationRecord>, LedgerStoreError>;

    async fn get_tx_binding_by_tx_hash(
        &self,
        tx_hash: &str,
    ) -> Result<Option<TxBindingRecord>, LedgerStoreError>;

    async fn get_tx_binding_by_hand_action(
        &self,
        hand_id: HandId,
        action_seq: u32,
    ) -> Result<Option<TxBindingRecord>, LedgerStoreError>;

    async fn list_exception_credits(
        &self,
        room_id: Option<RoomId>,
        page: PageRequest,
    ) -> Result<Vec<ExceptionCreditRecord>, LedgerStoreError>;
}

#[async_trait]
pub trait RoomSigningKeyReadRepository: Send + Sync {
    async fn get_active_room_signing_key(
        &self,
        room_id: RoomId,
    ) -> Result<Option<RoomSigningKeyRecord>, LedgerStoreError>;
}

#[async_trait]
pub trait SettlementPersistenceRepository: Send + Sync {
    async fn insert_settlement_plan(
        &self,
        record: &SettlementPlanRecordInsert,
    ) -> Result<(), LedgerStoreError>;

    async fn insert_settlement_record(
        &self,
        record: &SettlementRecordInsert,
    ) -> Result<(), LedgerStoreError>;

    async fn update_settlement_record_status_by_tx_hash(
        &self,
        update: &SettlementRecordStatusUpdate,
    ) -> Result<(), LedgerStoreError>;

    async fn upsert_settlement_showdown_input(
        &self,
        record: &SettlementShowdownInputRecord,
    ) -> Result<(), LedgerStoreError>;
}

#[async_trait]
pub trait SettlementPersistenceReadRepository: Send + Sync {
    async fn get_settlement_plan(
        &self,
        settlement_plan_id: Uuid,
    ) -> Result<Option<SettlementPlanRecordInsert>, LedgerStoreError>;

    async fn list_settlement_records_by_hand(
        &self,
        hand_id: HandId,
    ) -> Result<Vec<SettlementRecordInsert>, LedgerStoreError>;

    async fn get_settlement_showdown_input(
        &self,
        hand_id: HandId,
    ) -> Result<Option<SettlementShowdownInputRecord>, LedgerStoreError>;
}

#[derive(Debug, Default)]
pub struct NoopLedgerRepository;

#[async_trait]
impl LedgerRepository for NoopLedgerRepository {
    async fn insert_entries(&self, _entries: &[LedgerEntryInsert]) -> Result<(), LedgerStoreError> {
        Ok(())
    }
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryLedgerRepository {
    entries: Arc<Mutex<Vec<LedgerEntryInsert>>>,
}

impl InMemoryLedgerRepository {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries_snapshot(&self) -> Vec<LedgerEntryInsert> {
        self.entries
            .lock()
            .map_or_else(|_| Vec::new(), |entries| entries.clone())
    }
}

#[async_trait]
impl LedgerRepository for InMemoryLedgerRepository {
    async fn insert_entries(&self, entries: &[LedgerEntryInsert]) -> Result<(), LedgerStoreError> {
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?;
        guard.extend_from_slice(entries);
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct NoopChainTxRepository;

#[async_trait]
impl ChainTxRepository for NoopChainTxRepository {
    async fn upsert_tx_verification(
        &self,
        _record: &TxVerificationUpsert,
    ) -> Result<(), LedgerStoreError> {
        Ok(())
    }

    async fn insert_tx_binding(&self, _record: &TxBindingInsert) -> Result<(), LedgerStoreError> {
        Ok(())
    }

    async fn upsert_exception_credit(
        &self,
        _record: &ExceptionCreditUpsert,
    ) -> Result<(), LedgerStoreError> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct NoopSettlementPersistenceRepository;

#[async_trait]
impl SettlementPersistenceRepository for NoopSettlementPersistenceRepository {
    async fn insert_settlement_plan(
        &self,
        _record: &SettlementPlanRecordInsert,
    ) -> Result<(), LedgerStoreError> {
        Ok(())
    }

    async fn insert_settlement_record(
        &self,
        _record: &SettlementRecordInsert,
    ) -> Result<(), LedgerStoreError> {
        Ok(())
    }

    async fn update_settlement_record_status_by_tx_hash(
        &self,
        _update: &SettlementRecordStatusUpdate,
    ) -> Result<(), LedgerStoreError> {
        Ok(())
    }

    async fn upsert_settlement_showdown_input(
        &self,
        _record: &SettlementShowdownInputRecord,
    ) -> Result<(), LedgerStoreError> {
        Ok(())
    }
}

#[async_trait]
impl SettlementPersistenceReadRepository for NoopSettlementPersistenceRepository {
    async fn get_settlement_plan(
        &self,
        _settlement_plan_id: Uuid,
    ) -> Result<Option<SettlementPlanRecordInsert>, LedgerStoreError> {
        Ok(None)
    }

    async fn list_settlement_records_by_hand(
        &self,
        _hand_id: HandId,
    ) -> Result<Vec<SettlementRecordInsert>, LedgerStoreError> {
        Ok(Vec::new())
    }

    async fn get_settlement_showdown_input(
        &self,
        _hand_id: HandId,
    ) -> Result<Option<SettlementShowdownInputRecord>, LedgerStoreError> {
        Ok(None)
    }
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryChainTxRepository {
    tx_verifications: Arc<Mutex<HashMap<String, TxVerificationUpsert>>>,
    tx_bindings_by_hash: Arc<Mutex<HashMap<String, TxBindingInsert>>>,
    tx_bindings_by_action: Arc<Mutex<HashMap<(HandId, u32), String>>>,
    exception_credits: Arc<Mutex<HashMap<String, ExceptionCreditUpsert>>>,
    room_signing_keys: Arc<Mutex<HashMap<RoomId, RoomSigningKeyRecord>>>,
}

#[derive(Debug, Default, Clone)]
pub struct InMemorySettlementPersistenceRepository {
    settlement_plans: Arc<Mutex<HashMap<Uuid, SettlementPlanRecordInsert>>>,
    settlement_records: Arc<Mutex<HashMap<Uuid, SettlementRecordInsert>>>,
    showdown_inputs: Arc<Mutex<HashMap<HandId, SettlementShowdownInputRecord>>>,
}

#[derive(Debug, Clone)]
pub struct PostgresLedgerRepository {
    pool: PgPool,
}

impl PostgresLedgerRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(Debug, Clone)]
pub struct PostgresChainTxRepository {
    pool: PgPool,
    chain_id: String,
}

impl PostgresChainTxRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            chain_id: "cfx-espace".to_string(),
        }
    }

    #[must_use]
    pub fn with_chain_id(mut self, chain_id: impl Into<String>) -> Self {
        self.chain_id = chain_id.into();
        self
    }
}

fn chips_to_decimal_string(amount: Chips) -> String {
    amount.0.to_string()
}

fn parse_numeric_to_chips(s: &str) -> Option<Chips> {
    s.parse::<u128>().ok().map(Chips)
}

impl InMemoryChainTxRepository {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tx_verifications_len(&self) -> usize {
        self.tx_verifications.lock().map_or(0, |m| m.len())
    }

    pub fn tx_bindings_len(&self) -> usize {
        self.tx_bindings_by_hash.lock().map_or(0, |m| m.len())
    }

    pub fn exception_credits_len(&self) -> usize {
        self.exception_credits.lock().map_or(0, |m| m.len())
    }

    pub fn upsert_room_signing_key_record(
        &self,
        record: RoomSigningKeyRecord,
    ) -> Result<(), LedgerStoreError> {
        self.room_signing_keys
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?
            .insert(record.room_id, record);
        Ok(())
    }
}

impl InMemorySettlementPersistenceRepository {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn settlement_plans_len(&self) -> usize {
        self.settlement_plans.lock().map_or(0, |m| m.len())
    }

    pub fn settlement_records_len(&self) -> usize {
        self.settlement_records.lock().map_or(0, |m| m.len())
    }

    pub fn settlement_records_snapshot(&self) -> Vec<SettlementRecordInsert> {
        self.settlement_records
            .lock()
            .map_or_else(|_| Vec::new(), |m| m.values().cloned().collect())
    }
}

#[async_trait]
impl ChainTxRepository for InMemoryChainTxRepository {
    async fn upsert_tx_verification(
        &self,
        record: &TxVerificationUpsert,
    ) -> Result<(), LedgerStoreError> {
        self.tx_verifications
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?
            .insert(record.tx_hash.clone(), record.clone());
        Ok(())
    }

    async fn insert_tx_binding(&self, record: &TxBindingInsert) -> Result<(), LedgerStoreError> {
        {
            let by_action = self
                .tx_bindings_by_action
                .lock()
                .map_err(|_| LedgerStoreError::LockPoisoned)?;
            if by_action.contains_key(&(record.hand_id, record.action_seq)) {
                return Err(LedgerStoreError::DuplicateTxBinding);
            }
        }
        {
            let by_hash = self
                .tx_bindings_by_hash
                .lock()
                .map_err(|_| LedgerStoreError::LockPoisoned)?;
            if by_hash.contains_key(&record.tx_hash) {
                return Err(LedgerStoreError::DuplicateTxHashBinding);
            }
        }

        self.tx_bindings_by_action
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?
            .insert((record.hand_id, record.action_seq), record.tx_hash.clone());
        self.tx_bindings_by_hash
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?
            .insert(record.tx_hash.clone(), record.clone());
        Ok(())
    }

    async fn upsert_exception_credit(
        &self,
        record: &ExceptionCreditUpsert,
    ) -> Result<(), LedgerStoreError> {
        self.exception_credits
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?
            .insert(record.tx_hash.clone(), record.clone());
        Ok(())
    }
}

#[async_trait]
impl ChainTxReadRepository for InMemoryChainTxRepository {
    async fn get_tx_verification(
        &self,
        tx_hash: &str,
    ) -> Result<Option<TxVerificationRecord>, LedgerStoreError> {
        let guard = self
            .tx_verifications
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?;
        Ok(guard.get(tx_hash).map(|r| TxVerificationRecord {
            tx_hash: r.tx_hash.clone(),
            chain_id: "cfx-espace".to_string(),
            from_address: r.observed_from.clone(),
            to_address: r.observed_to.clone(),
            value: r.observed_amount,
            tx_status: r.status.clone(),
            confirmations: r.confirmations,
            verification_status: r.status.clone(),
            failure_reason: r.failure_reason.clone(),
            verified_at: Some(r.verified_at),
        }))
    }

    async fn get_tx_binding_by_tx_hash(
        &self,
        tx_hash: &str,
    ) -> Result<Option<TxBindingRecord>, LedgerStoreError> {
        let by_hash = self
            .tx_bindings_by_hash
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?;
        Ok(by_hash.get(tx_hash).and_then(|r| {
            r.expected_amount.map(|amt| TxBindingRecord {
                room_id: r.room_id,
                hand_id: r.hand_id,
                action_seq: r.action_seq,
                seat_id: r.seat_id,
                expected_amount: amt,
                tx_hash: r.tx_hash.clone(),
                binding_status: r.binding_status.clone(),
                bound_at: r.created_at,
            })
        }))
    }

    async fn get_tx_binding_by_hand_action(
        &self,
        hand_id: HandId,
        action_seq: u32,
    ) -> Result<Option<TxBindingRecord>, LedgerStoreError> {
        let by_action = self
            .tx_bindings_by_action
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?;
        let Some(tx_hash) = by_action.get(&(hand_id, action_seq)).cloned() else {
            return Ok(None);
        };
        let by_hash = self
            .tx_bindings_by_hash
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?;
        Ok(by_hash.get(&tx_hash).and_then(|r| {
            r.expected_amount.map(|amt| TxBindingRecord {
                room_id: r.room_id,
                hand_id: r.hand_id,
                action_seq: r.action_seq,
                seat_id: r.seat_id,
                expected_amount: amt,
                tx_hash: r.tx_hash.clone(),
                binding_status: r.binding_status.clone(),
                bound_at: r.created_at,
            })
        }))
    }

    async fn list_exception_credits(
        &self,
        room_id: Option<RoomId>,
        page: PageRequest,
    ) -> Result<Vec<ExceptionCreditRecord>, LedgerStoreError> {
        let guard = self
            .exception_credits
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?;
        Ok(guard
            .values()
            .filter(|r| room_id.is_none_or(|id| r.room_id == id))
            .skip(page.offset)
            .take(page.limit)
            .map(|r| ExceptionCreditRecord {
                room_id: r.room_id,
                seat_id: r.seat_id.unwrap_or(0),
                hand_id: None,
                tx_hash: Some(r.tx_hash.clone()),
                credit_kind: r.credit_type.clone(),
                amount: r.amount,
                status: r.status.clone(),
                reason: Some(r.reason.clone()),
                created_at: r.updated_at,
                updated_at: r.updated_at,
            })
            .collect())
    }
}

#[async_trait]
impl RoomSigningKeyReadRepository for InMemoryChainTxRepository {
    async fn get_active_room_signing_key(
        &self,
        room_id: RoomId,
    ) -> Result<Option<RoomSigningKeyRecord>, LedgerStoreError> {
        let guard = self
            .room_signing_keys
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?;
        Ok(guard.get(&room_id).cloned())
    }
}

#[async_trait]
impl SettlementPersistenceRepository for InMemorySettlementPersistenceRepository {
    async fn insert_settlement_plan(
        &self,
        record: &SettlementPlanRecordInsert,
    ) -> Result<(), LedgerStoreError> {
        self.settlement_plans
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?
            .insert(record.settlement_plan_id, record.clone());
        Ok(())
    }

    async fn insert_settlement_record(
        &self,
        record: &SettlementRecordInsert,
    ) -> Result<(), LedgerStoreError> {
        self.settlement_records
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?
            .insert(record.settlement_record_id, record.clone());
        Ok(())
    }

    async fn update_settlement_record_status_by_tx_hash(
        &self,
        update: &SettlementRecordStatusUpdate,
    ) -> Result<(), LedgerStoreError> {
        let mut guard = self
            .settlement_records
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?;
        for record in guard.values_mut() {
            if record.tx_hash.as_deref() == Some(update.tx_hash.as_str()) {
                record.settlement_status = update.settlement_status.clone();
                record.retry_count = update.retry_count;
                record.error_detail = update.error_detail.clone();
                record.updated_at = update.updated_at;
            }
        }
        Ok(())
    }

    async fn upsert_settlement_showdown_input(
        &self,
        record: &SettlementShowdownInputRecord,
    ) -> Result<(), LedgerStoreError> {
        self.showdown_inputs
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?
            .insert(record.hand_id, record.clone());
        Ok(())
    }
}

#[async_trait]
impl SettlementPersistenceReadRepository for InMemorySettlementPersistenceRepository {
    async fn get_settlement_plan(
        &self,
        settlement_plan_id: Uuid,
    ) -> Result<Option<SettlementPlanRecordInsert>, LedgerStoreError> {
        let guard = self
            .settlement_plans
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?;
        Ok(guard.get(&settlement_plan_id).cloned())
    }

    async fn list_settlement_records_by_hand(
        &self,
        hand_id: HandId,
    ) -> Result<Vec<SettlementRecordInsert>, LedgerStoreError> {
        let guard = self
            .settlement_records
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?;
        let mut out: Vec<_> = guard
            .values()
            .filter(|r| r.hand_id == hand_id)
            .cloned()
            .collect();
        out.sort_by_key(|r| r.created_at);
        Ok(out)
    }

    async fn get_settlement_showdown_input(
        &self,
        hand_id: HandId,
    ) -> Result<Option<SettlementShowdownInputRecord>, LedgerStoreError> {
        let guard = self
            .showdown_inputs
            .lock()
            .map_err(|_| LedgerStoreError::LockPoisoned)?;
        Ok(guard.get(&hand_id).cloned())
    }
}

#[async_trait]
impl LedgerRepository for PostgresLedgerRepository {
    async fn insert_entries(&self, entries: &[LedgerEntryInsert]) -> Result<(), LedgerStoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
        for entry in entries {
            let amount = chips_to_decimal_string(entry.amount);
            sqlx::query(
                r#"
                INSERT INTO ledger_entries (
                    ledger_entry_id, entry_type, room_id, hand_id, seat_id, agent_id,
                    asset_type, amount, direction, account_scope, related_tx_hash,
                    related_event_id, related_attempt_id, remark, trace_id
                ) VALUES (
                    $1, $2, $3, $4, $5, $6,
                    $7, CAST($8 AS NUMERIC), $9, $10, $11,
                    $12, $13, $14, $15
                )
                "#,
            )
            .bind(Uuid::now_v7())
            .bind(&entry.entry_type)
            .bind(entry.room_id.map(|v| v.0))
            .bind(entry.hand_id.map(|v| v.0))
            .bind(entry.seat_id.map(i16::from))
            .bind(entry.agent_id.map(|v| v.0))
            .bind(&entry.asset_type)
            .bind(amount)
            .bind(match entry.direction {
                LedgerDirection::Debit => "debit",
                LedgerDirection::Credit => "credit",
            })
            .bind(&entry.account_scope)
            .bind(&entry.related_tx_hash)
            .bind(
                entry
                    .related_event_id
                    .as_deref()
                    .and_then(|v| Uuid::parse_str(v).ok()),
            )
            .bind(
                entry
                    .related_attempt_id
                    .as_deref()
                    .and_then(|v| Uuid::parse_str(v).ok()),
            )
            .bind(&entry.remark)
            .bind(entry.trace_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl ChainTxRepository for PostgresChainTxRepository {
    async fn upsert_tx_verification(
        &self,
        record: &TxVerificationUpsert,
    ) -> Result<(), LedgerStoreError> {
        let value = record.observed_amount.map(chips_to_decimal_string);
        sqlx::query(
            r#"
            INSERT INTO tx_verifications (
                tx_verification_id, tx_hash, chain_id, from_address, to_address, value,
                tx_status, confirmations, verification_status, failure_reason, verified_at
            ) VALUES (
                $1, $2, $3, $4, $5, CAST($6 AS NUMERIC),
                $7, $8, $9, $10, $11
            )
            ON CONFLICT (tx_hash) DO UPDATE SET
                chain_id = EXCLUDED.chain_id,
                from_address = EXCLUDED.from_address,
                to_address = EXCLUDED.to_address,
                value = EXCLUDED.value,
                tx_status = EXCLUDED.tx_status,
                confirmations = EXCLUDED.confirmations,
                verification_status = EXCLUDED.verification_status,
                failure_reason = EXCLUDED.failure_reason,
                verified_at = EXCLUDED.verified_at
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(&record.tx_hash)
        .bind(&self.chain_id)
        .bind(&record.observed_from)
        .bind(&record.observed_to)
        .bind(value.unwrap_or_else(|| "0".to_string()))
        .bind(&record.status)
        .bind(i64::try_from(record.confirmations).unwrap_or(i64::MAX))
        .bind(&record.status)
        .bind(&record.failure_reason)
        .bind(record.verified_at)
        .execute(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn insert_tx_binding(&self, record: &TxBindingInsert) -> Result<(), LedgerStoreError> {
        let expected_amount = record
            .expected_amount
            .ok_or(LedgerStoreError::MissingExpectedAmount)?;
        let amount = chips_to_decimal_string(expected_amount);
        let res = sqlx::query(
            r#"
            INSERT INTO tx_bindings (
                tx_binding_id, room_id, hand_id, action_seq, seat_id,
                expected_amount, tx_hash, binding_status, bound_at
            ) VALUES (
                $1, $2, $3, $4, $5,
                CAST($6 AS NUMERIC), $7, $8, $9
            )
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(record.room_id.0)
        .bind(record.hand_id.0)
        .bind(i32::try_from(record.action_seq).unwrap_or(i32::MAX))
        .bind(i16::from(record.seat_id))
        .bind(amount)
        .bind(&record.tx_hash)
        .bind(&record.binding_status)
        .bind(record.created_at)
        .execute(&self.pool)
        .await;

        match res {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("uq_tx_bindings_hand_action_seq") {
                    Err(LedgerStoreError::DuplicateTxBinding)
                } else if msg.contains("uq_tx_bindings_tx_hash") {
                    Err(LedgerStoreError::DuplicateTxHashBinding)
                } else {
                    Err(LedgerStoreError::Database(msg))
                }
            }
        }
    }

    async fn upsert_exception_credit(
        &self,
        record: &ExceptionCreditUpsert,
    ) -> Result<(), LedgerStoreError> {
        let amount = chips_to_decimal_string(record.amount);
        sqlx::query(
            r#"
            INSERT INTO exception_credits (
                exception_credit_id, room_id, seat_id, hand_id, tx_hash,
                credit_kind, amount, status, reason, created_at, updated_at
            ) VALUES (
                $1, $2, $3, NULL, $4,
                $5, CAST($6 AS NUMERIC), $7, $8, $9, $9
            )
            ON CONFLICT (tx_hash) DO UPDATE SET
                room_id = EXCLUDED.room_id,
                seat_id = EXCLUDED.seat_id,
                credit_kind = EXCLUDED.credit_kind,
                amount = EXCLUDED.amount,
                status = EXCLUDED.status,
                reason = EXCLUDED.reason,
                updated_at = EXCLUDED.updated_at
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(record.room_id.0)
        .bind(i16::from(record.seat_id.unwrap_or(0)))
        .bind(&record.tx_hash)
        .bind(&record.credit_type)
        .bind(amount)
        .bind(&record.status)
        .bind(&record.reason)
        .bind(record.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl LedgerReadRepository for PostgresLedgerRepository {
    async fn list_ledger_entries_by_room_hand(
        &self,
        room_id: Option<RoomId>,
        hand_id: Option<HandId>,
        page: PageRequest,
    ) -> Result<Vec<LedgerEntryInsert>, LedgerStoreError> {
        let rows = sqlx::query(
            r#"
            SELECT entry_type, room_id, hand_id, seat_id, agent_id, asset_type, amount::text AS amount_text,
                   direction, account_scope, related_tx_hash, related_event_id, related_attempt_id, remark, trace_id
            FROM ledger_entries
            WHERE ($1::uuid IS NULL OR room_id = $1)
              AND ($2::uuid IS NULL OR hand_id = $2)
            ORDER BY created_at DESC
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(room_id.map(|v| v.0))
        .bind(hand_id.map(|v| v.0))
        .bind(i64::try_from(page.limit).unwrap_or(i64::MAX))
        .bind(i64::try_from(page.offset).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;

        rows.into_iter()
            .map(|row| {
                let amount_text: String = row
                    .try_get("amount_text")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
                Ok(LedgerEntryInsert {
                    entry_type: row
                        .try_get("entry_type")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    room_id: row
                        .try_get::<Option<Uuid>, _>("room_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?
                        .map(RoomId),
                    hand_id: row
                        .try_get::<Option<Uuid>, _>("hand_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?
                        .map(HandId),
                    seat_id: row
                        .try_get::<Option<i16>, _>("seat_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?
                        .and_then(|v| u8::try_from(v).ok()),
                    agent_id: row
                        .try_get::<Option<Uuid>, _>("agent_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?
                        .map(AgentId),
                    asset_type: row
                        .try_get("asset_type")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    amount: parse_numeric_to_chips(&amount_text).unwrap_or(Chips::ZERO),
                    direction: match row
                        .try_get::<String, _>("direction")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?
                        .as_str()
                    {
                        "debit" => LedgerDirection::Debit,
                        _ => LedgerDirection::Credit,
                    },
                    account_scope: row
                        .try_get("account_scope")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    related_tx_hash: row
                        .try_get("related_tx_hash")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    related_event_id: row
                        .try_get::<Option<Uuid>, _>("related_event_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?
                        .map(|v| v.to_string()),
                    related_attempt_id: row
                        .try_get::<Option<Uuid>, _>("related_attempt_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?
                        .map(|v| v.to_string()),
                    remark: row
                        .try_get("remark")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    trace_id: TraceId(
                        row.try_get("trace_id")
                            .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    ),
                })
            })
            .collect()
    }
}

#[async_trait]
impl ChainTxReadRepository for PostgresChainTxRepository {
    async fn get_tx_verification(
        &self,
        tx_hash: &str,
    ) -> Result<Option<TxVerificationRecord>, LedgerStoreError> {
        let row = sqlx::query(
            r#"
            SELECT tx_hash, chain_id, from_address, to_address, value::text AS value_text,
                   tx_status, confirmations, verification_status, failure_reason, verified_at
            FROM tx_verifications WHERE tx_hash = $1 LIMIT 1
            "#,
        )
        .bind(tx_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
        row.map(|row| {
            Ok(TxVerificationRecord {
                tx_hash: row
                    .try_get("tx_hash")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                chain_id: row
                    .try_get("chain_id")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                from_address: row
                    .try_get("from_address")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                to_address: row
                    .try_get("to_address")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                value: row
                    .try_get::<Option<String>, _>("value_text")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?
                    .and_then(|s| parse_numeric_to_chips(&s)),
                tx_status: row
                    .try_get("tx_status")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                confirmations: u64::try_from(
                    row.try_get::<i64, _>("confirmations")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                )
                .unwrap_or_default(),
                verification_status: row
                    .try_get("verification_status")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                failure_reason: row
                    .try_get("failure_reason")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                verified_at: row
                    .try_get("verified_at")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
            })
        })
        .transpose()
    }

    async fn get_tx_binding_by_tx_hash(
        &self,
        tx_hash: &str,
    ) -> Result<Option<TxBindingRecord>, LedgerStoreError> {
        let row = sqlx::query(
            r#"
            SELECT room_id, hand_id, action_seq, seat_id, expected_amount::text AS expected_amount_text,
                   tx_hash, binding_status, bound_at
            FROM tx_bindings
            WHERE tx_hash = $1
            LIMIT 1
            "#,
        )
        .bind(tx_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
        row.map(|row| {
            let expected_amount_text: String = row
                .try_get("expected_amount_text")
                .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
            Ok(TxBindingRecord {
                room_id: RoomId(
                    row.try_get("room_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                ),
                hand_id: HandId(
                    row.try_get("hand_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                ),
                action_seq: u32::try_from(
                    row.try_get::<i32, _>("action_seq")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                )
                .unwrap_or_default(),
                seat_id: u8::try_from(
                    row.try_get::<i16, _>("seat_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                )
                .unwrap_or_default(),
                expected_amount: parse_numeric_to_chips(&expected_amount_text)
                    .unwrap_or(Chips::ZERO),
                tx_hash: row
                    .try_get("tx_hash")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                binding_status: row
                    .try_get("binding_status")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                bound_at: row
                    .try_get("bound_at")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
            })
        })
        .transpose()
    }

    async fn get_tx_binding_by_hand_action(
        &self,
        hand_id: HandId,
        action_seq: u32,
    ) -> Result<Option<TxBindingRecord>, LedgerStoreError> {
        let row = sqlx::query(
            r#"
            SELECT room_id, hand_id, action_seq, seat_id, expected_amount::text AS expected_amount_text,
                   tx_hash, binding_status, bound_at
            FROM tx_bindings
            WHERE hand_id = $1 AND action_seq = $2
            LIMIT 1
            "#,
        )
        .bind(hand_id.0)
        .bind(i32::try_from(action_seq).unwrap_or(i32::MAX))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
        row.map(|row| {
            let expected_amount_text: String = row
                .try_get("expected_amount_text")
                .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
            Ok(TxBindingRecord {
                room_id: RoomId(
                    row.try_get("room_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                ),
                hand_id: HandId(
                    row.try_get("hand_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                ),
                action_seq: u32::try_from(
                    row.try_get::<i32, _>("action_seq")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                )
                .unwrap_or_default(),
                seat_id: u8::try_from(
                    row.try_get::<i16, _>("seat_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                )
                .unwrap_or_default(),
                expected_amount: parse_numeric_to_chips(&expected_amount_text)
                    .unwrap_or(Chips::ZERO),
                tx_hash: row
                    .try_get("tx_hash")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                binding_status: row
                    .try_get("binding_status")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                bound_at: row
                    .try_get("bound_at")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
            })
        })
        .transpose()
    }

    async fn list_exception_credits(
        &self,
        room_id: Option<RoomId>,
        page: PageRequest,
    ) -> Result<Vec<ExceptionCreditRecord>, LedgerStoreError> {
        let rows = sqlx::query(
            r#"
            SELECT room_id, seat_id, hand_id, tx_hash, credit_kind, amount::text AS amount_text,
                   status, reason, created_at, updated_at
            FROM exception_credits
            WHERE ($1::uuid IS NULL OR room_id = $1)
            ORDER BY updated_at DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(room_id.map(|v| v.0))
        .bind(i64::try_from(page.limit).unwrap_or(i64::MAX))
        .bind(i64::try_from(page.offset).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
        rows.into_iter()
            .map(|row| {
                let amount_text: String = row
                    .try_get("amount_text")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
                Ok(ExceptionCreditRecord {
                    room_id: RoomId(
                        row.try_get("room_id")
                            .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    ),
                    seat_id: u8::try_from(
                        row.try_get::<i16, _>("seat_id")
                            .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    )
                    .unwrap_or_default(),
                    hand_id: row
                        .try_get::<Option<Uuid>, _>("hand_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?
                        .map(HandId),
                    tx_hash: row
                        .try_get("tx_hash")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    credit_kind: row
                        .try_get("credit_kind")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    amount: parse_numeric_to_chips(&amount_text).unwrap_or(Chips::ZERO),
                    status: row
                        .try_get("status")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    reason: row
                        .try_get("reason")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    created_at: row
                        .try_get("created_at")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    updated_at: row
                        .try_get("updated_at")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                })
            })
            .collect()
    }
}

#[async_trait]
impl RoomSigningKeyReadRepository for PostgresChainTxRepository {
    async fn get_active_room_signing_key(
        &self,
        room_id: RoomId,
    ) -> Result<Option<RoomSigningKeyRecord>, LedgerStoreError> {
        let row = sqlx::query(
            r#"
            SELECT room_id, address, encrypted_private_key, cipher_alg, key_version, kek_id, nonce, aad,
                   status, created_at, rotated_at, revoked_at
            FROM room_signing_keys
            WHERE room_id = $1 AND status = 'active'
            ORDER BY key_version DESC
            LIMIT 1
            "#,
        )
        .bind(room_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;

        row.map(|row| {
            Ok(RoomSigningKeyRecord {
                room_id: RoomId(
                    row.try_get("room_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                ),
                address: row
                    .try_get("address")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                encrypted_private_key: row
                    .try_get("encrypted_private_key")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                cipher_alg: row
                    .try_get("cipher_alg")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                key_version: row
                    .try_get("key_version")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                kek_id: row
                    .try_get("kek_id")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                nonce: row
                    .try_get("nonce")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                aad: row
                    .try_get("aad")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                status: row
                    .try_get("status")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                created_at: row
                    .try_get("created_at")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                rotated_at: row
                    .try_get("rotated_at")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                revoked_at: row
                    .try_get("revoked_at")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
            })
        })
        .transpose()
    }
}

#[async_trait]
impl SettlementPersistenceRepository for PostgresChainTxRepository {
    async fn insert_settlement_plan(
        &self,
        record: &SettlementPlanRecordInsert,
    ) -> Result<(), LedgerStoreError> {
        sqlx::query(
            r#"
            INSERT INTO settlement_plans (
                settlement_plan_id, room_id, hand_id, status, rake_amount, payout_total,
                payload_json, created_at, finalized_at
            ) VALUES (
                $1, $2, $3, $4, CAST($5 AS NUMERIC), CAST($6 AS NUMERIC),
                $7, $8, $9
            )
            "#,
        )
        .bind(record.settlement_plan_id)
        .bind(record.room_id.0)
        .bind(record.hand_id.0)
        .bind(&record.status)
        .bind(chips_to_decimal_string(record.rake_amount))
        .bind(chips_to_decimal_string(record.payout_total))
        .bind(Json(record.payload_json.clone()))
        .bind(record.created_at)
        .bind(record.finalized_at)
        .execute(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn insert_settlement_record(
        &self,
        record: &SettlementRecordInsert,
    ) -> Result<(), LedgerStoreError> {
        sqlx::query(
            r#"
            INSERT INTO settlement_records (
                settlement_record_id, settlement_plan_id, room_id, hand_id, tx_hash,
                settlement_status, retry_count, error_detail, created_at, updated_at
            ) VALUES (
                $1, $2, $3, $4, $5,
                $6, $7, $8, $9, $10
            )
            "#,
        )
        .bind(record.settlement_record_id)
        .bind(record.settlement_plan_id)
        .bind(record.room_id.0)
        .bind(record.hand_id.0)
        .bind(&record.tx_hash)
        .bind(&record.settlement_status)
        .bind(record.retry_count)
        .bind(&record.error_detail)
        .bind(record.created_at)
        .bind(record.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn update_settlement_record_status_by_tx_hash(
        &self,
        update: &SettlementRecordStatusUpdate,
    ) -> Result<(), LedgerStoreError> {
        sqlx::query(
            r#"
            UPDATE settlement_records
            SET settlement_status = $1,
                retry_count = $2,
                error_detail = $3,
                updated_at = $4
            WHERE tx_hash = $5
            "#,
        )
        .bind(&update.settlement_status)
        .bind(update.retry_count)
        .bind(&update.error_detail)
        .bind(update.updated_at)
        .bind(&update.tx_hash)
        .execute(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn upsert_settlement_showdown_input(
        &self,
        record: &SettlementShowdownInputRecord,
    ) -> Result<(), LedgerStoreError> {
        sqlx::query(
            r#"
            INSERT INTO settlement_showdown_inputs (
                settlement_showdown_input_id, room_id, hand_id, board_cards,
                revealed_hole_cards_json, source, updated_at, trace_id
            ) VALUES (
                $1, $2, $3, $4,
                $5, $6, $7, $8
            )
            ON CONFLICT (hand_id) DO UPDATE SET
                room_id = EXCLUDED.room_id,
                board_cards = EXCLUDED.board_cards,
                revealed_hole_cards_json = EXCLUDED.revealed_hole_cards_json,
                source = EXCLUDED.source,
                updated_at = EXCLUDED.updated_at,
                trace_id = EXCLUDED.trace_id
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(record.room_id.0)
        .bind(record.hand_id.0)
        .bind(&record.board_cards)
        .bind(Json(record.revealed_hole_cards.clone()))
        .bind(&record.source)
        .bind(record.updated_at)
        .bind(record.trace_id.0)
        .execute(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl SettlementPersistenceReadRepository for PostgresChainTxRepository {
    async fn get_settlement_plan(
        &self,
        settlement_plan_id: Uuid,
    ) -> Result<Option<SettlementPlanRecordInsert>, LedgerStoreError> {
        let row = sqlx::query(
            r#"
            SELECT settlement_plan_id, room_id, hand_id, status, rake_amount::text AS rake_amount_text,
                   payout_total::text AS payout_total_text, payload_json, created_at, finalized_at
            FROM settlement_plans
            WHERE settlement_plan_id = $1
            LIMIT 1
            "#,
        )
        .bind(settlement_plan_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;

        row.map(|row| {
            let rake_amount_text: String = row
                .try_get("rake_amount_text")
                .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
            let payout_total_text: String = row
                .try_get("payout_total_text")
                .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
            let payload: Json<JsonValue> = row
                .try_get("payload_json")
                .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
            Ok(SettlementPlanRecordInsert {
                settlement_plan_id: row
                    .try_get("settlement_plan_id")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                room_id: RoomId(
                    row.try_get("room_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                ),
                hand_id: HandId(
                    row.try_get("hand_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                ),
                status: row
                    .try_get("status")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                rake_amount: parse_numeric_to_chips(&rake_amount_text).unwrap_or(Chips::ZERO),
                payout_total: parse_numeric_to_chips(&payout_total_text).unwrap_or(Chips::ZERO),
                payload_json: payload.0,
                created_at: row
                    .try_get("created_at")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                finalized_at: row
                    .try_get("finalized_at")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
            })
        })
        .transpose()
    }

    async fn list_settlement_records_by_hand(
        &self,
        hand_id: HandId,
    ) -> Result<Vec<SettlementRecordInsert>, LedgerStoreError> {
        let rows = sqlx::query(
            r#"
            SELECT settlement_record_id, settlement_plan_id, room_id, hand_id, tx_hash,
                   settlement_status, retry_count, error_detail, created_at, updated_at
            FROM settlement_records
            WHERE hand_id = $1
            ORDER BY created_at ASC
            "#,
        )
        .bind(hand_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;

        rows.into_iter()
            .map(|row| {
                Ok(SettlementRecordInsert {
                    settlement_record_id: row
                        .try_get("settlement_record_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    settlement_plan_id: row
                        .try_get("settlement_plan_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    room_id: RoomId(
                        row.try_get("room_id")
                            .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    ),
                    hand_id: HandId(
                        row.try_get("hand_id")
                            .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    ),
                    tx_hash: row
                        .try_get("tx_hash")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    settlement_status: row
                        .try_get("settlement_status")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    retry_count: row
                        .try_get("retry_count")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    error_detail: row
                        .try_get("error_detail")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    created_at: row
                        .try_get("created_at")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                    updated_at: row
                        .try_get("updated_at")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                })
            })
            .collect()
    }

    async fn get_settlement_showdown_input(
        &self,
        hand_id: HandId,
    ) -> Result<Option<SettlementShowdownInputRecord>, LedgerStoreError> {
        let row = sqlx::query(
            r#"
            SELECT room_id, hand_id, board_cards, revealed_hole_cards_json, source, updated_at, trace_id
            FROM settlement_showdown_inputs
            WHERE hand_id = $1
            LIMIT 1
            "#,
        )
        .bind(hand_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| LedgerStoreError::Database(e.to_string()))?;

        row.map(|row| {
            let revealed: Json<Vec<SettlementShowdownSeatCardsRecord>> = row
                .try_get("revealed_hole_cards_json")
                .map_err(|e| LedgerStoreError::Database(e.to_string()))?;
            Ok(SettlementShowdownInputRecord {
                room_id: RoomId(
                    row.try_get("room_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                ),
                hand_id: HandId(
                    row.try_get("hand_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                ),
                board_cards: row
                    .try_get("board_cards")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                revealed_hole_cards: revealed.0,
                source: row
                    .try_get("source")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                updated_at: row
                    .try_get("updated_at")
                    .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                trace_id: TraceId(
                    row.try_get("trace_id")
                        .map_err(|e| LedgerStoreError::Database(e.to_string()))?,
                ),
            })
        })
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn in_memory_chain_repo_upserts_verification_and_credit() {
        let repo = InMemoryChainTxRepository::new();
        let room_id = RoomId::new();
        let tx_hash = "0xabc".to_string();

        repo.upsert_tx_verification(&TxVerificationUpsert {
            tx_hash: tx_hash.clone(),
            room_id,
            hand_id: None,
            seat_id: Some(1),
            action_seq: Some(2),
            status: "matched".to_string(),
            confirmations: 3,
            failure_reason: None,
            observed_from: Some("from".to_string()),
            observed_to: Some("to".to_string()),
            observed_amount: Some(Chips(10)),
            verified_at: Utc::now(),
            trace_id: TraceId::new(),
        })
        .await
        .expect("upsert");

        repo.upsert_exception_credit(&ExceptionCreditUpsert {
            room_id,
            seat_id: Some(1),
            tx_hash,
            credit_type: "unmatched".to_string(),
            amount: Chips(10),
            status: "open".to_string(),
            reason: "value_mismatch".to_string(),
            updated_at: Utc::now(),
            trace_id: TraceId::new(),
        })
        .await
        .expect("upsert credit");

        assert_eq!(repo.tx_verifications_len(), 1);
        assert_eq!(repo.exception_credits_len(), 1);
    }

    #[tokio::test]
    async fn in_memory_chain_repo_rejects_duplicate_bindings() {
        let repo = InMemoryChainTxRepository::new();
        let room_id = RoomId::new();
        let hand_id = HandId::new();
        let base = TxBindingInsert {
            tx_hash: "0xtx1".to_string(),
            room_id,
            hand_id,
            seat_id: 1,
            action_seq: 7,
            expected_amount: Some(Chips(100)),
            binding_status: "submitted".to_string(),
            created_at: Utc::now(),
            trace_id: TraceId::new(),
        };

        repo.insert_tx_binding(&base).await.expect("insert");
        let err = repo
            .insert_tx_binding(&TxBindingInsert {
                tx_hash: "0xtx2".to_string(),
                ..base.clone()
            })
            .await
            .expect_err("duplicate hand/action");
        assert!(matches!(err, LedgerStoreError::DuplicateTxBinding));
    }

    #[tokio::test]
    async fn in_memory_chain_repo_returns_active_room_signing_key() {
        let repo = InMemoryChainTxRepository::new();
        let room_id = RoomId::new();
        repo.upsert_room_signing_key_record(RoomSigningKeyRecord {
            room_id,
            address: "0xroom".to_string(),
            encrypted_private_key: vec![1, 2, 3],
            cipher_alg: "AES-256-GCM".to_string(),
            key_version: 1,
            kek_id: "kek-1".to_string(),
            nonce: vec![0; 12],
            aad: b"aad".to_vec(),
            status: "active".to_string(),
            created_at: Utc::now(),
            rotated_at: None,
            revoked_at: None,
        })
        .expect("insert");

        let record = repo
            .get_active_room_signing_key(room_id)
            .await
            .expect("query")
            .expect("record");
        assert_eq!(record.address, "0xroom");
        assert_eq!(record.cipher_alg, "AES-256-GCM");
    }

    #[tokio::test]
    async fn in_memory_settlement_persistence_repo_inserts_plan_and_record() {
        let repo = InMemorySettlementPersistenceRepository::new();
        let room_id = RoomId::new();
        let hand_id = HandId::new();
        let plan_id = Uuid::now_v7();
        repo.insert_settlement_plan(&SettlementPlanRecordInsert {
            settlement_plan_id: plan_id,
            room_id,
            hand_id,
            status: "created".to_string(),
            rake_amount: Chips(10),
            payout_total: Chips(90),
            payload_json: serde_json::json!({"payouts":[{"seat_id":1,"amount":"90"}]}),
            created_at: Utc::now(),
            finalized_at: None,
        })
        .await
        .expect("insert plan");
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
        .expect("insert record");

        assert_eq!(repo.settlement_plans_len(), 1);
        assert_eq!(repo.settlement_records_len(), 1);
    }

    #[tokio::test]
    async fn in_memory_settlement_persistence_repo_updates_record_status_by_tx_hash() {
        let repo = InMemorySettlementPersistenceRepository::new();
        let room_id = RoomId::new();
        let hand_id = HandId::new();
        let tx_hash = "0xtx".to_string();
        repo.insert_settlement_record(&SettlementRecordInsert {
            settlement_record_id: Uuid::now_v7(),
            settlement_plan_id: Uuid::now_v7(),
            room_id,
            hand_id,
            tx_hash: Some(tx_hash.clone()),
            settlement_status: "submitted".to_string(),
            retry_count: 0,
            error_detail: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
        .await
        .expect("insert");

        repo.update_settlement_record_status_by_tx_hash(&SettlementRecordStatusUpdate {
            tx_hash,
            settlement_status: "failed".to_string(),
            retry_count: 1,
            error_detail: Some("rpc timeout".to_string()),
            updated_at: Utc::now(),
        })
        .await
        .expect("update");

        let guard = repo.settlement_records.lock().expect("lock");
        let record = guard.values().next().expect("record");
        assert_eq!(record.settlement_status, "failed");
        assert_eq!(record.retry_count, 1);
        assert_eq!(record.error_detail.as_deref(), Some("rpc timeout"));
    }

    #[tokio::test]
    async fn in_memory_settlement_persistence_read_repo_queries_by_plan_and_hand() {
        let repo = InMemorySettlementPersistenceRepository::new();
        let room_id = RoomId::new();
        let hand_id = HandId::new();
        let plan_id = Uuid::now_v7();
        let plan = SettlementPlanRecordInsert {
            settlement_plan_id: plan_id,
            room_id,
            hand_id,
            status: "created".to_string(),
            rake_amount: Chips(1),
            payout_total: Chips(9),
            payload_json: serde_json::json!({"ok":true}),
            created_at: Utc::now(),
            finalized_at: None,
        };
        repo.insert_settlement_plan(&plan)
            .await
            .expect("insert plan");
        repo.insert_settlement_record(&SettlementRecordInsert {
            settlement_record_id: Uuid::now_v7(),
            settlement_plan_id: plan_id,
            room_id,
            hand_id,
            tx_hash: Some("0x1".to_string()),
            settlement_status: "submitted".to_string(),
            retry_count: 0,
            error_detail: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
        .await
        .expect("insert record");

        let got_plan = repo
            .get_settlement_plan(plan_id)
            .await
            .expect("query")
            .expect("found");
        assert_eq!(got_plan.hand_id, hand_id);

        let records = repo
            .list_settlement_records_by_hand(hand_id)
            .await
            .expect("query records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].settlement_status, "submitted");
    }
}
