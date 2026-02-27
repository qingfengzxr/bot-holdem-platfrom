use poker_domain::{
    ActionType, Chips, DomainError, HandEventKind, HandStatus, LegalAction, MoneyError,
    PlayerAction, SeatId, Street,
};
use rs_poker::core::{Card, Rankable};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

use crate::EngineActionResult;
use crate::state::{EngineState, PotLayer};

// Fixed blind profile for current MVP:
// SB = 0.0001 ETH, BB = 0.0002 ETH (in wei-like chip units).
pub const DEFAULT_SMALL_BLIND: Chips = Chips(100_000_000_000_000);
pub const DEFAULT_BIG_BLIND: Chips = Chips(200_000_000_000_000);

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error(transparent)]
    Money(#[from] MoneyError),
    #[error("not enough seated players to deal")]
    NotEnoughPlayersToDeal,
    #[error("deck does not contain enough cards")]
    InsufficientDeck,
    #[error("deck contains duplicate cards")]
    DuplicateCardsInDeck,
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
    pub fn legal_actions(&self, state: &EngineState, seat_id: SeatId) -> Vec<LegalAction> {
        if state.snapshot.acting_seat_id != Some(seat_id) {
            return Vec::new();
        }

        let current_bet = state.betting_round.current_bet;
        let seat_bet = state
            .betting_round
            .player_bets
            .get(&seat_id)
            .copied()
            .unwrap_or(Chips::ZERO);
        let to_call = current_bet.checked_sub(seat_bet).unwrap_or(Chips::ZERO);

        let mut actions = vec![LegalAction {
            action_type: ActionType::Fold,
            min_amount: None,
            max_amount: None,
        }];

        if to_call == Chips::ZERO {
            actions.push(LegalAction {
                action_type: ActionType::Check,
                min_amount: None,
                max_amount: None,
            });
        } else {
            actions.push(LegalAction {
                action_type: ActionType::Call,
                min_amount: Some(to_call),
                max_amount: Some(to_call),
            });
        }

        let min_raise_to = state
            .betting_round
            .min_raise_to
            .unwrap_or_else(|| current_bet.checked_add(Chips(1)).unwrap_or(current_bet));

        actions.push(LegalAction {
            action_type: ActionType::RaiseTo,
            min_amount: Some(min_raise_to),
            max_amount: None,
        });
        actions.push(LegalAction {
            action_type: ActionType::AllIn,
            min_amount: None,
            max_amount: None,
        });

        actions
    }

    pub fn deal_new_hand_internal(&self, state: &mut EngineState) -> Result<(), EngineError> {
        self.deal_new_hand_internal_with_deck(state, ordered_deck())
    }

    pub fn deal_new_hand_internal_with_deck(
        &self,
        state: &mut EngineState,
        deck: Vec<Card>,
    ) -> Result<(), EngineError> {
        let mut seats: Vec<_> = state.hand.seated_players.iter().copied().collect();
        seats.sort_unstable();
        if seats.len() < 2 {
            return Err(EngineError::NotEnoughPlayersToDeal);
        }

        let unique_count = deck
            .iter()
            .copied()
            .map(u8::from)
            .collect::<HashSet<_>>()
            .len();
        if unique_count != deck.len() {
            return Err(EngineError::DuplicateCardsInDeck);
        }

        let required_cards = seats.len() * 2 + 8; // 4 rounds: holes + 3 burns + 5 board
        if deck.len() < required_cards {
            return Err(EngineError::InsufficientDeck);
        }

        let mut iter = deck.into_iter();
        let mut hole_cards: HashMap<SeatId, [Card; 2]> = HashMap::new();
        for _round in 0..2 {
            for seat in &seats {
                let card = iter.next().ok_or(EngineError::InsufficientDeck)?;
                if let Some(pair) = hole_cards.get_mut(seat) {
                    pair[1] = card;
                } else {
                    hole_cards.insert(*seat, [card, card]);
                }
            }
        }

        let burn1 = iter.next().ok_or(EngineError::InsufficientDeck)?;
        let flop = [
            iter.next().ok_or(EngineError::InsufficientDeck)?,
            iter.next().ok_or(EngineError::InsufficientDeck)?,
            iter.next().ok_or(EngineError::InsufficientDeck)?,
        ];
        let burn2 = iter.next().ok_or(EngineError::InsufficientDeck)?;
        let turn = iter.next().ok_or(EngineError::InsufficientDeck)?;
        let burn3 = iter.next().ok_or(EngineError::InsufficientDeck)?;
        let river = iter.next().ok_or(EngineError::InsufficientDeck)?;

        let mut board_cards = Vec::with_capacity(5);
        board_cards.extend_from_slice(&flop);
        board_cards.push(turn);
        board_cards.push(river);

        state.dealing = Some(crate::DealingState {
            undealt_deck: iter.collect(),
            burn_cards: vec![burn1, burn2, burn3],
            board_cards,
            hole_cards,
        });
        let (small_blind_seat, big_blind_seat, first_to_act) = self
            .blinds_and_first_to_act(state, &seats)
            .ok_or(EngineError::NotEnoughPlayersToDeal)?;

        state.snapshot.status = HandStatus::Running;
        state.snapshot.street = Street::Preflop;
        state.hand.street = Street::Preflop;
        state.snapshot.acting_seat_id = Some(first_to_act);
        state.hand.acting_seat_id = Some(first_to_act);
        state.betting_round.acting_seat_id = Some(first_to_act);
        state.betting_round.current_bet = DEFAULT_BIG_BLIND;
        state.betting_round.min_raise_to =
            Some(
                DEFAULT_BIG_BLIND
                    .checked_add(DEFAULT_BIG_BLIND)
                    .unwrap_or(DEFAULT_BIG_BLIND),
            );
        state.betting_round.player_bets.clear();
        state.betting_round.acted_seats.clear();
        state.pot.main_pot = Chips::ZERO;
        state.pot.side_pots.clear();
        state.pot.player_contributions.clear();

        state
            .betting_round
            .player_bets
            .insert(small_blind_seat, DEFAULT_SMALL_BLIND);
        state
            .betting_round
            .player_bets
            .insert(big_blind_seat, DEFAULT_BIG_BLIND);
        add_contribution(state, small_blind_seat, DEFAULT_SMALL_BLIND)?;
        add_contribution(state, big_blind_seat, DEFAULT_BIG_BLIND)?;
        state.pot.main_pot = DEFAULT_SMALL_BLIND.checked_add(DEFAULT_BIG_BLIND)?;
        state.snapshot.pot_total = state.pot.main_pot;
        Ok(())
    }

    pub fn apply_action(
        &self,
        state: &mut EngineState,
        action: PlayerAction,
    ) -> Result<EngineActionResult, EngineError> {
        if action.action_seq != state.snapshot.next_action_seq {
            return Err(DomainError::ActionIllegal.into());
        }

        if state.snapshot.acting_seat_id != Some(action.seat_id) {
            return Err(DomainError::NotYourTurn.into());
        }

        let prior_seat_bet = state
            .betting_round
            .player_bets
            .get(&action.seat_id)
            .copied()
            .unwrap_or(Chips::ZERO);
        let current_bet = state.betting_round.current_bet;
        let to_call = current_bet
            .checked_sub(prior_seat_bet)
            .unwrap_or(Chips::ZERO);

        let mut reset_round_acted = false;
        match action.action_type {
            ActionType::Fold => {
                state.hand.folded_seats.insert(action.seat_id);
                state.betting_round.acted_seats.insert(action.seat_id);
            }
            ActionType::Check => {
                if to_call != Chips::ZERO {
                    return Err(DomainError::ActionIllegal.into());
                }
                state.betting_round.acted_seats.insert(action.seat_id);
            }
            ActionType::Call => {
                if to_call == Chips::ZERO {
                    return Err(DomainError::ActionIllegal.into());
                }
                let target = current_bet;
                let delta = to_call;
                state
                    .betting_round
                    .player_bets
                    .insert(action.seat_id, target);
                state.pot.main_pot = state.pot.main_pot.checked_add(delta)?;
                add_contribution(state, action.seat_id, delta)?;
                state.snapshot.pot_total = state.pot.main_pot;
                state.betting_round.acted_seats.insert(action.seat_id);
            }
            ActionType::RaiseTo | ActionType::AllIn => {
                let target = action.amount.ok_or(DomainError::ActionIllegal)?;
                if target <= prior_seat_bet {
                    return Err(DomainError::ActionIllegal.into());
                }
                let is_raise = target > current_bet;
                if matches!(action.action_type, ActionType::RaiseTo) {
                    if !is_raise {
                        return Err(DomainError::ActionIllegal.into());
                    }
                    if let Some(min_raise_to) = state.betting_round.min_raise_to
                        && target < min_raise_to
                    {
                        return Err(DomainError::ActionIllegal.into());
                    }
                }

                let delta = target.checked_sub(prior_seat_bet).unwrap_or(Chips::ZERO);
                state
                    .betting_round
                    .player_bets
                    .insert(action.seat_id, target);
                if is_raise {
                    state.betting_round.current_bet = target;
                    state.betting_round.min_raise_to =
                        Some(next_min_raise_to(current_bet, target));
                }
                state.pot.main_pot = state.pot.main_pot.checked_add(delta)?;
                add_contribution(state, action.seat_id, delta)?;
                state.snapshot.pot_total = state.pot.main_pot;
                if matches!(action.action_type, ActionType::AllIn) {
                    state.hand.all_in_seats.insert(action.seat_id);
                }
                if is_raise {
                    reset_round_acted = true;
                } else {
                    state.betting_round.acted_seats.insert(action.seat_id);
                }
            }
        }
        self.rebuild_side_pots(state);
        if reset_round_acted {
            state.betting_round.acted_seats.clear();
            state
                .betting_round
                .acted_seats
                .extend(state.hand.all_in_seats.iter().copied());
            state.betting_round.acted_seats.insert(action.seat_id);
        }

        state.snapshot.next_action_seq = state.snapshot.next_action_seq.saturating_add(1);
        state.hand.next_action_seq = state.snapshot.next_action_seq;
        let street_changed = self.advance_turn(state, action.seat_id);

        Ok(EngineActionResult::Accepted { street_changed })
    }

    #[must_use]
    pub fn hand_started_event_kinds(&self, state: &EngineState) -> Vec<HandEventKind> {
        let mut events = vec![HandEventKind::HandStarted];
        if let Some(seat_id) = state.snapshot.acting_seat_id {
            events.push(HandEventKind::TurnStarted { seat_id });
        }
        events
    }

    #[must_use]
    pub fn post_action_event_kinds(
        &self,
        state: &EngineState,
        action: &PlayerAction,
        result: &EngineActionResult,
    ) -> Vec<HandEventKind> {
        let mut events = vec![
            HandEventKind::ActionAccepted(action.clone()),
            HandEventKind::PotUpdated {
                pot_total: state.snapshot.pot_total,
            },
        ];
        let EngineActionResult::Accepted { street_changed } = result;
        if let Some(street) = street_changed {
            events.push(HandEventKind::StreetChanged { street: *street });
        }
        if matches!(
            state.snapshot.status,
            HandStatus::Settled | HandStatus::Aborted
        ) {
            events.push(HandEventKind::HandClosed);
        } else if let Some(seat_id) = state.snapshot.acting_seat_id {
            events.push(HandEventKind::TurnStarted { seat_id });
        }
        events
    }

    #[must_use]
    pub fn evaluate_showdown(&self, input: &ShowdownInput) -> Option<ShowdownResult> {
        self.evaluate_showdown_for_eligible(input, None)
    }

    #[must_use]
    pub fn evaluate_showdown_for_eligible(
        &self,
        input: &ShowdownInput,
        eligible_seats: Option<&[SeatId]>,
    ) -> Option<ShowdownResult> {
        if input.revealed_hole_cards.is_empty() || input.board.len() < 3 {
            return None;
        }
        let eligible_filter = eligible_seats.map(|seats| {
            let mut v = seats.to_vec();
            v.sort_unstable();
            v
        });
        let mut best_rank = None;
        let mut winners = Vec::new();

        for (seat_id, hole) in &input.revealed_hole_cards {
            if let Some(filter) = &eligible_filter
                && filter.binary_search(seat_id).is_err()
            {
                continue;
            }
            let mut cards = Vec::with_capacity(input.board.len() + 2);
            cards.extend_from_slice(&input.board);
            cards.extend_from_slice(hole);
            let rank = cards.rank();

            match best_rank {
                None => {
                    best_rank = Some(rank);
                    winners.clear();
                    winners.push(*seat_id);
                }
                Some(current_best) if rank > current_best => {
                    best_rank = Some(rank);
                    winners.clear();
                    winners.push(*seat_id);
                }
                Some(current_best) if rank == current_best => {
                    winners.push(*seat_id);
                }
                Some(_) => {}
            }
        }

        if winners.is_empty() {
            None
        } else {
            winners.sort_unstable();
            Some(ShowdownResult {
                winner_seats: winners,
            })
        }
    }

    pub fn rebuild_side_pots(&self, state: &mut EngineState) {
        state.pot.side_pots =
            build_side_pots(&state.pot.player_contributions, &state.hand.folded_seats);
    }

    #[must_use]
    pub fn pot_layers(&self, state: &EngineState) -> Vec<PotLayer> {
        build_pot_layers(&state.pot.player_contributions, &state.hand.folded_seats)
    }

    fn advance_turn(&self, state: &mut EngineState, current_seat_id: SeatId) -> Option<Street> {
        let showdown_seats = self.active_seats_sorted(state);
        if showdown_seats.len() <= 1 {
            state.snapshot.status = HandStatus::Settled;
            state.snapshot.acting_seat_id = None;
            state.hand.acting_seat_id = None;
            state.betting_round.acting_seat_id = None;
            return None;
        }

        if self.is_betting_round_complete(state, &showdown_seats) {
            return self.advance_street(state, &showdown_seats);
        }

        let actionable_seats = self.actionable_seats_sorted(state);
        if actionable_seats.is_empty() {
            state.snapshot.status = HandStatus::Showdown;
            state.snapshot.street = Street::Showdown;
            state.hand.street = Street::Showdown;
            state.snapshot.acting_seat_id = None;
            state.hand.acting_seat_id = None;
            state.betting_round.acting_seat_id = None;
            return Some(Street::Showdown);
        }

        let mut seated_sorted: Vec<_> = state.hand.seated_players.iter().copied().collect();
        seated_sorted.sort_unstable();

        let next_seat = seated_sorted
            .iter()
            .position(|seat| *seat == current_seat_id)
            .and_then(|idx| {
                (1..=seated_sorted.len()).find_map(|step| {
                    let candidate = seated_sorted[(idx + step) % seated_sorted.len()];
                    (!state.hand.folded_seats.contains(&candidate)
                        && !state.hand.all_in_seats.contains(&candidate))
                    .then_some(candidate)
                })
            })
            .unwrap_or(actionable_seats[0]);

        state.snapshot.acting_seat_id = Some(next_seat);
        state.hand.acting_seat_id = Some(next_seat);
        state.betting_round.acting_seat_id = Some(next_seat);
        None
    }

    fn active_seats_sorted(&self, state: &EngineState) -> Vec<SeatId> {
        let mut seats: Vec<_> = state
            .hand
            .seated_players
            .iter()
            .copied()
            .filter(|seat| !state.hand.folded_seats.contains(seat))
            .collect();
        seats.sort_unstable();
        seats
    }

    fn actionable_seats_sorted(&self, state: &EngineState) -> Vec<SeatId> {
        let mut seats: Vec<_> = state
            .hand
            .seated_players
            .iter()
            .copied()
            .filter(|seat| {
                !state.hand.folded_seats.contains(seat) && !state.hand.all_in_seats.contains(seat)
            })
            .collect();
        seats.sort_unstable();
        seats
    }

    fn is_betting_round_complete(&self, state: &EngineState, active_seats: &[SeatId]) -> bool {
        if active_seats.is_empty() {
            return false;
        }
        active_seats.iter().all(|seat| {
            state.betting_round.acted_seats.contains(seat)
                && (state.hand.all_in_seats.contains(seat)
                    || state
                        .betting_round
                        .player_bets
                        .get(seat)
                        .copied()
                        .unwrap_or(Chips::ZERO)
                        == state.betting_round.current_bet)
        })
    }

    fn advance_street(&self, state: &mut EngineState, active_seats: &[SeatId]) -> Option<Street> {
        let Some(next_street) = next_street(state.snapshot.street) else {
            state.snapshot.status = HandStatus::Showdown;
            state.snapshot.street = Street::Showdown;
            state.hand.street = Street::Showdown;
            state.snapshot.acting_seat_id = None;
            state.hand.acting_seat_id = None;
            state.betting_round.acting_seat_id = None;
            return Some(Street::Showdown);
        };

        state.snapshot.street = next_street;
        state.hand.street = next_street;
        state.betting_round.current_bet = Chips::ZERO;
        state.betting_round.min_raise_to = None;
        state.betting_round.player_bets.clear();
        state.betting_round.acted_seats.clear();

        let next_seat = self
            .next_active_seat_left_of_button(state, active_seats)
            .unwrap_or(active_seats[0]);
        state.snapshot.acting_seat_id = Some(next_seat);
        state.hand.acting_seat_id = Some(next_seat);
        state.betting_round.acting_seat_id = Some(next_seat);
        Some(next_street)
    }

    fn blinds_and_first_to_act(
        &self,
        state: &EngineState,
        seats_sorted: &[SeatId],
    ) -> Option<(SeatId, SeatId, SeatId)> {
        if seats_sorted.len() < 2 {
            return None;
        }
        let n = seats_sorted.len();
        let dealer_idx = state_dealer_index(state.snapshot.hand_no, n);
        if n == 2 {
            let sb = seats_sorted[dealer_idx];
            let bb = seats_sorted[(dealer_idx + 1) % n];
            Some((sb, bb, sb))
        } else {
            let sb = seats_sorted[(dealer_idx + 1) % n];
            let bb = seats_sorted[(dealer_idx + 2) % n];
            let first = seats_sorted[(dealer_idx + 3) % n];
            Some((sb, bb, first))
        }
    }

    fn next_active_seat_left_of_button(
        &self,
        state: &EngineState,
        active_seats_sorted: &[SeatId],
    ) -> Option<SeatId> {
        if active_seats_sorted.is_empty() {
            return None;
        }
        let mut seated_sorted: Vec<_> = state.hand.seated_players.iter().copied().collect();
        seated_sorted.sort_unstable();
        let dealer_idx = state_dealer_index(state.snapshot.hand_no, seated_sorted.len());
        let dealer = seated_sorted.get(dealer_idx).copied()?;
        let start = seated_sorted
            .iter()
            .position(|seat| *seat == dealer)
            .unwrap_or(0);
        for step in 1..=seated_sorted.len() {
            let candidate = seated_sorted[(start + step) % seated_sorted.len()];
            if active_seats_sorted.contains(&candidate) {
                return Some(candidate);
            }
        }
        None
    }
}

fn state_dealer_index(hand_no: u64, seat_count: usize) -> usize {
    if seat_count == 0 {
        return 0;
    }
    hand_no.saturating_sub(1) as usize % seat_count
}

fn next_min_raise_to(current_bet: Chips, target_bet: Chips) -> Chips {
    let raise_size = target_bet
        .checked_sub(current_bet)
        .unwrap_or(Chips::ZERO);
    if raise_size == Chips::ZERO {
        return target_bet;
    }
    target_bet
        .checked_add(raise_size)
        .unwrap_or(target_bet)
}

fn ordered_deck() -> Vec<Card> {
    (0_u8..52_u8).map(Card::from).collect()
}

fn next_street(current: Street) -> Option<Street> {
    match current {
        Street::Preflop => Some(Street::Flop),
        Street::Flop => Some(Street::Turn),
        Street::Turn => Some(Street::River),
        Street::River | Street::Showdown => None,
    }
}

fn add_contribution(
    state: &mut EngineState,
    seat_id: SeatId,
    delta: Chips,
) -> Result<(), EngineError> {
    if delta == Chips::ZERO {
        return Ok(());
    }
    let next = state
        .pot
        .player_contributions
        .get(&seat_id)
        .copied()
        .unwrap_or(Chips::ZERO)
        .checked_add(delta)?;
    state.pot.player_contributions.insert(seat_id, next);
    Ok(())
}

fn build_side_pots(
    contributions: &std::collections::HashMap<SeatId, Chips>,
    folded_seats: &std::collections::HashSet<SeatId>,
) -> Vec<crate::SidePot> {
    build_pot_layers(contributions, folded_seats)
        .into_iter()
        .filter(|layer| !layer.is_main)
        .map(|layer| crate::SidePot {
            amount: layer.amount,
            eligible_seats: layer.eligible_seats,
        })
        .collect()
}

fn build_pot_layers(
    contributions: &std::collections::HashMap<SeatId, Chips>,
    folded_seats: &std::collections::HashSet<SeatId>,
) -> Vec<PotLayer> {
    let mut levels: Vec<(SeatId, Chips)> = contributions
        .iter()
        .filter_map(|(seat, amount)| (*amount > Chips::ZERO).then_some((*seat, *amount)))
        .collect();
    levels.sort_by_key(|(_, amount)| amount.as_u128());
    if levels.len() <= 1 {
        return Vec::new();
    }

    let mut layers = Vec::new();
    let mut prev = Chips::ZERO;
    for (idx, (_, amount)) in levels.iter().enumerate() {
        if *amount <= prev {
            continue;
        }
        let layer = amount.checked_sub(prev).unwrap_or(Chips::ZERO);
        if layer == Chips::ZERO {
            continue;
        }
        let contributors: Vec<SeatId> = levels[idx..].iter().map(|(seat, _)| *seat).collect();
        if contributors.len() <= 1 {
            break;
        }

        let segment_amount = Chips(layer.as_u128() * contributors.len() as u128);
        let eligible_seats: Vec<SeatId> = contributors
            .iter()
            .copied()
            .filter(|seat| !folded_seats.contains(seat))
            .collect();
        let mut eligible_seats = eligible_seats;
        eligible_seats.sort_unstable();

        layers.push(PotLayer {
            amount: segment_amount,
            eligible_seats,
            is_main: idx == 0,
        });

        prev = *amount;
    }

    layers
}

impl Default for PokerEngine {
    fn default() -> Self {
        Self::new()
    }
}
