mod room_service_port;

use agent_auth::SignedRequestMeta;
use anyhow::Result;
use chrono::Utc;
use observability::init_tracing;
use poker_domain::{RequestId, RoomId};
use poker_engine::PokerEngine;
use room_service_port::AppRoomService;
use rpc_gateway::{
    GameGetStateRequest, RoomBindAddressRequest, RoomBindSessionKeysRequest, RoomReadyRequest,
    RpcGateway,
};
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("app-server");

    let _engine = PokerEngine::new();
    let gateway = RpcGateway::new();
    gateway.log_bootstrap();
    let room_service = AppRoomService::new();
    let rpc_module = gateway.build_rpc_module(Arc::new(room_service.clone()))?;
    let room_id = RoomId::new();
    let room_handle = room_service.ensure_room(room_id)?;
    room_handle.join(0).await.map_err(anyhow::Error::msg)?;

    let signed_meta = || SignedRequestMeta {
        request_id: RequestId::new(),
        request_nonce: RequestId::new().0.to_string(),
        request_ts: Utc::now(),
        request_expiry_ms: 30_000,
        signature_pubkey_id: "demo-key".to_string(),
        signature: String::new(),
    };

    let _ = gateway
        .handle_bind_address(
            &room_service,
            RoomBindAddressRequest {
                room_id,
                seat_id: 0,
                seat_address: "cfx:demo-seat".to_string(),
                request_meta: signed_meta(),
            },
        )
        .await;
    let _ = gateway
        .handle_bind_session_keys(
            &room_service,
            RoomBindSessionKeysRequest {
                room_id,
                seat_id: 0,
                seat_address: "cfx:demo-seat".to_string(),
                card_encrypt_pubkey: "deadbeef".to_string(),
                request_verify_pubkey: "deadbeef".to_string(),
                key_algo: "x25519+ed25519".to_string(),
                proof_signature: "deadbeef".to_string(),
                request_meta: signed_meta(),
            },
        )
        .await;
    let _ = gateway
        .handle_room_ready(
            &room_service,
            RoomReadyRequest {
                room_id,
                seat_id: 0,
                request_meta: signed_meta(),
            },
        )
        .await;

    let state = gateway
        .handle_get_state(
            &room_service,
            GameGetStateRequest {
                room_id,
                hand_id: None,
                seat_id: Some(0),
            },
        )
        .await;
    info!(
        request_id = %RequestId::new().0,
        rpc_methods = rpc_module.method_names().count(),
        state_ok = state.ok,
        "demo rpc gateway flow executed"
    );
    info!("app-server bootstrap complete");
    Ok(())
}
