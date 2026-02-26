use serde::{Deserialize, Serialize};

use crate::ids::{HandId, RoomId};
use crate::money::Chips;

pub type SeatId = u8;
pub type ActionSeq = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Street {
    Preflop,
    Flop,
    Turn,
    River,
    Showdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandStatus {
    Created,
    Running,
    Showdown,
    Settling,
    Settled,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandSnapshot {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub hand_no: u64,
    pub status: HandStatus,
    pub street: Street,
    pub acting_seat_id: Option<SeatId>,
    pub next_action_seq: ActionSeq,
    pub pot_total: Chips,
}
