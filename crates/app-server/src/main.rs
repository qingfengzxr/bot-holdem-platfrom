use anyhow::Result;
use observability::init_tracing;
use poker_domain::RoomId;
use poker_engine::PokerEngine;
use rpc_gateway::RpcGateway;
use table_service::spawn_room_actor;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("app-server");

    let _engine = PokerEngine::new();
    let gateway = RpcGateway::new();
    gateway.log_bootstrap();
    let room_id = RoomId::new();
    let _room_handle = spawn_room_actor(room_id, 128);
    info!("app-server bootstrap complete");
    Ok(())
}
