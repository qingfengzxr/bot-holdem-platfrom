use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::action::PlayerAction;
use crate::game::{SeatId, Street};
use crate::ids::{HandId, RoomId, TraceId};
use crate::money::Chips;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditBehaviorEventKind {
    SeatJoined {
        seat_id: SeatId,
    },
    SeatLeft {
        seat_id: SeatId,
    },
    SeatAddressBound {
        seat_id: SeatId,
    },
    SeatSessionKeysBound {
        seat_id: SeatId,
        key_algo: String,
    },
    TurnTimeoutAutoFold {
        seat_id: SeatId,
        action_seq: u32,
    },
    ChainTxVerificationCallback {
        seat_id: Option<SeatId>,
        action_seq: Option<u32>,
        tx_hash: String,
        status: String,
        confirmations: u64,
        failure_reason: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditBehaviorEvent {
    pub room_id: Option<RoomId>,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
    pub trace_id: TraceId,
    pub occurred_at: DateTime<Utc>,
    pub kind: AuditBehaviorEventKind,
}
