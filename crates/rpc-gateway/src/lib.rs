use agent_auth::SignedRequestMeta;
use poker_domain::{ActionSeq, HandId, RoomId, SeatId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum RpcGatewayError {
    #[error("method not implemented")]
    NotImplemented,
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
pub struct JsonRpcRequestEnvelope {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResult<T> {
    pub ok: bool,
    pub data: Option<T>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
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
}
