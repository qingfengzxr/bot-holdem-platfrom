use serde::{Deserialize, Serialize};

use crate::game::{ActionSeq, SeatId};
use crate::ids::{HandId, RoomId};
use crate::money::Chips;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionType {
    Fold,
    Check,
    Call,
    RaiseTo,
    AllIn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerAction {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub action_seq: ActionSeq,
    pub seat_id: SeatId,
    pub action_type: ActionType,
    pub amount: Option<Chips>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegalAction {
    pub action_type: ActionType,
    pub min_amount: Option<Chips>,
    pub max_amount: Option<Chips>,
}
