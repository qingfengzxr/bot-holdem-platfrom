use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use ledger_store::{ChainTxRepository, NoopChainTxRepository, TxBindingInsert};
use rpc_gateway::{
    GameActRequest, GameGetPrivatePayloadsRequest, GameGetStateRequest, PrivatePayloadEvent,
    RoomBindAddressRequest, RoomBindSessionKeysRequest, RoomCreateRequest, RoomListRequest,
    RoomReadyRequest, RoomServicePort, RoomSummary, RpcGatewayError,
    to_player_action,
};
use table_service::{NoopRoomEventSink, RoomEventSink, RoomHandle, spawn_room_actor_with_sink};
use tracing::{debug, info, warn};

use poker_domain::{HandSnapshot, LegalAction, RoomId, SeatId};
use poker_engine::EngineState;

#[derive(Clone)]
pub struct AppRoomService {
    rooms: Arc<Mutex<HashMap<RoomId, RoomHandle>>>,
    joined_seats: Arc<Mutex<HashMap<RoomId, HashSet<SeatId>>>>,
    bound_addresses: Arc<Mutex<HashMap<RoomId, HashMap<SeatId, String>>>>,
    event_sink: Arc<dyn RoomEventSink>,
    chain_tx_repo: Arc<dyn ChainTxRepository>,
}

impl Default for AppRoomService {
    fn default() -> Self {
        Self::new()
    }
}

impl AppRoomService {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rooms: Arc::default(),
            joined_seats: Arc::default(),
            bound_addresses: Arc::default(),
            event_sink: Arc::new(NoopRoomEventSink),
            chain_tx_repo: Arc::new(NoopChainTxRepository),
        }
    }

    #[must_use]
    pub fn with_event_sink(event_sink: Arc<dyn RoomEventSink>) -> Self {
        Self {
            rooms: Arc::default(),
            joined_seats: Arc::default(),
            bound_addresses: Arc::default(),
            event_sink,
            chain_tx_repo: Arc::new(NoopChainTxRepository),
        }
    }

    #[must_use]
    pub fn with_sinks(
        event_sink: Arc<dyn RoomEventSink>,
        chain_tx_repo: Arc<dyn ChainTxRepository>,
    ) -> Self {
        Self {
            rooms: Arc::default(),
            joined_seats: Arc::default(),
            bound_addresses: Arc::default(),
            event_sink,
            chain_tx_repo,
        }
    }

    pub fn ensure_room(&self, room_id: RoomId) -> Result<RoomHandle, RpcGatewayError> {
        let mut rooms = self
            .rooms
            .lock()
            .map_err(|_| RpcGatewayError::Upstream("room map lock poisoned".to_string()))?;
        if let Some(handle) = rooms.get(&room_id) {
            return Ok(handle.clone());
        }
        let handle = spawn_room_actor_with_sink(room_id, 128, self.event_sink.clone());
        rooms.insert(room_id, handle.clone());
        info!(room_id = %room_id.0, "room actor spawned");
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

    pub fn room_ids(&self) -> Result<Vec<RoomId>, RpcGatewayError> {
        let rooms = self
            .rooms
            .lock()
            .map_err(|_| RpcGatewayError::Upstream("room map lock poisoned".to_string()))?;
        Ok(rooms.keys().copied().collect())
    }

    pub fn bound_seat_addresses(
        &self,
        room_id: RoomId,
    ) -> Result<HashMap<SeatId, String>, RpcGatewayError> {
        let all = self.bound_addresses.lock().map_err(|_| {
            RpcGatewayError::Upstream("bound address map lock poisoned".to_string())
        })?;
        Ok(all.get(&room_id).cloned().unwrap_or_default())
    }

    pub fn bound_seat_address(
        &self,
        room_id: RoomId,
        seat_id: SeatId,
    ) -> Result<Option<String>, RpcGatewayError> {
        let all = self.bound_addresses.lock().map_err(|_| {
            RpcGatewayError::Upstream("bound address map lock poisoned".to_string())
        })?;
        Ok(all.get(&room_id).and_then(|m| m.get(&seat_id)).cloned())
    }

    pub async fn get_engine_state(
        &self,
        room_id: RoomId,
    ) -> Result<Option<EngineState>, RpcGatewayError> {
        let room = self.get_room(room_id)?;
        room.get_engine_state()
            .await
            .map_err(RpcGatewayError::Upstream)
    }

    fn ensure_joined(&self, room_id: RoomId, seat_id: SeatId) -> Result<(), RpcGatewayError> {
        let joined = self
            .joined_seats
            .lock()
            .map_err(|_| RpcGatewayError::Upstream("joined seat map lock poisoned".to_string()))?;
        if joined
            .get(&room_id)
            .is_some_and(|set| set.contains(&seat_id))
        {
            Ok(())
        } else {
            warn!(room_id = %room_id.0, seat_id, "seat must join before bind/ready");
            Err(RpcGatewayError::Upstream(format!(
                "seat {seat_id} must call room.join before bind/ready"
            )))
        }
    }
}

#[async_trait]
impl RoomServicePort for AppRoomService {
    async fn create_room(&self, request: RoomCreateRequest) -> Result<RoomSummary, RpcGatewayError> {
        let room_id = request.room_id.unwrap_or_else(RoomId::new);
        let _ = self.ensure_room(room_id)?;
        Ok(RoomSummary {
            room_id,
            status: "active".to_string(),
        })
    }

    async fn list_rooms(
        &self,
        _request: RoomListRequest,
    ) -> Result<Vec<RoomSummary>, RpcGatewayError> {
        let rooms = self
            .rooms
            .lock()
            .map_err(|_| RpcGatewayError::Upstream("room map lock poisoned".to_string()))?;

        Ok(rooms
            .keys()
            .copied()
            .map(|room_id| RoomSummary {
                room_id,
                status: "active".to_string(),
            })
            .collect())
    }

    async fn join(&self, request: RoomReadyRequest) -> Result<(), RpcGatewayError> {
        let room = self.get_room(request.room_id)?;
        room.join(request.seat_id)
            .await
            .map_err(RpcGatewayError::Upstream)?;
        let mut joined = self
            .joined_seats
            .lock()
            .map_err(|_| RpcGatewayError::Upstream("joined seat map lock poisoned".to_string()))?;
        joined
            .entry(request.room_id)
            .or_default()
            .insert(request.seat_id);
        info!(
            room_id = %request.room_id.0,
            seat_id = request.seat_id,
            "room join accepted"
        );
        Ok(())
    }

    async fn ready(&self, request: RoomReadyRequest) -> Result<(), RpcGatewayError> {
        self.ensure_joined(request.room_id, request.seat_id)?;
        let room = self.get_room(request.room_id)?;
        room.ready(request.seat_id).await.map_err(RpcGatewayError::Upstream)?;
        info!(
            room_id = %request.room_id.0,
            seat_id = request.seat_id,
            "room ready accepted"
        );
        Ok(())
    }

    async fn bind_address(&self, request: RoomBindAddressRequest) -> Result<(), RpcGatewayError> {
        self.ensure_joined(request.room_id, request.seat_id)?;
        let room = self.get_room(request.room_id)?;
        {
            let mut all = self.bound_addresses.lock().map_err(|_| {
                RpcGatewayError::Upstream("bound address map lock poisoned".to_string())
            })?;
            all.entry(request.room_id)
                .or_default()
                .insert(request.seat_id, request.seat_address.clone());
        }
        room.bind_address(request.seat_id, request.seat_address)
            .await
            .map_err(RpcGatewayError::Upstream)?;
        info!(
            room_id = %request.room_id.0,
            seat_id = request.seat_id,
            "room bind_address accepted"
        );
        Ok(())
    }

    async fn bind_session_keys(
        &self,
        request: RoomBindSessionKeysRequest,
    ) -> Result<(), RpcGatewayError> {
        self.ensure_joined(request.room_id, request.seat_id)?;
        let room = self.get_room(request.room_id)?;
        room.bind_session_keys(
            request.seat_id,
            request.card_encrypt_pubkey,
            request.request_verify_pubkey,
            request.key_algo,
            request.proof_signature,
        )
        .await
        .map_err(RpcGatewayError::Upstream)?;
        info!(
            room_id = %request.room_id.0,
            seat_id = request.seat_id,
            "room bind_session_keys accepted"
        );
        Ok(())
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

    async fn get_private_payloads(
        &self,
        request: GameGetPrivatePayloadsRequest,
    ) -> Result<Vec<PrivatePayloadEvent>, RpcGatewayError> {
        let room = self.get_room(request.room_id)?;
        let events = room
            .get_private_events(request.seat_id, request.hand_id)
            .await
            .map_err(RpcGatewayError::Upstream)?;
        Ok(events
            .into_iter()
            .map(|ev| PrivatePayloadEvent {
                room_id: ev.room_id,
                hand_id: ev.hand_id,
                seat_id: ev.seat_id,
                event_name: ev.event_name,
                payload: ev.payload,
            })
            .collect())
    }

    async fn act(&self, request: GameActRequest) -> Result<(), RpcGatewayError> {
        let room = self.get_room(request.room_id)?;
        let tx_hash = request.tx_hash.clone();
        let action = to_player_action(&request)?;
        debug!(
            room_id = %request.room_id.0,
            hand_id = %request.hand_id.0,
            seat_id = request.seat_id,
            action_seq = request.action_seq,
            action_type = %request.action_type,
            has_tx_hash = request.tx_hash.is_some(),
            "game act received"
        );
        if let Some(tx_hash) = tx_hash {
            self.chain_tx_repo
                .insert_tx_binding(&TxBindingInsert {
                    tx_hash: tx_hash.clone(),
                    room_id: request.room_id,
                    hand_id: request.hand_id,
                    seat_id: request.seat_id,
                    action_seq: request.action_seq,
                    expected_amount: action.amount,
                    binding_status: "submitted".to_string(),
                    created_at: Utc::now(),
                    trace_id: poker_domain::TraceId::new(),
                })
                .await
                .map_err(|e| RpcGatewayError::Upstream(e.to_string()))?;
            room.queue_pending_chain_action(tx_hash, action)
                .await
                .map_err(RpcGatewayError::Upstream)?;
        } else {
            room.act(action).await.map_err(RpcGatewayError::Upstream)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_auth::SignedRequestMeta;
    use ledger_store::InMemoryChainTxRepository;

    fn signed_meta() -> SignedRequestMeta {
        SignedRequestMeta {
            request_id: poker_domain::RequestId::new(),
            request_nonce: "n1".to_string(),
            request_ts: Utc::now(),
            request_expiry_ms: 30_000,
            signature_pubkey_id: "k1".to_string(),
            signature: "sig".to_string(),
        }
    }

    #[tokio::test]
    async fn act_with_tx_hash_records_tx_binding() {
        let repo = Arc::new(InMemoryChainTxRepository::new());
        let service = AppRoomService::with_sinks(Arc::new(NoopRoomEventSink), repo.clone());
        let room_id = RoomId::new();
        let _room = service.ensure_room(room_id).expect("room");

        service
            .act(GameActRequest {
                room_id,
                hand_id: poker_domain::HandId::new(),
                action_seq: 1,
                seat_id: 0,
                action_type: "check".to_string(),
                amount: None,
                tx_hash: Some("0xtx-binding".to_string()),
                request_meta: signed_meta(),
            })
            .await
            .expect("act");

        assert_eq!(repo.tx_bindings_len(), 1);
    }

    #[tokio::test]
    async fn act_with_duplicate_hand_action_binding_is_rejected() {
        let repo = Arc::new(InMemoryChainTxRepository::new());
        let service = AppRoomService::with_sinks(Arc::new(NoopRoomEventSink), repo.clone());
        let room_id = RoomId::new();
        let hand_id = poker_domain::HandId::new();
        let _room = service.ensure_room(room_id).expect("room");

        let build_req = |tx_hash: &str| GameActRequest {
            room_id,
            hand_id,
            action_seq: 1,
            seat_id: 0,
            action_type: "check".to_string(),
            amount: None,
            tx_hash: Some(tx_hash.to_string()),
            request_meta: signed_meta(),
        };

        service.act(build_req("0xtx1")).await.expect("first act");
        let err = service
            .act(build_req("0xtx2"))
            .await
            .expect_err("duplicate binding");
        assert!(err.to_string().contains("duplicate tx binding"));
        assert_eq!(repo.tx_bindings_len(), 1);
    }

}
