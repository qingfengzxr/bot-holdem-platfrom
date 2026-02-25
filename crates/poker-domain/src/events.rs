use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::action::PlayerAction;
use crate::game::{HandId, RoomId, SeatId, Street};
use crate::ids::TraceId;
use crate::money::Chips;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandEventKind {
    HandStarted,
    TurnStarted { seat_id: SeatId },
    ActionAccepted(PlayerAction),
    StreetChanged { street: Street },
    PotUpdated { pot_total: Chips },
    HandClosed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandEvent {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub event_seq: u32,
    pub trace_id: TraceId,
    pub occurred_at: DateTime<Utc>,
    pub kind: HandEventKind,
}
