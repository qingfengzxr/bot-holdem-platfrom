use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rpc_gateway::{
    GameActRequest, GameGetStateRequest, RoomBindAddressRequest, RoomBindSessionKeysRequest,
    RoomServicePort, RpcGatewayError, to_player_action,
};
use table_service::{RoomHandle, spawn_room_actor};

use poker_domain::{HandSnapshot, LegalAction, RoomId, SeatId};

#[derive(Clone, Default)]
pub struct AppRoomService {
    rooms: Arc<Mutex<HashMap<RoomId, RoomHandle>>>,
}

impl AppRoomService {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ensure_room(&self, room_id: RoomId) -> Result<RoomHandle, RpcGatewayError> {
        let mut rooms = self
            .rooms
            .lock()
            .map_err(|_| RpcGatewayError::Upstream("room map lock poisoned".to_string()))?;
        if let Some(handle) = rooms.get(&room_id) {
            return Ok(handle.clone());
        }
        let handle = spawn_room_actor(room_id, 128);
        rooms.insert(room_id, handle.clone());
        Ok(handle)
    }

    fn get_room(&self, room_id: RoomId) -> Result<RoomHandle, RpcGatewayError> {
        let rooms = self
            .rooms
            .lock()
            .map_err(|_| RpcGatewayError::Upstream("room map lock poisoned".to_string()))?;
        rooms
            .get(&room_id)
            .cloned()
            .ok_or(RpcGatewayError::RoomNotFound)
    }
}

#[async_trait]
impl RoomServicePort for AppRoomService {
    async fn bind_address(&self, request: RoomBindAddressRequest) -> Result<(), RpcGatewayError> {
        let room = self.get_room(request.room_id)?;
        room.bind_address(request.seat_id, request.seat_address)
            .await
            .map_err(RpcGatewayError::Upstream)
    }

    async fn bind_session_keys(
        &self,
        request: RoomBindSessionKeysRequest,
    ) -> Result<(), RpcGatewayError> {
        let room = self.get_room(request.room_id)?;
        room.bind_session_keys(
            request.seat_id,
            request.card_encrypt_pubkey,
            request.request_verify_pubkey,
            request.key_algo,
            request.proof_signature,
        )
        .await
        .map_err(RpcGatewayError::Upstream)
    }

    async fn get_state(
        &self,
        request: GameGetStateRequest,
    ) -> Result<Option<HandSnapshot>, RpcGatewayError> {
        let room = self.get_room(request.room_id)?;
        room.get_state().await.map_err(RpcGatewayError::Upstream)
    }

    async fn get_legal_actions(
        &self,
        room_id: RoomId,
        seat_id: SeatId,
    ) -> Result<Vec<LegalAction>, RpcGatewayError> {
        let room = self.get_room(room_id)?;
        room.get_legal_actions(seat_id)
            .await
            .map_err(RpcGatewayError::Upstream)
    }

    async fn act(&self, request: GameActRequest) -> Result<(), RpcGatewayError> {
        let room = self.get_room(request.room_id)?;
        let action = to_player_action(&request)?;
        room.act(action).await.map_err(RpcGatewayError::Upstream)?;
        Ok(())
    }
}
