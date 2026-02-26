use agent_auth::{SigningMessageInput, build_signing_message, verify_evm_personal_signature};
use async_trait::async_trait;
use poker_domain::SessionId;
use rpc_gateway::{
    GameActRequest, RequestSignatureVerifier, RoomBindAddressRequest, RoomBindSessionKeysRequest,
    RoomReadyRequest,
};

use crate::room_service_port::AppRoomService;

#[derive(Clone)]
pub struct AppRpcRequestSignatureVerifier {
    room_service: AppRoomService,
}

impl AppRpcRequestSignatureVerifier {
    #[must_use]
    pub fn new(room_service: AppRoomService) -> Self {
        Self { room_service }
    }
}

#[async_trait]
impl RequestSignatureVerifier for AppRpcRequestSignatureVerifier {
    async fn verify_bind_address(&self, request: &RoomBindAddressRequest) -> Result<(), String> {
        let params = serde_json::json!({
            "seat_id": request.seat_id,
            "seat_address": request.seat_address,
        });
        let session_id = SessionId(uuid::Uuid::nil());
        let message = build_signing_message(&SigningMessageInput {
            method: "room.bind_address",
            session_id,
            room_id: Some(request.room_id),
            hand_id: None,
            action_seq: None,
            params: &params,
            meta: &request.request_meta,
        })
        .map_err(|e| e.to_string())?;
        verify_evm_personal_signature(
            &request.seat_address,
            message.as_bytes(),
            &request.request_meta.signature,
        )
        .map_err(|e| e.to_string())
    }

    async fn verify_bind_session_keys(
        &self,
        request: &RoomBindSessionKeysRequest,
    ) -> Result<(), String> {
        let params = serde_json::json!({
            "seat_id": request.seat_id,
            "seat_address": request.seat_address,
            "card_encrypt_pubkey": request.card_encrypt_pubkey,
            "request_verify_pubkey": request.request_verify_pubkey,
            "key_algo": request.key_algo,
            "proof_signature": request.proof_signature,
        });
        let session_id = SessionId(uuid::Uuid::nil());
        let message = build_signing_message(&SigningMessageInput {
            method: "room.bind_session_keys",
            session_id,
            room_id: Some(request.room_id),
            hand_id: None,
            action_seq: None,
            params: &params,
            meta: &request.request_meta,
        })
        .map_err(|e| e.to_string())?;
        verify_evm_personal_signature(
            &request.seat_address,
            message.as_bytes(),
            &request.request_meta.signature,
        )
        .map_err(|e| e.to_string())
    }

    async fn verify_room_ready(&self, request: &RoomReadyRequest) -> Result<(), String> {
        let seat_address = self
            .room_service
            .bound_seat_address(request.room_id, request.seat_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "missing bound seat address".to_string())?;
        let params = serde_json::json!({
            "seat_id": request.seat_id,
        });
        let session_id = SessionId(uuid::Uuid::nil());
        let message = build_signing_message(&SigningMessageInput {
            method: "room.ready",
            session_id,
            room_id: Some(request.room_id),
            hand_id: None,
            action_seq: None,
            params: &params,
            meta: &request.request_meta,
        })
        .map_err(|e| e.to_string())?;
        verify_evm_personal_signature(&seat_address, message.as_bytes(), &request.request_meta.signature)
        .map_err(|e| e.to_string())
    }

    async fn verify_game_act(&self, request: &GameActRequest) -> Result<(), String> {
        let seat_address = self
            .room_service
            .bound_seat_address(request.room_id, request.seat_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "missing bound seat address".to_string())?;

        let params = serde_json::json!({
            "seat_id": request.seat_id,
            "action_type": request.action_type,
            "amount": request.amount,
            "tx_hash": request.tx_hash,
        });
        let session_id = SessionId(uuid::Uuid::nil());
        let hand_id = Some(request.hand_id.0.to_string());
        let message = build_signing_message(&SigningMessageInput {
            method: "game.act",
            session_id,
            room_id: Some(request.room_id),
            hand_id,
            action_seq: Some(request.action_seq),
            params: &params,
            meta: &request.request_meta,
        })
        .map_err(|e| e.to_string())?;

        verify_evm_personal_signature(&seat_address, message.as_bytes(), &request.request_meta.signature)
        .map_err(|e| e.to_string())
    }
}
