use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use async_trait::async_trait;
use chrono::Utc;
use ethabi::{Function, Param, ParamType, StateMutability, Token};
use ledger_store::{
    LedgerDirection, LedgerEntryInsert, LedgerRepository, LedgerStoreError,
    RoomSigningKeyReadRepository, SettlementPersistenceRepository, SettlementRecordStatusUpdate,
};
use poker_domain::{Chips, HandId, MoneyError, RoomId, SeatId, TraceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha3::{Digest, Keccak256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum SettlementError {
    #[error("ledger error: {0}")]
    Ledger(#[from] LedgerStoreError),
    #[error("money error: {0}")]
    Money(#[from] MoneyError),
    #[error("invalid pot award: {0}")]
    InvalidPotAward(String),
    #[error("wallet adapter error: {0}")]
    WalletAdapter(String),
    #[error("key decrypt error: {0}")]
    KeyDecrypt(String),
    #[error("invalid rake policy: {0}")]
    InvalidRakePolicy(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementPayout {
    pub seat_id: SeatId,
    pub amount: Chips,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettlementPlan {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub rake_amount: Chips,
    pub payouts: Vec<SettlementPayout>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementTransferRequest {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub to_seat_id: SeatId,
    pub to_address: String,
    pub amount: Chips,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementTransferSubmission {
    pub seat_id: SeatId,
    pub tx_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettlementTxStatus {
    Pending,
    Confirmed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementTransferReceipt {
    pub tx_hash: String,
    pub status: SettlementTxStatus,
    pub confirmations: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchSettlementTransferRequest {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub transfers: Vec<SettlementTransferRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchSettlementTransferSubmission {
    pub tx_hash: String,
    pub transfer_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchSettlementTransferReceipt {
    pub tx_hash: String,
    pub status: SettlementTxStatus,
    pub confirmations: u64,
    pub applied_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedRoomSigningKey {
    pub room_id: RoomId,
    pub address: String,
    pub encrypted_private_key: Vec<u8>,
    pub cipher_alg: String,
    pub key_version: i32,
    pub kek_id: String,
    pub nonce: Vec<u8>,
    pub aad: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PotAwardInput {
    pub amount: Chips,
    pub eligible_seats: Vec<SeatId>,
    pub winner_seats: Vec<SeatId>,
}

impl SettlementPlan {
    pub fn payout_total(&self) -> Result<Chips, poker_domain::MoneyError> {
        self.payouts
            .iter()
            .try_fold(Chips::ZERO, |acc, p| acc.checked_add(p.amount))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RakePolicy {
    pub rake_bps: u16,
}

impl RakePolicy {
    #[must_use]
    pub fn zero() -> Self {
        Self { rake_bps: 0 }
    }

    #[must_use]
    pub fn three_percent() -> Self {
        Self { rake_bps: 300 }
    }

    pub fn rake_amount_for(&self, pot_total: Chips) -> Result<Chips, SettlementError> {
        if self.rake_bps > 10_000 {
            return Err(SettlementError::InvalidRakePolicy(format!(
                "rake_bps={} exceeds 10000",
                self.rake_bps
            )));
        }
        // Chain unit is integer chips, so rake always rounds down.
        Ok(Chips(
            pot_total
                .as_u128()
                .saturating_mul(self.rake_bps as u128)
                / 10_000,
        ))
    }
}

#[derive(Debug)]
pub struct SettlementService<L> {
    ledger_repo: L,
}

pub struct RoomSigningKeyHandle {
    address: String,
    key_version: i32,
    private_key_bytes: Vec<u8>,
}

impl RoomSigningKeyHandle {
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    #[must_use]
    pub fn key_version(&self) -> i32 {
        self.key_version
    }

    #[must_use]
    pub fn has_private_key_material(&self) -> bool {
        !self.private_key_bytes.is_empty()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn private_key_bytes(&self) -> &[u8] {
        &self.private_key_bytes
    }
}

#[async_trait]
pub trait SettlementWalletAdapter: Send + Sync {
    async fn submit_transfer(&self, request: &SettlementTransferRequest) -> Result<String, String>;

    async fn get_transfer_receipt(
        &self,
        tx_hash: &str,
    ) -> Result<SettlementTransferReceipt, String>;
}

#[async_trait]
pub trait BatchSettlementWalletAdapter: Send + Sync {
    async fn submit_batch_transfer(
        &self,
        request: &BatchSettlementTransferRequest,
    ) -> Result<String, String>;

    async fn get_batch_transfer_receipt(
        &self,
        tx_hash: &str,
    ) -> Result<BatchSettlementTransferReceipt, String>;
}

fn decrypt_room_signing_key_aes256_gcm(
    record: &EncryptedRoomSigningKey,
    kek: &[u8; 32],
) -> Result<Vec<u8>, SettlementError> {
    if !record.cipher_alg.eq_ignore_ascii_case("AES-256-GCM") {
        return Err(SettlementError::KeyDecrypt(format!(
            "unsupported cipher_alg {}",
            record.cipher_alg
        )));
    }
    let nonce: [u8; 12] = record
        .nonce
        .clone()
        .try_into()
        .map_err(|_| SettlementError::KeyDecrypt("invalid AES-GCM nonce length".to_string()))?;
    let cipher =
        Aes256Gcm::new_from_slice(kek).map_err(|e| SettlementError::KeyDecrypt(e.to_string()))?;
    cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &record.encrypted_private_key,
                aad: &record.aad,
            },
        )
        .map_err(|e| SettlementError::KeyDecrypt(e.to_string()))
}

fn decrypt_room_signing_key_record_aes256_gcm(
    record: &ledger_store::RoomSigningKeyRecord,
    kek: &[u8; 32],
) -> Result<Vec<u8>, SettlementError> {
    decrypt_room_signing_key_aes256_gcm(
        &EncryptedRoomSigningKey {
            room_id: record.room_id,
            address: record.address.clone(),
            encrypted_private_key: record.encrypted_private_key.clone(),
            cipher_alg: record.cipher_alg.clone(),
            key_version: record.key_version,
            kek_id: record.kek_id.clone(),
            nonce: record.nonce.clone(),
            aad: record.aad.clone(),
        },
        kek,
    )
}

async fn load_and_decrypt_active_room_signing_key<R, F>(
    repo: &R,
    room_id: RoomId,
    mut resolve_kek: F,
) -> Result<Option<Vec<u8>>, SettlementError>
where
    R: RoomSigningKeyReadRepository,
    F: FnMut(&str) -> Option<[u8; 32]>,
{
    let Some(record) = repo
        .get_active_room_signing_key(room_id)
        .await
        .map_err(SettlementError::Ledger)?
    else {
        return Ok(None);
    };
    let kek = resolve_kek(&record.kek_id).ok_or_else(|| {
        SettlementError::KeyDecrypt(format!("missing KEK material for kek_id={}", record.kek_id))
    })?;
    decrypt_room_signing_key_record_aes256_gcm(&record, &kek).map(Some)
}

pub async fn mark_failed_receipts_for_manual_review<R: SettlementPersistenceRepository>(
    repo: &R,
    receipts: &[SettlementTransferReceipt],
    retry_count: i32,
    reason: &str,
) -> Result<usize, SettlementError> {
    let mut updated = 0usize;
    for receipt in receipts {
        if !matches!(receipt.status, SettlementTxStatus::Failed) {
            continue;
        }
        repo.update_settlement_record_status_by_tx_hash(&SettlementRecordStatusUpdate {
            tx_hash: receipt.tx_hash.clone(),
            settlement_status: "manual_review_required".to_string(),
            retry_count,
            error_detail: Some(reason.to_string()),
            updated_at: Utc::now(),
        })
        .await
        .map_err(SettlementError::Ledger)?;
        updated += 1;
    }
    Ok(updated)
}

#[derive(Debug, Clone)]
pub struct ReqwestEvmSettlementWalletAdapter {
    endpoint: String,
    from_address: String,
    client: reqwest::Client,
    next_nonce: Arc<Mutex<Option<u64>>>,
}

impl ReqwestEvmSettlementWalletAdapter {
    #[must_use]
    pub fn new(endpoint: impl Into<String>, from_address: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            from_address: from_address.into(),
            client: reqwest::Client::new(),
            next_nonce: Arc::new(Mutex::new(None)),
        }
    }

    async fn rpc_call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, String> {
        let body = serde_json::json!({
            "jsonrpc":"2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let resp = self
            .client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let payload: JsonRpcResponse<T> = resp.json().await.map_err(|e| e.to_string())?;
        if let Some(err) = payload.error {
            return Err(format!("rpc code={} message={}", err.code, err.message));
        }
        payload.result.ok_or_else(|| "missing result".to_string())
    }

    async fn reserve_nonce(&self) -> Result<u64, String> {
        let pending_hex: String = self
            .rpc_call(
                "eth_getTransactionCount",
                serde_json::json!([self.from_address, "pending"]),
            )
            .await?;
        let chain_pending = parse_hex_u64(&pending_hex)?;

        let mut guard = self
            .next_nonce
            .lock()
            .map_err(|_| "nonce lock poisoned".to_string())?;
        let nonce = match *guard {
            Some(local_next) => local_next.max(chain_pending),
            None => chain_pending,
        };
        *guard = Some(nonce.saturating_add(1));
        Ok(nonce)
    }
}

#[derive(Debug, Clone)]
pub struct ReqwestEvmBatchSettlementWalletAdapter {
    endpoint: String,
    from_address: String,
    contract_address: String,
    client: reqwest::Client,
    next_nonce: Arc<Mutex<Option<u64>>>,
    tx_transfer_count: Arc<Mutex<HashMap<String, usize>>>,
}

impl ReqwestEvmBatchSettlementWalletAdapter {
    #[must_use]
    pub fn new(
        endpoint: impl Into<String>,
        from_address: impl Into<String>,
        contract_address: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            from_address: from_address.into(),
            contract_address: contract_address.into(),
            client: reqwest::Client::new(),
            next_nonce: Arc::new(Mutex::new(None)),
            tx_transfer_count: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn rpc_call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, String> {
        let body = serde_json::json!({
            "jsonrpc":"2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let resp = self
            .client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let payload: JsonRpcResponse<T> = resp.json().await.map_err(|e| e.to_string())?;
        if let Some(err) = payload.error {
            return Err(format!("rpc code={} message={}", err.code, err.message));
        }
        payload.result.ok_or_else(|| "missing result".to_string())
    }

    async fn reserve_nonce(&self) -> Result<u64, String> {
        let pending_hex: String = self
            .rpc_call(
                "eth_getTransactionCount",
                serde_json::json!([self.from_address, "pending"]),
            )
            .await?;
        let chain_pending = parse_hex_u64(&pending_hex)?;
        let mut guard = self
            .next_nonce
            .lock()
            .map_err(|_| "nonce lock poisoned".to_string())?;
        let nonce = match *guard {
            Some(local_next) => local_next.max(chain_pending),
            None => chain_pending,
        };
        *guard = Some(nonce.saturating_add(1));
        Ok(nonce)
    }

    fn encode_batch_settlement_call(
        &self,
        request: &BatchSettlementTransferRequest,
    ) -> Result<String, String> {
        let recipients: Vec<Token> = request
            .transfers
            .iter()
            .map(|t| {
                t.to_address
                    .parse()
                    .map(Token::Address)
                    .map_err(|e| format!("invalid recipient address {}: {e}", t.to_address))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let amounts: Vec<Token> = request
            .transfers
            .iter()
            .map(|t| Token::Uint(ethabi::ethereum_types::U256::from(t.amount.as_u128())))
            .collect();
        let settlement_id = build_settlement_batch_id(request.room_id, request.hand_id);

        #[allow(deprecated)]
        let func = Function {
            name: "settleBatch".to_string(),
            inputs: vec![
                Param {
                    name: "recipients".to_string(),
                    kind: ParamType::Array(Box::new(ParamType::Address)),
                    internal_type: None,
                },
                Param {
                    name: "amounts".to_string(),
                    kind: ParamType::Array(Box::new(ParamType::Uint(256))),
                    internal_type: None,
                },
                Param {
                    name: "settlementId".to_string(),
                    kind: ParamType::FixedBytes(32),
                    internal_type: None,
                },
            ],
            outputs: vec![],
            constant: None,
            state_mutability: StateMutability::Payable,
        };
        let data = func
            .encode_input(&[
                Token::Array(recipients),
                Token::Array(amounts),
                Token::FixedBytes(settlement_id.to_vec()),
            ])
            .map_err(|e| format!("abi encode failed: {e}"))?;
        Ok(format!("0x{}", hex::encode(data)))
    }
}

fn build_settlement_batch_id(room_id: RoomId, hand_id: HandId) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(room_id.0.as_bytes());
    hasher.update(hand_id.0.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0_u8; 32];
    out.copy_from_slice(&digest);
    out
}

impl<L> SettlementService<L>
where
    L: LedgerRepository,
{
    #[must_use]
    pub fn new(ledger_repo: L) -> Self {
        Self { ledger_repo }
    }

    pub async fn load_room_signing_key_for_settlement<R, F>(
        &self,
        repo: &R,
        room_id: RoomId,
        resolve_kek: F,
        trace_id: TraceId,
    ) -> Result<Option<RoomSigningKeyHandle>, SettlementError>
    where
        R: RoomSigningKeyReadRepository,
        F: FnMut(&str) -> Option<[u8; 32]>,
    {
        let Some(record) = repo
            .get_active_room_signing_key(room_id)
            .await
            .map_err(SettlementError::Ledger)?
        else {
            return Ok(None);
        };
        let encrypted_len = record.encrypted_private_key.len();
        let key_version = record.key_version;
        let address = record.address.clone();
        let private_key_bytes = load_and_decrypt_active_room_signing_key(repo, room_id, resolve_kek)
            .await?
            .ok_or_else(|| SettlementError::KeyDecrypt("active key disappeared".to_string()))?;
        info!(
            room_id = %room_id.0,
            address = %address,
            key_version,
            encrypted_len,
            trace_id = ?trace_id,
            "room signing key loaded for settlement service"
        );
        Ok(Some(RoomSigningKeyHandle {
            address,
            key_version,
            private_key_bytes,
        }))
    }

    pub fn build_plan_from_winners(
        &self,
        room_id: RoomId,
        hand_id: HandId,
        pot_total: Chips,
        winner_seats: &[SeatId],
        rake_policy: RakePolicy,
    ) -> Result<SettlementPlan, SettlementError> {
        let mut winners = winner_seats.to_vec();
        winners.sort_unstable();
        winners.dedup();
        if winners.is_empty() {
            return Ok(SettlementPlan {
                room_id,
                hand_id,
                rake_amount: Chips::ZERO,
                payouts: Vec::new(),
            });
        }

        let rake_amount = rake_policy.rake_amount_for(pot_total)?;
        let distributable = pot_total.checked_sub(rake_amount)?;
        let winner_count = winners.len() as u128;
        let base = Chips(distributable.as_u128() / winner_count);
        let remainder = distributable.as_u128() % winner_count;

        let payouts = winners
            .into_iter()
            .enumerate()
            .map(|(idx, seat_id)| SettlementPayout {
                seat_id,
                amount: Chips(base.as_u128() + u128::from((idx as u128) < remainder)),
            })
            .collect();

        Ok(SettlementPlan {
            room_id,
            hand_id,
            rake_amount,
            payouts,
        })
    }

    pub fn build_plan_from_pot_awards(
        &self,
        room_id: RoomId,
        hand_id: HandId,
        pots: &[PotAwardInput],
        rake_policy: RakePolicy,
    ) -> Result<SettlementPlan, SettlementError> {
        let mut gross_by_seat: HashMap<SeatId, Chips> = HashMap::new();

        for pot in pots {
            if pot.amount == Chips::ZERO {
                continue;
            }
            let mut eligible = pot.eligible_seats.clone();
            eligible.sort_unstable();
            eligible.dedup();

            let mut winners = pot.winner_seats.clone();
            winners.sort_unstable();
            winners.dedup();
            if winners.is_empty() {
                continue;
            }
            if !winners
                .iter()
                .all(|seat| eligible.binary_search(seat).is_ok())
            {
                return Err(SettlementError::InvalidPotAward(
                    "winner_seats must be subset of eligible_seats".to_string(),
                ));
            }

            let split = split_amount_deterministic(pot.amount, &winners);
            for payout in split {
                let next = gross_by_seat
                    .get(&payout.seat_id)
                    .copied()
                    .unwrap_or(Chips::ZERO)
                    .checked_add(payout.amount)?;
                gross_by_seat.insert(payout.seat_id, next);
            }
        }

        let mut gross_payouts: Vec<SettlementPayout> = gross_by_seat
            .into_iter()
            .map(|(seat_id, amount)| SettlementPayout { seat_id, amount })
            .collect();
        gross_payouts.sort_by_key(|p| p.seat_id);

        let gross_total = gross_payouts
            .iter()
            .try_fold(Chips::ZERO, |acc, p| acc.checked_add(p.amount))?;
        if gross_total == Chips::ZERO {
            return Ok(SettlementPlan {
                room_id,
                hand_id,
                rake_amount: Chips::ZERO,
                payouts: Vec::new(),
            });
        }

        let rake_amount = rake_policy.rake_amount_for(gross_total)?;
        let distributable = gross_total.checked_sub(rake_amount)?;
        let payouts = apportion_by_weight(&gross_payouts, distributable)?;

        Ok(SettlementPlan {
            room_id,
            hand_id,
            rake_amount,
            payouts,
        })
    }

    pub async fn record_plan(
        &self,
        plan: &SettlementPlan,
        trace_id: TraceId,
    ) -> Result<(), SettlementError> {
        let mut entries = Vec::with_capacity(plan.payouts.len() + 1);

        if plan.rake_amount != Chips::ZERO {
            entries.push(LedgerEntryInsert {
                entry_type: "rake_accrual".to_string(),
                room_id: Some(plan.room_id),
                hand_id: Some(plan.hand_id),
                seat_id: None,
                agent_id: None,
                asset_type: "CFX".to_string(),
                amount: plan.rake_amount,
                direction: LedgerDirection::Credit,
                account_scope: "room_rake".to_string(),
                related_tx_hash: None,
                related_event_id: None,
                related_attempt_id: None,
                remark: Some("rake accrual from settlement plan".to_string()),
                trace_id,
            });
        }

        for payout in &plan.payouts {
            entries.push(LedgerEntryInsert {
                entry_type: "settlement_payout".to_string(),
                room_id: Some(plan.room_id),
                hand_id: Some(plan.hand_id),
                seat_id: Some(payout.seat_id),
                agent_id: None,
                asset_type: "CFX".to_string(),
                amount: payout.amount,
                direction: LedgerDirection::Debit,
                account_scope: "room_pot".to_string(),
                related_tx_hash: None,
                related_event_id: None,
                related_attempt_id: None,
                remark: Some("planned payout".to_string()),
                trace_id,
            });
        }

        self.ledger_repo.insert_entries(&entries).await?;
        info!(entries = entries.len(), "settlement plan recorded");
        Ok(())
    }

    pub async fn submit_direct_payouts<W>(
        &self,
        plan: &SettlementPlan,
        seat_addresses: &std::collections::HashMap<SeatId, String>,
        wallet: &W,
    ) -> Result<Vec<SettlementTransferSubmission>, SettlementError>
    where
        W: SettlementWalletAdapter,
    {
        let mut submissions = Vec::new();
        for payout in &plan.payouts {
            if payout.amount == Chips::ZERO {
                continue;
            }
            let Some(to_address) = seat_addresses.get(&payout.seat_id).cloned() else {
                return Err(SettlementError::WalletAdapter(format!(
                    "missing payout address for seat {}",
                    payout.seat_id
                )));
            };
            let tx_hash = wallet
                .submit_transfer(&SettlementTransferRequest {
                    room_id: plan.room_id,
                    hand_id: plan.hand_id,
                    to_seat_id: payout.seat_id,
                    to_address,
                    amount: payout.amount,
                })
                .await
                .map_err(SettlementError::WalletAdapter)?;
            submissions.push(SettlementTransferSubmission {
                seat_id: payout.seat_id,
                tx_hash,
            });
        }
        Ok(submissions)
    }

    pub async fn check_payout_submissions<W>(
        &self,
        wallet: &W,
        submissions: &[SettlementTransferSubmission],
        min_confirmations: u64,
    ) -> Result<Vec<SettlementTransferReceipt>, SettlementError>
    where
        W: SettlementWalletAdapter,
    {
        let mut receipts = Vec::with_capacity(submissions.len());
        for submission in submissions {
            let mut receipt = wallet
                .get_transfer_receipt(&submission.tx_hash)
                .await
                .map_err(SettlementError::WalletAdapter)?;
            if matches!(receipt.status, SettlementTxStatus::Confirmed)
                && receipt.confirmations < min_confirmations
            {
                receipt.status = SettlementTxStatus::Pending;
            }
            receipts.push(receipt);
        }
        Ok(receipts)
    }

    pub async fn submit_batch_payouts<W>(
        &self,
        plan: &SettlementPlan,
        seat_addresses: &std::collections::HashMap<SeatId, String>,
        wallet: &W,
    ) -> Result<BatchSettlementTransferSubmission, SettlementError>
    where
        W: BatchSettlementWalletAdapter,
    {
        let mut transfers = Vec::new();
        for payout in &plan.payouts {
            if payout.amount == Chips::ZERO {
                continue;
            }
            let Some(to_address) = seat_addresses.get(&payout.seat_id).cloned() else {
                return Err(SettlementError::WalletAdapter(format!(
                    "missing payout address for seat {}",
                    payout.seat_id
                )));
            };
            transfers.push(SettlementTransferRequest {
                room_id: plan.room_id,
                hand_id: plan.hand_id,
                to_seat_id: payout.seat_id,
                to_address,
                amount: payout.amount,
            });
        }

        let tx_hash = wallet
            .submit_batch_transfer(&BatchSettlementTransferRequest {
                room_id: plan.room_id,
                hand_id: plan.hand_id,
                transfers,
            })
            .await
            .map_err(SettlementError::WalletAdapter)?;

        Ok(BatchSettlementTransferSubmission {
            tx_hash,
            transfer_count: plan
                .payouts
                .iter()
                .filter(|p| p.amount != Chips::ZERO)
                .count(),
        })
    }

    pub async fn check_batch_payout_submission<W>(
        &self,
        wallet: &W,
        submission: &BatchSettlementTransferSubmission,
        min_confirmations: u64,
    ) -> Result<BatchSettlementTransferReceipt, SettlementError>
    where
        W: BatchSettlementWalletAdapter,
    {
        let mut receipt = wallet
            .get_batch_transfer_receipt(&submission.tx_hash)
            .await
            .map_err(SettlementError::WalletAdapter)?;
        if matches!(receipt.status, SettlementTxStatus::Confirmed)
            && receipt.confirmations < min_confirmations
        {
            receipt.status = SettlementTxStatus::Pending;
        }
        Ok(receipt)
    }
}

#[async_trait]
impl SettlementWalletAdapter for ReqwestEvmSettlementWalletAdapter {
    async fn submit_transfer(&self, request: &SettlementTransferRequest) -> Result<String, String> {
        let value_hex = format!("0x{:x}", request.amount.as_u128());
        let nonce_hex = format!("0x{:x}", self.reserve_nonce().await?);
        self.rpc_call(
            "eth_sendTransaction",
            serde_json::json!([{
                "from": self.from_address,
                "to": request.to_address,
                "value": value_hex,
                "nonce": nonce_hex,
            }]),
        )
        .await
    }

    async fn get_transfer_receipt(
        &self,
        tx_hash: &str,
    ) -> Result<SettlementTransferReceipt, String> {
        let receipt: Option<EthTransactionReceipt> = self
            .rpc_call("eth_getTransactionReceipt", serde_json::json!([tx_hash]))
            .await?;
        let Some(receipt) = receipt else {
            return Ok(SettlementTransferReceipt {
                tx_hash: tx_hash.to_string(),
                status: SettlementTxStatus::Pending,
                confirmations: 0,
            });
        };

        let latest_block_hex: String = self
            .rpc_call("eth_blockNumber", serde_json::json!([]))
            .await?;
        let latest_block = parse_hex_u64(&latest_block_hex)?;
        let receipt_block = receipt
            .block_number
            .as_deref()
            .map(parse_hex_u64)
            .transpose()?
            .unwrap_or(0);
        let confirmations = latest_block.saturating_sub(receipt_block).saturating_add(1);
        let success = receipt
            .status
            .as_deref()
            .map(parse_hex_u64)
            .transpose()?
            .map(|v| v == 1)
            .unwrap_or(false);

        Ok(SettlementTransferReceipt {
            tx_hash: tx_hash.to_string(),
            status: if success {
                SettlementTxStatus::Confirmed
            } else {
                SettlementTxStatus::Failed
            },
            confirmations,
        })
    }
}

#[async_trait]
impl BatchSettlementWalletAdapter for ReqwestEvmBatchSettlementWalletAdapter {
    async fn submit_batch_transfer(
        &self,
        request: &BatchSettlementTransferRequest,
    ) -> Result<String, String> {
        if request.transfers.is_empty() {
            return Err("batch transfer list cannot be empty".to_string());
        }
        let total_value = request
            .transfers
            .iter()
            .try_fold(Chips::ZERO, |acc, t| acc.checked_add(t.amount))
            .map_err(|e| format!("batch payout total overflow: {e}"))?;
        let data = self.encode_batch_settlement_call(request)?;
        let nonce_hex = format!("0x{:x}", self.reserve_nonce().await?);
        let value_hex = format!("0x{:x}", total_value.as_u128());
        let tx_hash: String = self
            .rpc_call(
                "eth_sendTransaction",
                serde_json::json!([{
                    "from": self.from_address,
                    "to": self.contract_address,
                    "value": value_hex,
                    "data": data,
                    "nonce": nonce_hex,
                }]),
            )
            .await?;
        self.tx_transfer_count
            .lock()
            .map_err(|_| "tx_transfer_count lock poisoned".to_string())?
            .insert(tx_hash.clone(), request.transfers.len());
        Ok(tx_hash)
    }

    async fn get_batch_transfer_receipt(
        &self,
        tx_hash: &str,
    ) -> Result<BatchSettlementTransferReceipt, String> {
        let receipt: Option<EthTransactionReceipt> = self
            .rpc_call("eth_getTransactionReceipt", serde_json::json!([tx_hash]))
            .await?;
        let Some(receipt) = receipt else {
            return Ok(BatchSettlementTransferReceipt {
                tx_hash: tx_hash.to_string(),
                status: SettlementTxStatus::Pending,
                confirmations: 0,
                applied_count: self
                    .tx_transfer_count
                    .lock()
                    .ok()
                    .and_then(|m| m.get(tx_hash).copied())
                    .unwrap_or(0),
            });
        };

        let latest_block_hex: String = self
            .rpc_call("eth_blockNumber", serde_json::json!([]))
            .await?;
        let latest_block = parse_hex_u64(&latest_block_hex)?;
        let receipt_block = receipt
            .block_number
            .as_deref()
            .map(parse_hex_u64)
            .transpose()?
            .unwrap_or(0);
        let confirmations = latest_block.saturating_sub(receipt_block).saturating_add(1);
        let success = receipt
            .status
            .as_deref()
            .map(parse_hex_u64)
            .transpose()?
            .map(|v| v == 1)
            .unwrap_or(false);
        let applied_count = self
            .tx_transfer_count
            .lock()
            .ok()
            .and_then(|m| m.get(tx_hash).copied())
            .unwrap_or(0);
        Ok(BatchSettlementTransferReceipt {
            tx_hash: tx_hash.to_string(),
            status: if success {
                SettlementTxStatus::Confirmed
            } else {
                SettlementTxStatus::Failed
            },
            confirmations,
            applied_count,
        })
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct EthTransactionReceipt {
    status: Option<String>,
    #[serde(rename = "blockNumber")]
    block_number: Option<String>,
}

fn split_amount_deterministic(amount: Chips, winners: &[SeatId]) -> Vec<SettlementPayout> {
    if winners.is_empty() {
        return Vec::new();
    }
    let count = winners.len() as u128;
    let base = amount.as_u128() / count;
    let remainder = amount.as_u128() % count;
    winners
        .iter()
        .copied()
        .enumerate()
        .map(|(idx, seat_id)| SettlementPayout {
            seat_id,
            amount: Chips(base + u128::from((idx as u128) < remainder)),
        })
        .collect()
}

fn apportion_by_weight(
    gross: &[SettlementPayout],
    target_total: Chips,
) -> Result<Vec<SettlementPayout>, SettlementError> {
    if gross.is_empty() {
        return Ok(Vec::new());
    }
    let gross_total: u128 = gross.iter().map(|p| p.amount.as_u128()).sum();
    if gross_total == 0 {
        return Ok(Vec::new());
    }

    let target = target_total.as_u128();
    let mut floor_sum: u128 = 0;
    let mut rows: Vec<(SeatId, u128, u128)> = Vec::with_capacity(gross.len());
    for p in gross {
        let weighted = p.amount.as_u128().saturating_mul(target);
        let floor = weighted / gross_total;
        let rem = weighted % gross_total;
        floor_sum = floor_sum.saturating_add(floor);
        rows.push((p.seat_id, floor, rem));
    }

    let mut extra = target.saturating_sub(floor_sum);
    rows.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    for row in &mut rows {
        if extra == 0 {
            break;
        }
        row.1 = row.1.saturating_add(1);
        extra -= 1;
    }
    rows.sort_by_key(|r| r.0);

    Ok(rows
        .into_iter()
        .filter_map(|(seat_id, amount, _)| {
            (amount > 0).then_some(SettlementPayout {
                seat_id,
                amount: Chips(amount),
            })
        })
        .collect())
}

fn parse_hex_u64(input: &str) -> Result<u64, String> {
    let s = input.trim();
    let raw = s.strip_prefix("0x").unwrap_or(s);
    if raw.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(raw, 16).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::aead::{Aead, Payload};
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use ledger_store::{
        InMemoryChainTxRepository, InMemoryLedgerRepository,
        InMemorySettlementPersistenceRepository, NoopLedgerRepository, RoomSigningKeyRecord,
        SettlementPersistenceReadRepository, SettlementPlanRecordInsert, SettlementRecordInsert,
    };
    use std::collections::HashMap;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use uuid::Uuid;

    #[derive(Debug, Default, Clone)]
    struct MockWalletAdapter {
        calls: Arc<Mutex<Vec<SettlementTransferRequest>>>,
    }

    #[derive(Debug, Default, Clone)]
    struct MockBatchWalletAdapter {
        calls: Arc<Mutex<Vec<BatchSettlementTransferRequest>>>,
    }

    #[async_trait]
    impl SettlementWalletAdapter for MockWalletAdapter {
        async fn submit_transfer(
            &self,
            request: &SettlementTransferRequest,
        ) -> Result<String, String> {
            let mut guard = self.calls.lock().map_err(|_| "lock poisoned".to_string())?;
            guard.push(request.clone());
            Ok(format!("0xtx{}", guard.len()))
        }

        async fn get_transfer_receipt(
            &self,
            tx_hash: &str,
        ) -> Result<SettlementTransferReceipt, String> {
            let confirmations = if tx_hash.ends_with('1') { 3 } else { 0 };
            Ok(SettlementTransferReceipt {
                tx_hash: tx_hash.to_string(),
                status: if confirmations > 0 {
                    SettlementTxStatus::Confirmed
                } else {
                    SettlementTxStatus::Pending
                },
                confirmations,
            })
        }
    }

    #[async_trait]
    impl BatchSettlementWalletAdapter for MockBatchWalletAdapter {
        async fn submit_batch_transfer(
            &self,
            request: &BatchSettlementTransferRequest,
        ) -> Result<String, String> {
            self.calls
                .lock()
                .map_err(|_| "lock poisoned".to_string())?
                .push(request.clone());
            Ok("0xbatchtx1".to_string())
        }

        async fn get_batch_transfer_receipt(
            &self,
            tx_hash: &str,
        ) -> Result<BatchSettlementTransferReceipt, String> {
            Ok(BatchSettlementTransferReceipt {
                tx_hash: tx_hash.to_string(),
                status: SettlementTxStatus::Confirmed,
                confirmations: 1,
                applied_count: 2,
            })
        }
    }

    #[test]
    fn build_plan_splits_evenly_with_deterministic_remainder() {
        let svc = SettlementService::new(NoopLedgerRepository);
        let room_id = RoomId::new();
        let hand_id = HandId::new();
        let plan = svc
            .build_plan_from_winners(room_id, hand_id, Chips(10), &[2, 1, 3], RakePolicy::zero())
            .expect("plan");

        assert_eq!(plan.rake_amount, Chips::ZERO);
        assert_eq!(plan.payouts.len(), 3);
        assert_eq!(
            plan.payouts[0],
            SettlementPayout {
                seat_id: 1,
                amount: Chips(4)
            }
        );
        assert_eq!(
            plan.payouts[1],
            SettlementPayout {
                seat_id: 2,
                amount: Chips(3)
            }
        );
        assert_eq!(
            plan.payouts[2],
            SettlementPayout {
                seat_id: 3,
                amount: Chips(3)
            }
        );
        assert_eq!(plan.payout_total().expect("sum"), Chips(10));
    }

    #[test]
    fn build_plan_applies_rake_bps_before_split() {
        let svc = SettlementService::new(NoopLedgerRepository);
        let plan = svc
            .build_plan_from_winners(
                RoomId::new(),
                HandId::new(),
                Chips(1000),
                &[0, 1],
                RakePolicy { rake_bps: 300 },
            )
            .expect("plan");

        assert_eq!(plan.rake_amount, Chips(30));
        assert_eq!(plan.payout_total().expect("sum"), Chips(970));
        assert_eq!(plan.payouts[0].amount, Chips(485));
        assert_eq!(plan.payouts[1].amount, Chips(485));
    }

    #[test]
    fn rake_rounds_down_to_chain_unit() {
        let policy = RakePolicy::three_percent();
        assert_eq!(policy.rake_amount_for(Chips(33)).expect("rake"), Chips(0));
        assert_eq!(policy.rake_amount_for(Chips(34)).expect("rake"), Chips(1));
    }

    #[test]
    fn build_plan_rejects_invalid_rake_policy() {
        let svc = SettlementService::new(NoopLedgerRepository);
        let err = svc
            .build_plan_from_winners(
                RoomId::new(),
                HandId::new(),
                Chips(100),
                &[0, 1],
                RakePolicy { rake_bps: 10_001 },
            )
            .expect_err("invalid rake policy should be rejected");
        assert!(err.to_string().contains("invalid rake policy"));
    }

    #[test]
    fn build_plan_from_pot_awards_handles_main_and_side_pot() {
        let svc = SettlementService::new(NoopLedgerRepository);
        let plan = svc
            .build_plan_from_pot_awards(
                RoomId::new(),
                HandId::new(),
                &[
                    PotAwardInput {
                        amount: Chips(120),
                        eligible_seats: vec![0, 1, 2],
                        winner_seats: vec![2],
                    },
                    PotAwardInput {
                        amount: Chips(120),
                        eligible_seats: vec![0, 1],
                        winner_seats: vec![1],
                    },
                ],
                RakePolicy::zero(),
            )
            .expect("plan");

        assert_eq!(plan.rake_amount, Chips::ZERO);
        assert_eq!(plan.payout_total().expect("sum"), Chips(240));
        assert_eq!(
            plan.payouts,
            vec![
                SettlementPayout {
                    seat_id: 1,
                    amount: Chips(120)
                },
                SettlementPayout {
                    seat_id: 2,
                    amount: Chips(120)
                }
            ]
        );
    }

    #[test]
    fn build_plan_from_pot_awards_rejects_winner_outside_eligible() {
        let svc = SettlementService::new(NoopLedgerRepository);
        let err = svc
            .build_plan_from_pot_awards(
                RoomId::new(),
                HandId::new(),
                &[PotAwardInput {
                    amount: Chips(10),
                    eligible_seats: vec![0, 1],
                    winner_seats: vec![2],
                }],
                RakePolicy::zero(),
            )
            .expect_err("invalid");
        assert!(err.to_string().contains("winner_seats"));
    }

    #[test]
    fn build_plan_from_pot_awards_applies_rake_proportionally() {
        let svc = SettlementService::new(NoopLedgerRepository);
        let plan = svc
            .build_plan_from_pot_awards(
                RoomId::new(),
                HandId::new(),
                &[
                    PotAwardInput {
                        amount: Chips(101),
                        eligible_seats: vec![0, 1],
                        winner_seats: vec![0, 1],
                    },
                    PotAwardInput {
                        amount: Chips(100),
                        eligible_seats: vec![0],
                        winner_seats: vec![0],
                    },
                ],
                RakePolicy { rake_bps: 1000 },
            )
            .expect("plan");

        assert_eq!(plan.rake_amount, Chips(20));
        assert_eq!(plan.payout_total().expect("sum"), Chips(181));
        // Gross before rake would be seat0=151, seat1=50; after proportional apportionment seat0 stays larger.
        assert!(
            plan.payouts
                .iter()
                .any(|p| p.seat_id == 0 && p.amount > Chips(130))
        );
        assert!(
            plan.payouts
                .iter()
                .any(|p| p.seat_id == 1 && p.amount < Chips(50))
        );
    }

    #[test]
    fn pot_award_rake_rounding_and_remainder_apportion_is_deterministic() {
        let svc = SettlementService::new(NoopLedgerRepository);
        let plan = svc
            .build_plan_from_pot_awards(
                RoomId::new(),
                HandId::new(),
                &[PotAwardInput {
                    amount: Chips(34),
                    eligible_seats: vec![1, 0],
                    winner_seats: vec![1, 0],
                }],
                RakePolicy::three_percent(),
            )
            .expect("plan");
        // 34 * 3% = 1.02 => floor 1, distributable = 33.
        assert_eq!(plan.rake_amount, Chips(1));
        assert_eq!(plan.payout_total().expect("sum"), Chips(33));
        // Equal weights with odd remainder should deterministically grant +1 to smaller seat_id.
        assert_eq!(
            plan.payouts,
            vec![
                SettlementPayout {
                    seat_id: 0,
                    amount: Chips(17)
                },
                SettlementPayout {
                    seat_id: 1,
                    amount: Chips(16)
                }
            ]
        );
    }

    #[tokio::test]
    async fn record_plan_writes_rake_and_payout_entries() {
        let repo = InMemoryLedgerRepository::new();
        let svc = SettlementService::new(repo.clone());
        let plan = SettlementPlan {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            rake_amount: Chips(30),
            payouts: vec![
                SettlementPayout {
                    seat_id: 0,
                    amount: Chips(100),
                },
                SettlementPayout {
                    seat_id: 1,
                    amount: Chips(200),
                },
            ],
        };

        svc.record_plan(&plan, TraceId::new())
            .await
            .expect("record");
        let entries = repo.entries_snapshot();
        assert_eq!(entries.len(), 3);
        assert!(
            entries
                .iter()
                .any(|e| e.entry_type == "rake_accrual" && e.amount == Chips(30))
        );
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.entry_type == "settlement_payout")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn submit_direct_payouts_sends_one_transfer_per_payout() {
        let repo = InMemoryLedgerRepository::new();
        let svc = SettlementService::new(repo);
        let wallet = MockWalletAdapter::default();
        let plan = SettlementPlan {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            rake_amount: Chips(10),
            payouts: vec![
                SettlementPayout {
                    seat_id: 1,
                    amount: Chips(120),
                },
                SettlementPayout {
                    seat_id: 2,
                    amount: Chips(80),
                },
            ],
        };
        let mut seat_addresses = HashMap::new();
        seat_addresses.insert(1, "0xseat1".to_string());
        seat_addresses.insert(2, "0xseat2".to_string());

        let submissions = svc
            .submit_direct_payouts(&plan, &seat_addresses, &wallet)
            .await
            .expect("submit");
        assert_eq!(submissions.len(), 2);
        assert_eq!(submissions[0].seat_id, 1);
        assert_eq!(submissions[1].seat_id, 2);

        let calls = wallet.calls.lock().expect("lock");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].to_address, "0xseat1");
        assert_eq!(calls[1].to_address, "0xseat2");
    }

    #[tokio::test]
    async fn submit_direct_payouts_requires_seat_address_mapping() {
        let svc = SettlementService::new(InMemoryLedgerRepository::new());
        let wallet = MockWalletAdapter::default();
        let plan = SettlementPlan {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            rake_amount: Chips::ZERO,
            payouts: vec![SettlementPayout {
                seat_id: 7,
                amount: Chips(1),
            }],
        };

        let err = svc
            .submit_direct_payouts(&plan, &HashMap::new(), &wallet)
            .await
            .expect_err("missing address should fail");
        assert!(err.to_string().contains("missing payout address"));
    }

    #[tokio::test]
    async fn check_payout_submissions_applies_confirmation_threshold() {
        let svc = SettlementService::new(InMemoryLedgerRepository::new());
        let wallet = MockWalletAdapter::default();
        let receipts = svc
            .check_payout_submissions(
                &wallet,
                &[
                    SettlementTransferSubmission {
                        seat_id: 1,
                        tx_hash: "0xtx1".to_string(),
                    },
                    SettlementTransferSubmission {
                        seat_id: 2,
                        tx_hash: "0xtx2".to_string(),
                    },
                ],
                5,
            )
            .await
            .expect("receipts");

        assert_eq!(receipts.len(), 2);
        assert_eq!(receipts[0].status, SettlementTxStatus::Pending);
        assert_eq!(receipts[0].confirmations, 3);
        assert_eq!(receipts[1].status, SettlementTxStatus::Pending);
        assert_eq!(receipts[1].confirmations, 0);
    }

    #[tokio::test]
    async fn batch_payout_adapter_interface_builds_and_checks_receipt() {
        let svc = SettlementService::new(InMemoryLedgerRepository::new());
        let wallet = MockBatchWalletAdapter::default();
        let plan = SettlementPlan {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            rake_amount: Chips::ZERO,
            payouts: vec![
                SettlementPayout {
                    seat_id: 1,
                    amount: Chips(5),
                },
                SettlementPayout {
                    seat_id: 2,
                    amount: Chips(6),
                },
            ],
        };
        let mut seat_addresses = HashMap::new();
        seat_addresses.insert(1, "0xseat1".to_string());
        seat_addresses.insert(2, "0xseat2".to_string());

        let submission = svc
            .submit_batch_payouts(&plan, &seat_addresses, &wallet)
            .await
            .expect("submit batch");
        assert_eq!(submission.tx_hash, "0xbatchtx1");
        assert_eq!(submission.transfer_count, 2);

        let receipt = svc
            .check_batch_payout_submission(&wallet, &submission, 2)
            .await
            .expect("receipt");
        assert_eq!(receipt.tx_hash, "0xbatchtx1");
        assert_eq!(receipt.status, SettlementTxStatus::Pending);

        let calls = wallet.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].transfers.len(), 2);
    }

    #[test]
    fn decrypt_room_signing_key_aes256_gcm_roundtrip() {
        let kek = [7_u8; 32];
        let nonce = [9_u8; 12];
        let aad = b"room-key:v1".to_vec();
        let plaintext = b"0xdeadbeefcafebabe".to_vec();
        let cipher = Aes256Gcm::new_from_slice(&kek).expect("cipher");
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .expect("encrypt");
        let record = EncryptedRoomSigningKey {
            room_id: RoomId::new(),
            address: "0xroom".to_string(),
            encrypted_private_key: ciphertext,
            cipher_alg: "AES-256-GCM".to_string(),
            key_version: 1,
            kek_id: "kek-1".to_string(),
            nonce: nonce.to_vec(),
            aad: aad.clone(),
        };

        let decrypted = decrypt_room_signing_key_aes256_gcm(&record, &kek).expect("decrypt");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn decrypt_room_signing_key_fails_on_aad_mismatch() {
        let kek = [1_u8; 32];
        let nonce = [2_u8; 12];
        let cipher = Aes256Gcm::new_from_slice(&kek).expect("cipher");
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: b"secret",
                    aad: b"aad-ok",
                },
            )
            .expect("encrypt");
        let record = EncryptedRoomSigningKey {
            room_id: RoomId::new(),
            address: "0xroom".to_string(),
            encrypted_private_key: ciphertext,
            cipher_alg: "AES-256-GCM".to_string(),
            key_version: 1,
            kek_id: "kek-1".to_string(),
            nonce: nonce.to_vec(),
            aad: b"aad-bad".to_vec(),
        };

        let err = decrypt_room_signing_key_aes256_gcm(&record, &kek).expect_err("aad mismatch");
        assert!(err.to_string().contains("decrypt error") || err.to_string().contains("aead"));
    }

    #[tokio::test]
    async fn settlement_service_loads_and_decrypts_active_room_key() {
        let room_id = RoomId::new();
        let kek = [3_u8; 32];
        let nonce = [4_u8; 12];
        let aad = b"room-key-aad".to_vec();
        let plaintext = b"room-private-key".to_vec();
        let cipher = Aes256Gcm::new_from_slice(&kek).expect("cipher");
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .expect("encrypt");

        let repo = InMemoryChainTxRepository::new();
        repo.upsert_room_signing_key_record(RoomSigningKeyRecord {
            room_id,
            address: "0xroom".to_string(),
            encrypted_private_key: ciphertext,
            cipher_alg: "AES-256-GCM".to_string(),
            key_version: 1,
            kek_id: "kek-1".to_string(),
            nonce: nonce.to_vec(),
            aad: aad.clone(),
            status: "active".to_string(),
            created_at: chrono::Utc::now(),
            rotated_at: None,
            revoked_at: None,
        })
        .expect("insert");

        let svc = SettlementService::new(InMemoryLedgerRepository::new());
        let handle = svc
            .load_room_signing_key_for_settlement(
                &repo,
                room_id,
                |kek_id| (kek_id == "kek-1").then_some(kek),
                TraceId::new(),
            )
            .await
            .expect("decrypt")
            .expect("active key");
        assert_eq!(handle.address(), "0xroom");
        assert_eq!(handle.key_version(), 1);
        assert_eq!(handle.private_key_bytes(), plaintext.as_slice());
    }

    #[tokio::test]
    async fn mark_failed_receipts_for_manual_review_updates_only_failed_receipts() {
        let repo = InMemorySettlementPersistenceRepository::new();
        let room_id = RoomId::new();
        let hand_id = HandId::new();
        let plan_id = Uuid::now_v7();
        let now = Utc::now();
        repo.insert_settlement_plan(&SettlementPlanRecordInsert {
            settlement_plan_id: plan_id,
            room_id,
            hand_id,
            status: "created".to_string(),
            rake_amount: Chips::ZERO,
            payout_total: Chips(10),
            payload_json: serde_json::json!({}),
            created_at: now,
            finalized_at: None,
        })
        .await
        .expect("plan");
        repo.insert_settlement_record(&SettlementRecordInsert {
            settlement_record_id: Uuid::now_v7(),
            settlement_plan_id: plan_id,
            room_id,
            hand_id,
            tx_hash: Some("0xfail".to_string()),
            settlement_status: "failed".to_string(),
            retry_count: 1,
            error_detail: Some("old".to_string()),
            created_at: now,
            updated_at: now,
        })
        .await
        .expect("record1");
        repo.insert_settlement_record(&SettlementRecordInsert {
            settlement_record_id: Uuid::now_v7(),
            settlement_plan_id: plan_id,
            room_id,
            hand_id,
            tx_hash: Some("0xok".to_string()),
            settlement_status: "confirmed".to_string(),
            retry_count: 0,
            error_detail: None,
            created_at: now,
            updated_at: now,
        })
        .await
        .expect("record2");

        let updated = mark_failed_receipts_for_manual_review(
            &repo,
            &[
                SettlementTransferReceipt {
                    tx_hash: "0xfail".to_string(),
                    status: SettlementTxStatus::Failed,
                    confirmations: 0,
                },
                SettlementTransferReceipt {
                    tx_hash: "0xok".to_string(),
                    status: SettlementTxStatus::Confirmed,
                    confirmations: 2,
                },
            ],
            2,
            "manual review required after failed payout receipt",
        )
        .await
        .expect("mark");

        assert_eq!(updated, 1);
        let records = repo
            .list_settlement_records_by_hand(hand_id)
            .await
            .expect("list");
        assert!(records.iter().any(|r| {
            r.tx_hash.as_deref() == Some("0xfail")
                && r.settlement_status == "manual_review_required"
                && r.retry_count == 2
        }));
        assert!(records.iter().any(|r| {
            r.tx_hash.as_deref() == Some("0xok") && r.settlement_status == "confirmed"
        }));
    }

    #[test]
    fn parse_hex_u64_parses_evm_quantities() {
        assert_eq!(parse_hex_u64("0x0").expect("zero"), 0);
        assert_eq!(parse_hex_u64("0x10").expect("sixteen"), 16);
    }

    #[test]
    fn settlement_batch_id_is_stable_for_same_room_and_hand() {
        let room_id = RoomId::new();
        let hand_id = HandId::new();
        let a = build_settlement_batch_id(room_id, hand_id);
        let b = build_settlement_batch_id(room_id, hand_id);
        assert_eq!(a, b);
    }

    #[test]
    fn batch_settlement_adapter_encodes_contract_call_data() {
        let adapter = ReqwestEvmBatchSettlementWalletAdapter::new(
            "http://127.0.0.1:8545",
            "0x0000000000000000000000000000000000000001",
            "0x0000000000000000000000000000000000000002",
        );
        let request = BatchSettlementTransferRequest {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            transfers: vec![
                SettlementTransferRequest {
                    room_id: RoomId::new(),
                    hand_id: HandId::new(),
                    to_seat_id: 1,
                    to_address: "0x0000000000000000000000000000000000000011".to_string(),
                    amount: Chips(7),
                },
                SettlementTransferRequest {
                    room_id: RoomId::new(),
                    hand_id: HandId::new(),
                    to_seat_id: 2,
                    to_address: "0x0000000000000000000000000000000000000022".to_string(),
                    amount: Chips(9),
                },
            ],
        };
        let data = adapter
            .encode_batch_settlement_call(&request)
            .expect("encode");
        assert!(data.starts_with("0x"));
        assert!(data.len() > 10);
    }

    #[tokio::test]
    #[ignore = "requires local anvil binary and free TCP port"]
    async fn reqwest_evm_wallet_adapter_can_submit_and_poll_against_anvil() {
        let port = 28545_u16;
        let mut child = Command::new("anvil")
            .arg("-p")
            .arg(port.to_string())
            .arg("--silent")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn anvil");
        tokio::time::sleep(Duration::from_millis(500)).await;

        let client = reqwest::Client::new();
        let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"eth_accounts","params":[]});
        let resp: serde_json::Value = client
            .post(format!("http://127.0.0.1:{port}"))
            .json(&body)
            .send()
            .await
            .expect("send")
            .json()
            .await
            .expect("json");
        let accounts = resp["result"]
            .as_array()
            .expect("accounts")
            .iter()
            .filter_map(|v| v.as_str())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        assert!(accounts.len() >= 2);

        let adapter = ReqwestEvmSettlementWalletAdapter::new(
            format!("http://127.0.0.1:{port}"),
            accounts[0].clone(),
        );
        let tx_hash = adapter
            .submit_transfer(&SettlementTransferRequest {
                room_id: RoomId::new(),
                hand_id: HandId::new(),
                to_seat_id: 1,
                to_address: accounts[1].clone(),
                amount: Chips(1),
            })
            .await
            .expect("submit");
        assert!(tx_hash.starts_with("0x"));

        let receipt = adapter
            .get_transfer_receipt(&tx_hash)
            .await
            .expect("receipt");
        assert!(matches!(
            receipt.status,
            SettlementTxStatus::Pending | SettlementTxStatus::Confirmed
        ));

        let _ = child.kill();
        let _ = child.wait();
    }
}
