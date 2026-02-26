use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use hex::FromHex;
use k256::ecdsa::{RecoveryId, Signature as Secp256k1Signature, SigningKey as Secp256k1SigningKey, VerifyingKey as Secp256k1VerifyingKey};
use poker_domain::{AgentId, HandId, RequestId, RoomId, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha3::{Digest, Keccak256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("request expired")]
    RequestExpired,
    #[error("request timestamp too far in future")]
    RequestFromFuture,
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("invalid ethereum signature")]
    InvalidEthereumSignature,
    #[error("ethereum signature recovery failed")]
    EthereumSignatureRecoveryFailed,
    #[error("ethereum address mismatch")]
    EthereumAddressMismatch,
    #[error("replay request detected")]
    ReplayDetected,
    #[error("replay store unavailable")]
    ReplayStoreUnavailable,
    #[error("idempotency key already consumed")]
    IdempotencyKeyConflict,
    #[error("idempotency store unavailable")]
    IdempotencyStoreUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReplayNonceKey {
    pub agent_id: AgentId,
    pub request_id: RequestId,
    pub request_nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionIdempotencyKey {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub seat_id: u8,
    pub action_seq: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedRequestMeta {
    pub request_id: RequestId,
    pub request_nonce: String,
    pub request_ts: DateTime<Utc>,
    pub request_expiry_ms: i64,
    pub signature_pubkey_id: String,
    pub signature: String,
}

#[derive(Debug, Clone)]
pub struct SigningMessageInput<'a> {
    pub method: &'a str,
    pub session_id: SessionId,
    pub room_id: Option<RoomId>,
    pub hand_id: Option<String>,
    pub action_seq: Option<u32>,
    pub params: &'a Value,
    pub meta: &'a SignedRequestMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeatKeyBindingClaim {
    pub agent_id: Option<AgentId>,
    pub session_id: Option<SessionId>,
    pub room_id: RoomId,
    pub seat_id: u8,
    pub seat_address: String,
    pub card_encrypt_pubkey: String,
    pub request_verify_pubkey: String,
    pub key_algo: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

pub trait ReplayNonceStore: Send + Sync {
    fn consume_once(&self, key: ReplayNonceKey) -> Result<(), AuthError>;
}

pub trait ActionIdempotencyStore: Send + Sync {
    fn consume_once(&self, key: ActionIdempotencyKey) -> Result<(), AuthError>;
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryReplayNonceStore {
    seen: Arc<Mutex<HashSet<ReplayNonceKey>>>,
}

impl InMemoryReplayNonceStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ReplayNonceStore for InMemoryReplayNonceStore {
    fn consume_once(&self, key: ReplayNonceKey) -> Result<(), AuthError> {
        let mut seen = self
            .seen
            .lock()
            .map_err(|_| AuthError::ReplayStoreUnavailable)?;
        if !seen.insert(key) {
            return Err(AuthError::ReplayDetected);
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryActionIdempotencyStore {
    seen: Arc<Mutex<HashSet<ActionIdempotencyKey>>>,
}

impl InMemoryActionIdempotencyStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ActionIdempotencyStore for InMemoryActionIdempotencyStore {
    fn consume_once(&self, key: ActionIdempotencyKey) -> Result<(), AuthError> {
        let mut seen = self
            .seen
            .lock()
            .map_err(|_| AuthError::IdempotencyStoreUnavailable)?;
        if !seen.insert(key) {
            return Err(AuthError::IdempotencyKeyConflict);
        }
        Ok(())
    }
}

pub fn validate_request_window(
    now: DateTime<Utc>,
    request_ts: DateTime<Utc>,
    expiry_ms: i64,
    future_skew_tolerance_ms: i64,
) -> Result<(), AuthError> {
    if request_ts - now > Duration::milliseconds(future_skew_tolerance_ms) {
        return Err(AuthError::RequestFromFuture);
    }
    if now - request_ts > Duration::milliseconds(expiry_ms) {
        return Err(AuthError::RequestExpired);
    }
    Ok(())
}

pub fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize_json(v)))
                .collect::<BTreeMap<_, _>>();

            let mut out = serde_json::Map::with_capacity(sorted.len());
            for (k, v) in sorted {
                let _ = out.insert(k, v);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_json).collect()),
        _ => value.clone(),
    }
}

pub fn canonical_params_json(params: &Value) -> Result<String, AuthError> {
    let canonical = canonicalize_json(params);
    Ok(serde_json::to_string(&canonical)?)
}

pub fn build_signing_message(input: &SigningMessageInput<'_>) -> Result<String, AuthError> {
    let canonical_params = canonical_params_json(input.params)?;

    Ok(format!(
        "method={}|session_id={}|room_id={}|hand_id={}|action_seq={}|params={}|request_id={}|request_nonce={}|request_ts={}|request_expiry_ms={}",
        input.method,
        input.session_id.0,
        input.room_id.map_or_else(String::new, |v| v.0.to_string()),
        input.hand_id.clone().unwrap_or_default(),
        input.action_seq.map_or_else(String::new, |v| v.to_string()),
        canonical_params,
        input.meta.request_id.0,
        input.meta.request_nonce,
        input.meta.request_ts.to_rfc3339(),
        input.meta.request_expiry_ms
    ))
}

pub fn build_seat_key_binding_message(claim: &SeatKeyBindingClaim) -> String {
    format!(
        "room_id={}|seat_id={}|seat_address={}|card_encrypt_pubkey={}|request_verify_pubkey={}|key_algo={}|issued_at={}|expires_at={}|agent_id={}|session_id={}",
        claim.room_id.0,
        claim.seat_id,
        claim.seat_address,
        claim.card_encrypt_pubkey,
        claim.request_verify_pubkey,
        claim.key_algo,
        claim.issued_at.to_rfc3339(),
        claim
            .expires_at
            .map_or_else(String::new, |v| v.to_rfc3339()),
        claim.agent_id.map_or_else(String::new, |v| v.0.to_string()),
        claim
            .session_id
            .map_or_else(String::new, |v| v.0.to_string()),
    )
}

pub fn evm_personal_sign_hash(message: &[u8]) -> [u8; 32] {
    let prefix = format!("\x19Ethereum Signed Message:\n{}", message.len());
    let mut hasher = Keccak256::new();
    hasher.update(prefix.as_bytes());
    hasher.update(message);
    let digest = hasher.finalize();
    let mut out = [0_u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn normalize_evm_address(address: &str) -> String {
    address.trim().to_ascii_lowercase()
}

pub fn evm_address_from_secp256k1_verifying_key(key: &Secp256k1VerifyingKey) -> String {
    let encoded = key.to_encoded_point(false);
    let bytes = encoded.as_bytes();
    let digest = Keccak256::digest(&bytes[1..]);
    format!("0x{}", hex::encode(&digest[12..]))
}

pub fn verify_evm_personal_signature(
    expected_address: &str,
    message: &[u8],
    signature_hex: &str,
) -> Result<(), AuthError> {
    let sig_hex = signature_hex.trim().trim_start_matches("0x");
    let sig_bytes = <Vec<u8>>::from_hex(sig_hex).map_err(|_| AuthError::InvalidEthereumSignature)?;
    if sig_bytes.len() != 65 {
        return Err(AuthError::InvalidEthereumSignature);
    }
    let sig = Secp256k1Signature::try_from(&sig_bytes[..64])
        .map_err(|_| AuthError::InvalidEthereumSignature)?;
    let mut v = sig_bytes[64];
    if v >= 27 {
        v = v.saturating_sub(27);
    }
    let recid = RecoveryId::try_from(v).map_err(|_| AuthError::InvalidEthereumSignature)?;
    let digest = evm_personal_sign_hash(message);
    let key = Secp256k1VerifyingKey::recover_from_prehash(&digest, &sig, recid)
        .map_err(|_| AuthError::EthereumSignatureRecoveryFailed)?;
    let recovered = evm_address_from_secp256k1_verifying_key(&key);
    if normalize_evm_address(&recovered) != normalize_evm_address(expected_address) {
        return Err(AuthError::EthereumAddressMismatch);
    }
    Ok(())
}

pub fn sign_evm_personal_message_secp256k1(
    signing_key: &Secp256k1SigningKey,
    message: &[u8],
) -> Result<String, AuthError> {
    let digest = evm_personal_sign_hash(message);
    let (sig, recid) = signing_key
        .sign_prehash_recoverable(&digest)
        .map_err(|_| AuthError::InvalidEthereumSignature)?;
    let mut out = [0_u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = recid.to_byte();
    Ok(format!("0x{}", hex::encode(out)))
}

pub fn verify_seat_key_binding_proof_evm(
    claim: &SeatKeyBindingClaim,
    proof_signature_hex: &str,
) -> Result<(), AuthError> {
    let message = build_seat_key_binding_message(claim);
    verify_evm_personal_signature(&claim.seat_address, message.as_bytes(), proof_signature_hex)
}

pub fn consume_replay_nonce<S: ReplayNonceStore>(
    store: &S,
    agent_id: AgentId,
    meta: &SignedRequestMeta,
) -> Result<(), AuthError> {
    store.consume_once(ReplayNonceKey {
        agent_id,
        request_id: meta.request_id,
        request_nonce: meta.request_nonce.clone(),
    })
}

pub fn consume_action_idempotency_key<S: ActionIdempotencyStore>(
    store: &S,
    room_id: RoomId,
    hand_id: HandId,
    seat_id: u8,
    action_seq: u32,
) -> Result<(), AuthError> {
    store.consume_once(ActionIdempotencyKey {
        room_id,
        hand_id,
        seat_id,
        action_seq,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey as EvmSigningKey;

    #[test]
    fn canonical_json_sorts_object_keys_recursively() {
        let input = serde_json::json!({
            "b": 1,
            "a": { "z": 2, "y": 3 },
        });

        let actual = canonical_params_json(&input).expect("canonical json");
        assert_eq!(actual, r#"{"a":{"y":3,"z":2},"b":1}"#);
    }

    #[test]
    fn in_memory_replay_store_rejects_duplicates() {
        let store = InMemoryReplayNonceStore::new();
        let key = ReplayNonceKey {
            agent_id: AgentId::new(),
            request_id: RequestId::new(),
            request_nonce: "n1".to_string(),
        };

        store.consume_once(key.clone()).expect("first insert");
        let err = store.consume_once(key).expect_err("duplicate");
        assert_eq!(err.to_string(), AuthError::ReplayDetected.to_string());
    }

    #[test]
    fn in_memory_idempotency_store_rejects_duplicates() {
        let store = InMemoryActionIdempotencyStore::new();
        let key = ActionIdempotencyKey {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            seat_id: 1,
            action_seq: 7,
        };

        store.consume_once(key.clone()).expect("first insert");
        let err = store.consume_once(key).expect_err("duplicate");
        assert_eq!(
            err.to_string(),
            AuthError::IdempotencyKeyConflict.to_string()
        );
    }

    #[test]
    fn validate_request_window_accepts_recent_timestamp() {
        let now = Utc::now();
        let result = validate_request_window(now, now - Duration::seconds(1), 5_000, 5_000);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_request_window_rejects_expired_timestamp() {
        let now = Utc::now();
        let result = validate_request_window(now, now - Duration::seconds(30), 1_000, 5_000);
        assert!(matches!(result, Err(AuthError::RequestExpired)));
    }

    #[test]
    fn seat_key_binding_message_is_stable() {
        let claim = SeatKeyBindingClaim {
            agent_id: None,
            session_id: None,
            room_id: RoomId::new(),
            seat_id: 1,
            seat_address: "cfx:abc".to_string(),
            card_encrypt_pubkey: "encpk".to_string(),
            request_verify_pubkey: "sigpk".to_string(),
            key_algo: "x25519+ed25519".to_string(),
            issued_at: Utc::now(),
            expires_at: None,
        };

        let msg = build_seat_key_binding_message(&claim);
        assert!(msg.contains("seat_address=cfx:abc"));
        assert!(msg.contains("key_algo=x25519+ed25519"));
    }

    #[test]
    fn verify_evm_personal_signature_roundtrip() {
        let signing_key = EvmSigningKey::from_slice(&[7_u8; 32]).expect("evm key");
        let address = evm_address_from_secp256k1_verifying_key(signing_key.verifying_key());
        let message = b"hello-evm-auth";
        let signature = sign_evm_personal_message_secp256k1(&signing_key, message).expect("sign");
        verify_evm_personal_signature(&address, message, &signature).expect("verify");
    }

    #[test]
    fn verify_evm_personal_signature_rejects_mismatch_address() {
        let signing_key = EvmSigningKey::from_slice(&[7_u8; 32]).expect("evm key");
        let other_key = EvmSigningKey::from_slice(&[8_u8; 32]).expect("other key");
        let wrong_address = evm_address_from_secp256k1_verifying_key(other_key.verifying_key());
        let message = b"hello-evm-auth";
        let signature = sign_evm_personal_message_secp256k1(&signing_key, message).expect("sign");
        let err =
            verify_evm_personal_signature(&wrong_address, message, &signature).expect_err("mismatch");
        assert!(matches!(err, AuthError::EthereumAddressMismatch));
    }

    #[test]
    fn verify_seat_key_binding_proof_evm_roundtrip() {
        let signing_key = EvmSigningKey::from_slice(&[9_u8; 32]).expect("evm key");
        let seat_address = evm_address_from_secp256k1_verifying_key(signing_key.verifying_key());
        let claim = SeatKeyBindingClaim {
            agent_id: None,
            session_id: None,
            room_id: RoomId::new(),
            seat_id: 2,
            seat_address,
            card_encrypt_pubkey: "11".repeat(32),
            request_verify_pubkey: "22".repeat(32),
            key_algo: "x25519+evm".to_string(),
            issued_at: Utc::now(),
            expires_at: None,
        };
        let sig = sign_evm_personal_message_secp256k1(
            &signing_key,
            build_seat_key_binding_message(&claim).as_bytes(),
        )
        .expect("proof sig");

        verify_seat_key_binding_proof_evm(&claim, &sig).expect("verify");
    }

    #[test]
    fn verify_seat_key_binding_proof_evm_rejects_mismatch_address() {
        let signing_key = EvmSigningKey::from_slice(&[9_u8; 32]).expect("evm key");
        let other_key = EvmSigningKey::from_slice(&[10_u8; 32]).expect("other key");
        let seat_address = evm_address_from_secp256k1_verifying_key(other_key.verifying_key());
        let claim_for_sig = SeatKeyBindingClaim {
            agent_id: None,
            session_id: None,
            room_id: RoomId::new(),
            seat_id: 2,
            seat_address: evm_address_from_secp256k1_verifying_key(signing_key.verifying_key()),
            card_encrypt_pubkey: "11".repeat(32),
            request_verify_pubkey: "22".repeat(32),
            key_algo: "x25519+evm".to_string(),
            issued_at: Utc::now(),
            expires_at: None,
        };
        let sig = sign_evm_personal_message_secp256k1(
            &signing_key,
            build_seat_key_binding_message(&claim_for_sig).as_bytes(),
        )
        .expect("proof sig");

        let mismatched_claim = SeatKeyBindingClaim {
            seat_address,
            ..claim_for_sig
        };
        let err =
            verify_seat_key_binding_proof_evm(&mismatched_claim, &sig).expect_err("mismatch");
        assert!(matches!(err, AuthError::EthereumAddressMismatch));
    }
}
