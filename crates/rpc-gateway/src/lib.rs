use agent_auth::{SignedRequestMeta, validate_request_window};
use async_trait::async_trait;
use chrono::Utc;
use poker_domain::{
    ActionSeq, ActionType, Chips, HandId, HandSnapshot, LegalAction, PlayerAction, RoomId, SeatId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum RpcGatewayError {
    #[error("method not implemented")]
    NotImplemented,
    #[error("room not found")]
    RoomNotFound,
    #[error("invalid action type")]
    InvalidActionType,
    #[error("invalid amount")]
    InvalidAmount,
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("forbidden")]
    Forbidden,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameActRequest {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub action_seq: ActionSeq,
    pub seat_id: SeatId,
    pub action_type: String,
    pub amount: Option<String>,
    pub tx_hash: Option<String>,
    pub request_meta: SignedRequestMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameGetStateRequest {
    pub room_id: RoomId,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomBindAddressRequest {
    pub room_id: RoomId,
    pub seat_id: SeatId,
    pub seat_address: String,
    pub request_meta: SignedRequestMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomBindSessionKeysRequest {
    pub room_id: RoomId,
    pub seat_id: SeatId,
    pub seat_address: String,
    pub card_encrypt_pubkey: String,
    pub request_verify_pubkey: String,
    pub key_algo: String,
    pub proof_signature: String,
    pub request_meta: SignedRequestMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequestEnvelope {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Agent,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodScope {
    Public,
    Agent,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodPolicy {
    pub scope: MethodScope,
    pub requires_signature: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct RequestAuthContext {
    pub role: Role,
    pub has_valid_signature: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResult<T> {
    pub ok: bool,
    pub data: Option<T>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[async_trait]
pub trait RoomServicePort: Send + Sync {
    async fn bind_address(&self, request: RoomBindAddressRequest) -> Result<(), RpcGatewayError>;
    async fn bind_session_keys(
        &self,
        request: RoomBindSessionKeysRequest,
    ) -> Result<(), RpcGatewayError>;
    async fn get_state(
        &self,
        request: GameGetStateRequest,
    ) -> Result<Option<HandSnapshot>, RpcGatewayError>;
    async fn get_legal_actions(
        &self,
        room_id: RoomId,
        seat_id: SeatId,
    ) -> Result<Vec<LegalAction>, RpcGatewayError>;
    async fn act(&self, request: GameActRequest) -> Result<(), RpcGatewayError>;
}

#[derive(Debug, Default)]
pub struct RpcGateway;

impl RpcGateway {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    pub fn describe_methods(&self) -> Vec<&'static str> {
        vec![
            "auth.register_agent",
            "auth.get_challenge",
            "room.list",
            "room.join",
            "room.ready",
            "room.bind_address",
            "room.bind_session_keys",
            "game.get_state",
            "game.get_legal_actions",
            "game.act",
            "game.get_hand_history",
            "game.get_ledger",
        ]
    }

    pub fn log_bootstrap(&self) {
        info!(
            methods = self.describe_methods().len(),
            "rpc gateway initialized"
        );
    }

    pub fn method_policy(&self, method: &str) -> MethodPolicy {
        match method {
            "room.list" | "game.get_state" | "game.get_legal_actions" => MethodPolicy {
                scope: MethodScope::Agent,
                requires_signature: false,
            },
            "room.bind_address" | "room.bind_session_keys" | "room.ready" | "game.act" => {
                MethodPolicy {
                    scope: MethodScope::Agent,
                    requires_signature: true,
                }
            }
            "admin.dump_state" => MethodPolicy {
                scope: MethodScope::Admin,
                requires_signature: true,
            },
            _ => MethodPolicy {
                scope: MethodScope::Public,
                requires_signature: false,
            },
        }
    }

    pub fn authorize_method(
        &self,
        method: &str,
        ctx: RequestAuthContext,
    ) -> Result<(), RpcGatewayError> {
        let policy = self.method_policy(method);

        let role_allowed = match policy.scope {
            MethodScope::Public => true,
            MethodScope::Agent => matches!(ctx.role, Role::Agent | Role::Admin),
            MethodScope::Admin => matches!(ctx.role, Role::Admin),
        };

        if !role_allowed {
            return Err(RpcGatewayError::Forbidden);
        }
        if policy.requires_signature && !ctx.has_valid_signature {
            return Err(RpcGatewayError::Forbidden);
        }

        Ok(())
    }

    pub async fn handle_bind_address<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: RoomBindAddressRequest,
    ) -> RpcResult<()> {
        if let Err(err) = validate_request_meta(&request.request_meta) {
            return RpcResult::err("REQUEST_INVALID", err.to_string());
        }
        match room_service.bind_address(request).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err("ROOM_BIND_ADDRESS_FAILED", err.to_string()),
        }
    }

    pub async fn handle_bind_session_keys<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: RoomBindSessionKeysRequest,
    ) -> RpcResult<()> {
        if let Err(err) = validate_request_meta(&request.request_meta) {
            return RpcResult::err("REQUEST_INVALID", err.to_string());
        }
        match room_service.bind_session_keys(request).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err("ROOM_BIND_KEYS_FAILED", err.to_string()),
        }
    }

    pub async fn handle_get_state<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: GameGetStateRequest,
    ) -> RpcResult<Option<HandSnapshot>> {
        match room_service.get_state(request).await {
            Ok(state) => RpcResult::ok(state),
            Err(err) => RpcResult::err("GAME_GET_STATE_FAILED", err.to_string()),
        }
    }

    pub async fn handle_game_act<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: GameActRequest,
    ) -> RpcResult<()> {
        if let Err(err) = validate_request_meta(&request.request_meta) {
            return RpcResult::err("REQUEST_INVALID", err.to_string());
        }
        match room_service.act(request).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err("GAME_ACT_FAILED", err.to_string()),
        }
    }
}

impl<T> RpcResult<T> {
    #[must_use]
    pub fn ok(data: T) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error_code: None,
            error_message: None,
        }
    }

    #[must_use]
    pub fn err(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error_code: Some(code.into()),
            error_message: Some(message.into()),
        }
    }
}

pub fn parse_action_type(action_type: &str) -> Result<ActionType, RpcGatewayError> {
    match action_type {
        "fold" => Ok(ActionType::Fold),
        "check" => Ok(ActionType::Check),
        "call" => Ok(ActionType::Call),
        "raise_to" => Ok(ActionType::RaiseTo),
        "all_in" => Ok(ActionType::AllIn),
        _ => Err(RpcGatewayError::InvalidActionType),
    }
}

pub fn parse_amount_u128(amount: Option<&str>) -> Result<Option<Chips>, RpcGatewayError> {
    amount
        .map(|raw| {
            raw.parse::<u128>()
                .map(Chips)
                .map_err(|_| RpcGatewayError::InvalidAmount)
        })
        .transpose()
}

pub fn to_player_action(request: &GameActRequest) -> Result<PlayerAction, RpcGatewayError> {
    Ok(PlayerAction {
        room_id: request.room_id,
        hand_id: request.hand_id,
        action_seq: request.action_seq,
        seat_id: request.seat_id,
        action_type: parse_action_type(&request.action_type)?,
        amount: parse_amount_u128(request.amount.as_deref())?,
    })
}

fn validate_request_meta(meta: &SignedRequestMeta) -> Result<(), RpcGatewayError> {
    validate_request_window(Utc::now(), meta.request_ts, meta.request_expiry_ms, 5_000)
        .map_err(|err| RpcGatewayError::Upstream(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use poker_domain::{HandId, RequestId, SessionId};

    fn sample_meta() -> SignedRequestMeta {
        SignedRequestMeta {
            request_id: RequestId::new(),
            request_nonce: "nonce-1".to_string(),
            request_ts: Utc::now(),
            request_expiry_ms: 30_000,
            signature_pubkey_id: "pk1".to_string(),
            signature: "sig".to_string(),
        }
    }

    #[test]
    fn parse_action_type_maps_expected_values() {
        assert_eq!(parse_action_type("fold").expect("fold"), ActionType::Fold);
        assert_eq!(
            parse_action_type("check").expect("check"),
            ActionType::Check
        );
        assert!(parse_action_type("raise").is_err());
    }

    #[test]
    fn parse_amount_parses_u128_string() {
        assert_eq!(
            parse_amount_u128(Some("42")).expect("amount"),
            Some(Chips(42))
        );
        assert!(parse_amount_u128(Some("not-a-number")).is_err());
    }

    #[test]
    fn to_player_action_translates_request() {
        let req = GameActRequest {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            action_seq: 7,
            seat_id: 2,
            action_type: "call".to_string(),
            amount: Some("100".to_string()),
            tx_hash: Some("0xabc".to_string()),
            request_meta: sample_meta(),
        };

        let action = to_player_action(&req).expect("convert");
        assert_eq!(action.action_type, ActionType::Call);
        assert_eq!(action.amount, Some(Chips(100)));
    }

    #[test]
    fn validate_request_meta_rejects_expired_requests() {
        let mut meta = sample_meta();
        meta.request_ts = Utc::now() - Duration::minutes(10);
        meta.request_expiry_ms = 1_000;
        assert!(validate_request_meta(&meta).is_err());
    }

    #[test]
    fn rpc_result_err_builds_failure_payload() {
        let result: RpcResult<()> = RpcResult::err("ERR", "boom");
        assert!(!result.ok);
        assert_eq!(result.error_code.as_deref(), Some("ERR"));
    }

    #[test]
    fn method_policy_requires_signature_for_game_act() {
        let gw = RpcGateway::new();
        let policy = gw.method_policy("game.act");
        assert!(policy.requires_signature);
        assert_eq!(policy.scope, MethodScope::Agent);
    }

    #[test]
    fn authorize_method_rejects_missing_signature_when_required() {
        let gw = RpcGateway::new();
        let result = gw.authorize_method(
            "game.act",
            RequestAuthContext {
                role: Role::Agent,
                has_valid_signature: false,
            },
        );
        assert!(matches!(result, Err(RpcGatewayError::Forbidden)));
    }

    #[test]
    fn session_id_type_is_available_for_rpc_context() {
        let _session_id = SessionId::new();
    }
}
