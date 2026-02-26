use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use agent_auth::SignedRequestMeta;
use async_trait::async_trait;
use chrono::Utc;
use poker_domain::{ActionType, HandId, RequestId, RoomId, SeatId, SessionId};
use rpc_gateway::{
    GameActRequest, GameGetLegalActionsRequest, GameGetStateRequest, RoomBindAddressRequest,
    RoomBindSessionKeysRequest, RoomReadyRequest,
};
use seat_crypto::{HoleCardsDealtCipherPayload, SeatCryptoError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum AgentSkillError {
    #[error("not connected")]
    NotConnected,
    #[error("seat context missing")]
    MissingSeatContext,
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("seat crypto error: {0}")]
    SeatCrypto(#[from] SeatCryptoError),
    #[error("action not in legal action set")]
    ActionNotLegal,
    #[error("duplicate action attempt detected")]
    DuplicateActionAttempt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkillConfig {
    pub endpoint_http: String,
    pub endpoint_ws: Option<String>,
    pub session_id: Option<SessionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeatContext {
    pub room_id: RoomId,
    pub seat_id: SeatId,
    pub seat_address: Option<String>,
    pub request_signature_pubkey_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub room_id: RoomId,
    pub hand_id: Option<HandId>,
    pub seat_id: SeatId,
    pub legal_actions: Vec<String>,
    pub public_state: Value,
    pub private_payloads: Vec<HoleCardsDealtCipherPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionAttemptKey {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub seat_id: SeatId,
    pub action_seq: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetryDecision {
    Retry,
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub retry_on_timeout: bool,
    pub retry_on_transport_error: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            retry_on_timeout: true,
            retry_on_transport_error: true,
        }
    }
}

impl RetryPolicy {
    #[must_use]
    pub fn should_retry(&self, attempt_no: u32, reason: RetryableErrorKind) -> RetryDecision {
        if attempt_no >= self.max_attempts {
            return RetryDecision::Stop;
        }

        let allowed = match reason {
            RetryableErrorKind::Timeout => self.retry_on_timeout,
            RetryableErrorKind::Transport => self.retry_on_transport_error,
        };

        if allowed {
            RetryDecision::Retry
        } else {
            RetryDecision::Stop
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetryableErrorKind {
    Timeout,
    Transport,
}

#[derive(Debug, Default, Clone)]
pub struct LocalActionAttemptStore {
    seen: Arc<Mutex<HashSet<ActionAttemptKey>>>,
}

impl LocalActionAttemptStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reserve(&self, key: ActionAttemptKey) -> Result<(), AgentSkillError> {
        let mut guard = self
            .seen
            .lock()
            .map_err(|_| AgentSkillError::DuplicateActionAttempt)?;
        if !guard.insert(key) {
            return Err(AgentSkillError::DuplicateActionAttempt);
        }
        Ok(())
    }
}

#[async_trait]
pub trait AgentSkill {
    async fn connect(&mut self, config: AgentSkillConfig) -> Result<(), AgentSkillError>;
    async fn join_room(&mut self, room_id: RoomId, seat_id: SeatId) -> Result<(), AgentSkillError>;
    async fn bind_seat_address(
        &mut self,
        seat_address: String,
    ) -> Result<RoomBindAddressRequest, AgentSkillError>;
    async fn observe(&self) -> Result<Observation, AgentSkillError>;
    async fn build_game_act_request(
        &self,
        hand_id: HandId,
        action_seq: u32,
        action_type: ActionType,
        amount: Option<String>,
        tx_hash: Option<String>,
    ) -> Result<GameActRequest, AgentSkillError>;
}

#[derive(Debug, Default)]
pub struct LocalAgentSkill {
    config: Option<AgentSkillConfig>,
    seat_ctx: Option<SeatContext>,
    session_id: Option<SessionId>,
    retry_policy: RetryPolicy,
    action_attempts: LocalActionAttemptStore,
}

impl LocalAgentSkill {
    fn signed_meta(&self) -> Result<SignedRequestMeta, AgentSkillError> {
        let seat_ctx = self
            .seat_ctx
            .as_ref()
            .ok_or(AgentSkillError::MissingSeatContext)?;
        Ok(SignedRequestMeta {
            request_id: RequestId::new(),
            request_nonce: Uuid::now_v7().to_string(),
            request_ts: Utc::now(),
            request_expiry_ms: 30_000,
            signature_pubkey_id: seat_ctx
                .request_signature_pubkey_id
                .clone()
                .unwrap_or_else(|| "unset".to_string()),
            signature: String::new(),
        })
    }

    pub fn build_get_state_request(
        &self,
        hand_id: Option<HandId>,
    ) -> Result<GameGetStateRequest, AgentSkillError> {
        let seat_ctx = self
            .seat_ctx
            .as_ref()
            .ok_or(AgentSkillError::MissingSeatContext)?;
        Ok(GameGetStateRequest {
            room_id: seat_ctx.room_id,
            hand_id,
            seat_id: Some(seat_ctx.seat_id),
        })
    }

    pub fn build_get_legal_actions_request(&self) -> Result<GameGetLegalActionsRequest, AgentSkillError> {
        let seat_ctx = self
            .seat_ctx
            .as_ref()
            .ok_or(AgentSkillError::MissingSeatContext)?;
        Ok(GameGetLegalActionsRequest {
            room_id: seat_ctx.room_id,
            seat_id: seat_ctx.seat_id,
        })
    }

    pub fn build_room_ready_request(&self) -> Result<RoomReadyRequest, AgentSkillError> {
        let seat_ctx = self
            .seat_ctx
            .as_ref()
            .ok_or(AgentSkillError::MissingSeatContext)?;
        Ok(RoomReadyRequest {
            room_id: seat_ctx.room_id,
            seat_id: seat_ctx.seat_id,
            request_meta: self.signed_meta()?,
        })
    }

    pub fn build_bind_session_keys_request_unsigned(
        &mut self,
        card_encrypt_pubkey_hex: String,
        key_algo: String,
    ) -> Result<RoomBindSessionKeysRequest, AgentSkillError> {
        let mut request_meta = self.signed_meta()?;
        let seat_ctx = self
            .seat_ctx
            .as_mut()
            .ok_or(AgentSkillError::MissingSeatContext)?;
        let seat_address = seat_ctx
            .seat_address
            .clone()
            .ok_or(AgentSkillError::MissingSeatContext)?;
        request_meta.signature_pubkey_id = seat_address.clone();
        Ok(RoomBindSessionKeysRequest {
            room_id: seat_ctx.room_id,
            seat_id: seat_ctx.seat_id,
            seat_address,
            card_encrypt_pubkey: card_encrypt_pubkey_hex,
            request_verify_pubkey: String::new(),
            key_algo,
            proof_signature: String::new(),
            request_meta,
        })
    }

    pub fn decrypt_private_payloads_placeholder(
        &self,
        payloads: &[HoleCardsDealtCipherPayload],
    ) -> Result<Vec<Vec<u8>>, AgentSkillError> {
        let seat_ctx = self
            .seat_ctx
            .as_ref()
            .ok_or(AgentSkillError::MissingSeatContext)?;

        payloads
            .iter()
            .filter(|p| p.aad.room_id == seat_ctx.room_id && p.aad.seat_id == seat_ctx.seat_id)
            .map(|p| {
                p.envelope
                    .decode_ciphertext()
                    .map_err(AgentSkillError::from)
            })
            .collect()
    }

    pub fn validate_action_choice(
        &self,
        legal_actions: &[String],
        action_type: ActionType,
    ) -> Result<(), AgentSkillError> {
        let normalized = format!("{action_type:?}").to_lowercase();
        if legal_actions.iter().any(|a| a == &normalized) {
            Ok(())
        } else {
            Err(AgentSkillError::ActionNotLegal)
        }
    }

    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.retry_policy
    }

    pub fn set_retry_policy(&mut self, retry_policy: RetryPolicy) {
        self.retry_policy = retry_policy;
    }

    pub fn reserve_action_attempt(
        &self,
        hand_id: HandId,
        action_seq: u32,
    ) -> Result<(), AgentSkillError> {
        let seat_ctx = self
            .seat_ctx
            .as_ref()
            .ok_or(AgentSkillError::MissingSeatContext)?;
        self.action_attempts.reserve(ActionAttemptKey {
            room_id: seat_ctx.room_id,
            hand_id,
            seat_id: seat_ctx.seat_id,
            action_seq,
        })
    }
}

#[async_trait]
impl AgentSkill for LocalAgentSkill {
    async fn connect(&mut self, config: AgentSkillConfig) -> Result<(), AgentSkillError> {
        self.session_id = config.session_id;
        self.config = Some(config);
        Ok(())
    }

    async fn join_room(&mut self, room_id: RoomId, seat_id: SeatId) -> Result<(), AgentSkillError> {
        if self.config.is_none() {
            return Err(AgentSkillError::NotConnected);
        }
        self.seat_ctx = Some(SeatContext {
            room_id,
            seat_id,
            seat_address: None,
            request_signature_pubkey_id: None,
        });
        Ok(())
    }

    async fn bind_seat_address(
        &mut self,
        seat_address: String,
    ) -> Result<RoomBindAddressRequest, AgentSkillError> {
        let meta = self.signed_meta()?;
        let seat_ctx = self
            .seat_ctx
            .as_mut()
            .ok_or(AgentSkillError::MissingSeatContext)?;
        seat_ctx.seat_address = Some(seat_address.clone());
        Ok(RoomBindAddressRequest {
            room_id: seat_ctx.room_id,
            seat_id: seat_ctx.seat_id,
            seat_address,
            request_meta: meta,
        })
    }

    async fn observe(&self) -> Result<Observation, AgentSkillError> {
        let seat_ctx = self
            .seat_ctx
            .as_ref()
            .ok_or(AgentSkillError::MissingSeatContext)?;
        Ok(Observation {
            room_id: seat_ctx.room_id,
            hand_id: None,
            seat_id: seat_ctx.seat_id,
            legal_actions: Vec::new(),
            public_state: serde_json::json!({}),
            private_payloads: Vec::new(),
        })
    }

    async fn build_game_act_request(
        &self,
        hand_id: HandId,
        action_seq: u32,
        action_type: ActionType,
        amount: Option<String>,
        tx_hash: Option<String>,
    ) -> Result<GameActRequest, AgentSkillError> {
        let seat_ctx = self
            .seat_ctx
            .as_ref()
            .ok_or(AgentSkillError::MissingSeatContext)?;
        self.reserve_action_attempt(hand_id, action_seq)?;
        Ok(GameActRequest {
            room_id: seat_ctx.room_id,
            hand_id,
            action_seq,
            seat_id: seat_ctx.seat_id,
            action_type: format!("{action_type:?}").to_lowercase(),
            amount,
            tx_hash,
            request_meta: self.signed_meta()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_respects_attempt_budget() {
        let policy = RetryPolicy {
            max_attempts: 2,
            ..RetryPolicy::default()
        };

        assert_eq!(
            policy.should_retry(1, RetryableErrorKind::Timeout),
            RetryDecision::Retry
        );
        assert_eq!(
            policy.should_retry(2, RetryableErrorKind::Timeout),
            RetryDecision::Stop
        );
    }

    #[test]
    fn local_action_attempt_store_rejects_duplicate_key() {
        let store = LocalActionAttemptStore::new();
        let key = ActionAttemptKey {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            seat_id: 1,
            action_seq: 3,
        };

        store.reserve(key.clone()).expect("first");
        let err = store.reserve(key).expect_err("duplicate");
        assert!(matches!(err, AgentSkillError::DuplicateActionAttempt));
    }

    #[tokio::test]
    async fn local_skill_builds_ready_and_bind_session_keys_requests() {
        let mut skill = LocalAgentSkill::default();
        skill.connect(AgentSkillConfig {
            endpoint_http: "http://127.0.0.1:9000".to_string(),
            endpoint_ws: None,
            session_id: None,
        })
        .await
        .expect("connect");
        let room_id = RoomId::new();
        skill.join_room(room_id, 1).await.expect("join");
        let _bind_addr = skill
            .bind_seat_address("0xabc".to_string())
            .await
            .expect("bind seat addr");

        let ready = skill.build_room_ready_request().expect("ready req");
        assert_eq!(ready.room_id, room_id);
        assert_eq!(ready.seat_id, 1);

        let bind_keys = skill
            .build_bind_session_keys_request_unsigned("11".repeat(32), "x25519+evm".to_string())
            .expect("bind keys req");
        assert_eq!(bind_keys.room_id, room_id);
        assert_eq!(bind_keys.seat_id, 1);
        assert_eq!(bind_keys.seat_address, "0xabc");
        assert!(bind_keys.proof_signature.is_empty());
        assert!(bind_keys.request_verify_pubkey.is_empty());
    }

    #[tokio::test]
    async fn local_skill_builds_game_act_request() {
        let mut skill = LocalAgentSkill::default();
        skill.connect(AgentSkillConfig {
            endpoint_http: "http://127.0.0.1:9000".to_string(),
            endpoint_ws: None,
            session_id: None,
        })
        .await
        .expect("connect");
        let room_id = RoomId::new();
        skill.join_room(room_id, 0).await.expect("join");
        let _ = skill
            .bind_seat_address("0xabc".to_string())
            .await
            .expect("bind address");
        let _ = skill
            .build_bind_session_keys_request_unsigned("11".repeat(32), "x25519+evm".to_string())
            .expect("bind keys");
        let req = skill
            .build_game_act_request(
                HandId::new(),
                1,
                ActionType::Check,
                None,
                Some("0xtx".to_string()),
            )
            .await
            .expect("act");
        assert_eq!(req.action_type, "check");
    }
}
