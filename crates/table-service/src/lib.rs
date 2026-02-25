use std::collections::{HashMap, HashSet};

use poker_domain::{HandSnapshot, LegalAction, PlayerAction, RoomId, SeatId};
use poker_engine::{EngineActionResult, EngineState, PokerEngine};
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

#[derive(Debug)]
pub enum RoomCommand {
    Join {
        seat_id: SeatId,
    },
    Leave {
        seat_id: SeatId,
    },
    BindAddress {
        seat_id: SeatId,
        seat_address: String,
    },
    BindSessionKeys {
        seat_id: SeatId,
        card_encrypt_pubkey: String,
        request_verify_pubkey: String,
        key_algo: String,
        proof_signature: String,
    },
    Ready {
        seat_id: SeatId,
    },
    GetState {
        reply: oneshot::Sender<Option<HandSnapshot>>,
    },
    GetLegalActions {
        seat_id: SeatId,
        reply: oneshot::Sender<Vec<LegalAction>>,
    },
    Act {
        action: PlayerAction,
        reply: oneshot::Sender<Result<EngineActionResult, String>>,
    },
}

#[derive(Debug, Clone)]
pub struct RoomHandle {
    sender: mpsc::Sender<RoomCommand>,
}

impl RoomHandle {
    #[must_use]
    pub fn new(sender: mpsc::Sender<RoomCommand>) -> Self {
        Self { sender }
    }

    pub fn sender(&self) -> mpsc::Sender<RoomCommand> {
        self.sender.clone()
    }

    pub async fn join(&self, seat_id: SeatId) -> Result<(), String> {
        self.sender
            .send(RoomCommand::Join { seat_id })
            .await
            .map_err(|_| "room actor unavailable".to_string())
    }

    pub async fn leave(&self, seat_id: SeatId) -> Result<(), String> {
        self.sender
            .send(RoomCommand::Leave { seat_id })
            .await
            .map_err(|_| "room actor unavailable".to_string())
    }

    pub async fn bind_address(&self, seat_id: SeatId, seat_address: String) -> Result<(), String> {
        self.sender
            .send(RoomCommand::BindAddress {
                seat_id,
                seat_address,
            })
            .await
            .map_err(|_| "room actor unavailable".to_string())
    }

    pub async fn ready(&self, seat_id: SeatId) -> Result<(), String> {
        self.sender
            .send(RoomCommand::Ready { seat_id })
            .await
            .map_err(|_| "room actor unavailable".to_string())
    }

    pub async fn bind_session_keys(
        &self,
        seat_id: SeatId,
        card_encrypt_pubkey: String,
        request_verify_pubkey: String,
        key_algo: String,
        proof_signature: String,
    ) -> Result<(), String> {
        self.sender
            .send(RoomCommand::BindSessionKeys {
                seat_id,
                card_encrypt_pubkey,
                request_verify_pubkey,
                key_algo,
                proof_signature,
            })
            .await
            .map_err(|_| "room actor unavailable".to_string())
    }

    pub async fn get_state(&self) -> Result<Option<HandSnapshot>, String> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RoomCommand::GetState { reply: tx })
            .await
            .map_err(|_| "room actor unavailable".to_string())?;
        rx.await.map_err(|_| "room actor dropped reply".to_string())
    }

    pub async fn get_legal_actions(&self, seat_id: SeatId) -> Result<Vec<LegalAction>, String> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RoomCommand::GetLegalActions { seat_id, reply: tx })
            .await
            .map_err(|_| "room actor unavailable".to_string())?;
        rx.await.map_err(|_| "room actor dropped reply".to_string())
    }

    pub async fn act(&self, action: PlayerAction) -> Result<EngineActionResult, String> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RoomCommand::Act { action, reply: tx })
            .await
            .map_err(|_| "room actor unavailable".to_string())?;
        rx.await
            .map_err(|_| "room actor dropped reply".to_string())?
    }
}

#[derive(Default)]
pub struct RoomRegistry {
    rooms: HashMap<RoomId, RoomHandle>,
}

impl RoomRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, room_id: RoomId, handle: RoomHandle) {
        self.rooms.insert(room_id, handle);
    }

    pub fn get(&self, room_id: &RoomId) -> Option<RoomHandle> {
        self.rooms.get(room_id).cloned()
    }
}

pub fn spawn_room_actor(room_id: RoomId, queue_capacity: usize) -> RoomHandle {
    let (tx, mut rx) = mpsc::channel(queue_capacity);

    tokio::spawn(async move {
        let engine = PokerEngine::new();
        let mut state = EngineState::new(room_id, 1);
        let mut seated: HashSet<SeatId> = HashSet::new();
        let mut ready: HashSet<SeatId> = HashSet::new();
        let mut bound_addresses: HashMap<SeatId, String> = HashMap::new();
        let mut request_pubkeys: HashMap<SeatId, String> = HashMap::new();
        let mut card_pubkeys: HashMap<SeatId, String> = HashMap::new();

        while let Some(cmd) = rx.recv().await {
            match cmd {
                RoomCommand::Join { seat_id } => {
                    seated.insert(seat_id);
                    ready.remove(&seat_id);
                    debug!(?room_id, seat_id, "seat joined");
                }
                RoomCommand::Leave { seat_id } => {
                    seated.remove(&seat_id);
                    ready.remove(&seat_id);
                    bound_addresses.remove(&seat_id);
                    debug!(?room_id, seat_id, "seat left");
                }
                RoomCommand::BindAddress {
                    seat_id,
                    seat_address,
                } => {
                    if seated.contains(&seat_id) {
                        bound_addresses.insert(seat_id, seat_address);
                    }
                }
                RoomCommand::BindSessionKeys {
                    seat_id,
                    card_encrypt_pubkey,
                    request_verify_pubkey,
                    key_algo,
                    proof_signature,
                } => {
                    if seated.contains(&seat_id) {
                        card_pubkeys.insert(seat_id, card_encrypt_pubkey);
                        request_pubkeys.insert(seat_id, request_verify_pubkey);
                        debug!(
                            ?room_id,
                            seat_id,
                            key_algo,
                            proof_sig_len = proof_signature.len(),
                            "seat session keys bound"
                        );
                    }
                }
                RoomCommand::Ready { seat_id } => {
                    if seated.contains(&seat_id) {
                        ready.insert(seat_id);
                        let has_addr = bound_addresses.contains_key(&seat_id);
                        let has_card_key = card_pubkeys.contains_key(&seat_id);
                        let has_req_key = request_pubkeys.contains_key(&seat_id);
                        if state.snapshot.acting_seat_id.is_none()
                            && has_addr
                            && has_card_key
                            && has_req_key
                        {
                            state.start(seat_id);
                        }
                    }
                }
                RoomCommand::GetState { reply } => {
                    let _ = reply.send(Some(state.snapshot.clone()));
                }
                RoomCommand::GetLegalActions { seat_id, reply } => {
                    let legal_actions = engine.legal_actions(&state, seat_id);
                    let _ = reply.send(legal_actions);
                }
                RoomCommand::Act { action, reply } => {
                    let result = engine
                        .apply_action(&mut state, action)
                        .map_err(|err| err.to_string());
                    let _ = reply.send(result);
                }
            }
        }
    });

    RoomHandle::new(tx)
}
