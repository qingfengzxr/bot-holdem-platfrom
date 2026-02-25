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
                RoomCommand::Ready { seat_id } => {
                    if seated.contains(&seat_id) {
                        ready.insert(seat_id);
                        if state.snapshot.acting_seat_id.is_none() {
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
