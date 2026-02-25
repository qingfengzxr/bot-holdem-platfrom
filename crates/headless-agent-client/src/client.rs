use poker_domain::{ActionType, HandId, RoomId, SeatId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnContext {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub seat_id: SeatId,
    pub legal_actions: Vec<String>,
    pub action_deadline_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateCache {
    pub current_turn: Option<TurnContext>,
    pub last_public_state_json: Option<String>,
}

impl StateCache {
    pub fn update_turn(&mut self, turn: TurnContext) {
        self.current_turn = Some(turn);
    }

    pub fn clear_turn(&mut self) {
        self.current_turn = None;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FallbackPolicy {
    pub default_action: ActionType,
}

impl Default for FallbackPolicy {
    fn default() -> Self {
        Self {
            default_action: ActionType::Fold,
        }
    }
}

impl FallbackPolicy {
    pub fn choose_action(&self, legal_actions: &[String]) -> ActionType {
        let fallback = format!("{:?}", self.default_action).to_lowercase();
        if legal_actions.iter().any(|a| a == &fallback) {
            self.default_action
        } else if legal_actions.iter().any(|a| a == "check") {
            ActionType::Check
        } else {
            ActionType::Fold
        }
    }
}
