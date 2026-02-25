mod room_service_port;

use anyhow::Result;
use observability::init_tracing;
use poker_domain::{RequestId, RoomId};
use poker_engine::PokerEngine;
use room_service_port::AppRoomService;
use rpc_gateway::{GameGetStateRequest, RpcGateway};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("app-server");

    let _engine = PokerEngine::new();
    let gateway = RpcGateway::new();
    gateway.log_bootstrap();
    let room_service = AppRoomService::new();
    let room_id = RoomId::new();
    let room_handle = room_service.ensure_room(room_id)?;
    room_handle.join(0).await.map_err(anyhow::Error::msg)?;
    room_handle.ready(0).await.map_err(anyhow::Error::msg)?;

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
        state_ok = state.ok,
        "demo rpc gateway flow executed"
    );
    info!("app-server bootstrap complete");
    Ok(())
}
