use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use hex::FromHex;
use poker_domain::{AgentId, RequestId, RoomId, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("request expired")]
    RequestExpired,
    #[error("request timestamp too far in future")]
    RequestFromFuture,
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("invalid public key bytes")]
    InvalidPublicKey,
    #[error("invalid signature bytes")]
    InvalidSignature,
    #[error("signature verification failed")]
    SignatureVerificationFailed,
    #[error("replay request detected")]
    ReplayDetected,
    #[error("replay store unavailable")]
    ReplayStoreUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReplayNonceKey {
    pub agent_id: AgentId,
    pub request_id: RequestId,
    pub request_nonce: String,
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
pub struct SignatureMaterial<'a> {
    pub pubkey_hex: &'a str,
    pub signature_hex: &'a str,
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

pub fn verify_ed25519_signature(
    material: &SignatureMaterial<'_>,
    message: &[u8],
) -> Result<(), AuthError> {
    let pubkey_bytes =
        <[u8; 32]>::from_hex(material.pubkey_hex).map_err(|_| AuthError::InvalidPublicKey)?;
    let sig_bytes =
        <[u8; 64]>::from_hex(material.signature_hex).map_err(|_| AuthError::InvalidSignature)?;

    let verifying_key =
        VerifyingKey::from_bytes(&pubkey_bytes).map_err(|_| AuthError::InvalidPublicKey)?;
    let signature = Signature::from_slice(&sig_bytes).map_err(|_| AuthError::InvalidSignature)?;

    verifying_key
        .verify(message, &signature)
        .map_err(|_| AuthError::SignatureVerificationFailed)
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

pub fn verify_seat_key_binding_proof_ed25519(
    claim: &SeatKeyBindingClaim,
    proof_signature_hex: &str,
    proof_pubkey_hex: &str,
) -> Result<(), AuthError> {
    let message = build_seat_key_binding_message(claim);
    let material = SignatureMaterial {
        pubkey_hex: proof_pubkey_hex,
        signature_hex: proof_signature_hex,
    };
    verify_ed25519_signature(&material, message.as_bytes())
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
