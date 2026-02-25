use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
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
pub struct SigningMessageInput<'a> {
    pub method: &'a str,
    pub session_id: SessionId,
    pub room_id: Option<RoomId>,
    pub hand_id: Option<String>,
    pub action_seq: Option<u32>,
    pub params: &'a Value,
    pub meta: &'a SignedRequestMeta,
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
}
