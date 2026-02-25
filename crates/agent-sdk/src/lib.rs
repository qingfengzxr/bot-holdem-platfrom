use async_trait::async_trait;
use chrono::Utc;
use poker_domain::{ActionType, HandId, RequestId, RoomId, SeatId, SessionId};
use rpc_gateway::{GameActRequest, GameGetStateRequest, RoomBindAddressRequest};
use seat_crypto::{HoleCardsDealtCipherPayload, SeatCryptoError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use agent_auth::SignedRequestMeta;

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
