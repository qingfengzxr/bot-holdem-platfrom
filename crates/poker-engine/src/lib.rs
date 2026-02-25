mod engine;
mod state;

pub use engine::{EngineError, PokerEngine, ShowdownInput, ShowdownResult};
pub use state::{BettingRoundState, EngineState, HandState, PotState, SidePot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineActionResult {
    Accepted,
}

#[cfg(test)]
mod tests {
    use poker_domain::{ActionType, PlayerAction, RoomId};

    use super::{EngineState, PokerEngine};

    #[test]
    fn apply_action_rejects_when_not_players_turn() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.start(0);

        let action = PlayerAction {
            room_id,
            hand_id: state.snapshot.hand_id,
            action_seq: 1,
            seat_id: 1,
            action_type: ActionType::Fold,
            amount: None,
        };

        let err = engine
            .apply_action(&mut state, action)
            .expect_err("not your turn");
        assert!(err.to_string().contains("not your turn"));
    }

    #[test]
    fn apply_action_advances_action_seq_when_accepted() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.start(0);

        let action = PlayerAction {
            room_id,
            hand_id: state.snapshot.hand_id,
            action_seq: 1,
            seat_id: 0,
            action_type: ActionType::Check,
            amount: None,
        };

        let _ = engine.apply_action(&mut state, action).expect("accepted");
        assert_eq!(state.snapshot.next_action_seq, 2);
        assert_eq!(state.hand.next_action_seq, 2);
    }
}
