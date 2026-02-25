use poker_domain::{ActionType, DomainError, LegalAction, PlayerAction, SeatId};
use rs_poker::core::Card;
use thiserror::Error;

use crate::EngineActionResult;
use crate::state::EngineState;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Domain(#[from] DomainError),
}

#[derive(Debug, Clone)]
pub struct PokerEngine;

#[derive(Debug, Clone)]
pub struct ShowdownInput {
    pub board: Vec<Card>,
    pub revealed_hole_cards: Vec<(SeatId, [Card; 2])>,
}

#[derive(Debug, Clone)]
pub struct ShowdownResult {
    pub winner_seats: Vec<SeatId>,
}

impl PokerEngine {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn legal_actions(&self, _state: &EngineState, _seat_id: SeatId) -> Vec<LegalAction> {
        vec![
            LegalAction {
                action_type: ActionType::Fold,
                min_amount: None,
                max_amount: None,
            },
            LegalAction {
                action_type: ActionType::Check,
                min_amount: None,
                max_amount: None,
            },
        ]
    }

    pub fn apply_action(
        &self,
        state: &mut EngineState,
        action: PlayerAction,
    ) -> Result<EngineActionResult, EngineError> {
        if state.snapshot.acting_seat_id != Some(action.seat_id) {
            return Err(DomainError::NotYourTurn.into());
        }

        state.snapshot.next_action_seq = state.snapshot.next_action_seq.saturating_add(1);
        state.hand.next_action_seq = state.snapshot.next_action_seq;

        Ok(EngineActionResult::Accepted)
    }

    #[must_use]
    pub fn evaluate_showdown(&self, input: &ShowdownInput) -> Option<ShowdownResult> {
        if input.revealed_hole_cards.is_empty() {
            return None;
        }

        // Placeholder: actual showdown evaluation will compare hand ranks using rs-poker.
        let winner = input.revealed_hole_cards[0].0;
        Some(ShowdownResult {
            winner_seats: vec![winner],
        })
    }
}
