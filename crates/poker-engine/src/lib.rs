use poker_domain::Street;

mod engine;
mod state;

pub use engine::{
    DEFAULT_BIG_BLIND, DEFAULT_SMALL_BLIND, EngineError, PokerEngine, ShowdownInput,
    ShowdownResult,
};
pub use state::{
    BettingRoundState, DealingState, EngineState, HandState, PotLayer, PotState, SidePot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineActionResult {
    Accepted { street_changed: Option<Street> },
}

#[cfg(test)]
mod tests {
    use poker_domain::{ActionType, PlayerAction, RoomId, SeatId};
    use rs_poker::core::{Card, FlatHand};

    use super::{DEFAULT_BIG_BLIND, DEFAULT_SMALL_BLIND, EngineState, PokerEngine};

    fn expected_blinds_and_first_to_act(
        hand_no: u64,
        seats_sorted: &[SeatId],
    ) -> (SeatId, SeatId, SeatId) {
        let n = seats_sorted.len();
        let dealer_idx = hand_no.saturating_sub(1) as usize % n;
        if n == 2 {
            let sb = seats_sorted[dealer_idx];
            let bb = seats_sorted[(dealer_idx + 1) % n];
            (sb, bb, sb)
        } else {
            let sb = seats_sorted[(dealer_idx + 1) % n];
            let bb = seats_sorted[(dealer_idx + 2) % n];
            let first = seats_sorted[(dealer_idx + 3) % n];
            (sb, bb, first)
        }
    }

    fn assert_rotation_for_seat_count(seat_count: usize, hand_count: u64) {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let seats: Vec<SeatId> = (0..seat_count as u8).collect();
        for hand_no in 1..=hand_count {
            let mut state = EngineState::new(room_id, hand_no);
            for seat in &seats {
                state.seat_player(*seat);
            }
            engine
                .deal_new_hand_internal(&mut state)
                .expect("deal hand for rotation test");
            let (sb, bb, first) = expected_blinds_and_first_to_act(hand_no, &seats);
            assert_eq!(
                state.betting_round.player_bets.get(&sb).copied(),
                Some(DEFAULT_SMALL_BLIND)
            );
            assert_eq!(
                state.betting_round.player_bets.get(&bb).copied(),
                Some(DEFAULT_BIG_BLIND)
            );
            assert_eq!(
                state.betting_round.current_bet,
                DEFAULT_BIG_BLIND,
                "hand_no={hand_no} seat_count={seat_count}"
            );
            assert_eq!(
                state.snapshot.pot_total,
                DEFAULT_SMALL_BLIND
                    .checked_add(DEFAULT_BIG_BLIND)
                    .expect("sum blinds"),
                "hand_no={hand_no} seat_count={seat_count}"
            );
            assert_eq!(
                state.snapshot.acting_seat_id,
                Some(first),
                "hand_no={hand_no} seat_count={seat_count}"
            );
        }
    }

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
    fn hand_started_event_kinds_emit_hand_started_and_turn_started() {
        let engine = PokerEngine::new();
        let mut state = EngineState::new(RoomId::new(), 1);
        state.seat_player(0);
        state.start(0);

        let events = engine.hand_started_event_kinds(&state);
        assert!(matches!(
            events[0],
            poker_domain::HandEventKind::HandStarted
        ));
        assert!(matches!(
            events[1],
            poker_domain::HandEventKind::TurnStarted { seat_id: 0 }
        ));
    }

    #[test]
    fn internal_dealing_assigns_hole_cards_board_and_burns() {
        let engine = PokerEngine::new();
        let mut state = EngineState::new(RoomId::new(), 1);
        state.seat_player(0);
        state.seat_player(1);
        state.seat_player(3);

        engine
            .deal_new_hand_internal(&mut state)
            .expect("deal internal hand");

        let dealing = state.dealing.as_ref().expect("dealing state");
        assert_eq!(dealing.hole_cards.len(), 3);
        assert_eq!(dealing.board_cards.len(), 5);
        assert_eq!(dealing.burn_cards.len(), 3);
        assert_eq!(dealing.undealt_deck.len(), 52 - (3 * 2 + 8));

        let mut seen = std::collections::HashSet::new();
        for cards in dealing.hole_cards.values() {
            assert!(seen.insert(u8::from(cards[0])));
            assert!(seen.insert(u8::from(cards[1])));
        }
        for c in &dealing.board_cards {
            assert!(seen.insert(u8::from(*c)));
        }
        for c in &dealing.burn_cards {
            assert!(seen.insert(u8::from(*c)));
        }
    }

    #[test]
    fn internal_dealing_with_custom_deck_is_round_robin_and_deterministic() {
        let engine = PokerEngine::new();
        let mut state = EngineState::new(RoomId::new(), 1);
        state.seat_player(0);
        state.seat_player(2);
        let deck = (0_u8..52_u8).map(Card::from).collect::<Vec<_>>();

        engine
            .deal_new_hand_internal_with_deck(&mut state, deck)
            .expect("deal custom deck");
        let dealing = state.dealing.as_ref().expect("dealing");

        // Ordered deck with round-robin over seats [0,2]:
        // round1: seat0=0, seat2=1; round2: seat0=2, seat2=3
        assert_eq!(u8::from(dealing.hole_cards.get(&0).expect("seat0")[0]), 0);
        assert_eq!(u8::from(dealing.hole_cards.get(&0).expect("seat0")[1]), 2);
        assert_eq!(u8::from(dealing.hole_cards.get(&2).expect("seat2")[0]), 1);
        assert_eq!(u8::from(dealing.hole_cards.get(&2).expect("seat2")[1]), 3);
        // Burn, flop(3), burn, turn, burn, river after 4 hole cards.
        assert_eq!(u8::from(dealing.burn_cards[0]), 4);
        assert_eq!(u8::from(dealing.board_cards[0]), 5);
        assert_eq!(u8::from(dealing.board_cards[1]), 6);
        assert_eq!(u8::from(dealing.board_cards[2]), 7);
        assert_eq!(u8::from(dealing.burn_cards[1]), 8);
        assert_eq!(u8::from(dealing.board_cards[3]), 9);
        assert_eq!(u8::from(dealing.burn_cards[2]), 10);
        assert_eq!(u8::from(dealing.board_cards[4]), 11);
    }

    #[test]
    fn blind_rotation_matrix_heads_up_continuous_hands() {
        assert_rotation_for_seat_count(2, 8);
    }

    #[test]
    fn blind_rotation_matrix_three_handed_continuous_hands() {
        assert_rotation_for_seat_count(3, 9);
    }

    #[test]
    fn blind_rotation_matrix_four_handed_continuous_hands() {
        assert_rotation_for_seat_count(4, 12);
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

        let result = engine
            .apply_action(&mut state, action.clone())
            .expect("accepted");
        assert_eq!(state.snapshot.next_action_seq, 2);
        assert_eq!(state.hand.next_action_seq, 2);
        let events = engine.post_action_event_kinds(&state, &action, &result);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, poker_domain::HandEventKind::ActionAccepted(_)))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, poker_domain::HandEventKind::PotUpdated { .. }))
        );
    }

    #[test]
    fn apply_action_rejects_when_action_seq_mismatch() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.start(0);

        let action = PlayerAction {
            room_id,
            hand_id: state.snapshot.hand_id,
            action_seq: 2,
            seat_id: 0,
            action_type: ActionType::Check,
            amount: None,
        };

        let err = engine
            .apply_action(&mut state, action)
            .expect_err("mismatched action seq should fail");
        assert!(err.to_string().contains("action is not legal"));
        assert_eq!(state.snapshot.next_action_seq, 1);
    }

    #[test]
    fn legal_actions_return_check_when_no_bet_to_call() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.start(0);

        let actions = engine.legal_actions(&state, 0);
        assert!(actions.iter().any(|a| a.action_type == ActionType::Check));
        assert!(!actions.iter().any(|a| a.action_type == ActionType::Call));
    }

    #[test]
    fn legal_actions_are_empty_for_non_acting_seat() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.seat_player(0);
        state.seat_player(1);
        state.start(0);

        let actions = engine.legal_actions(&state, 1);
        assert!(actions.is_empty());
    }

    #[test]
    fn legal_actions_return_call_when_facing_bet() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.start(0);
        state.betting_round.current_bet = poker_domain::Chips(100);
        state
            .betting_round
            .player_bets
            .insert(0, poker_domain::Chips(20));

        let actions = engine.legal_actions(&state, 0);
        let call = actions
            .iter()
            .find(|a| a.action_type == ActionType::Call)
            .expect("call action");
        assert_eq!(call.min_amount, Some(poker_domain::Chips(80)));
        assert!(!actions.iter().any(|a| a.action_type == ActionType::Check));
    }

    #[test]
    fn raise_to_updates_current_bet_and_pot() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.start(0);

        let action = PlayerAction {
            room_id,
            hand_id: state.snapshot.hand_id,
            action_seq: 1,
            seat_id: 0,
            action_type: ActionType::RaiseTo,
            amount: Some(poker_domain::Chips(50)),
        };

        let _ = engine
            .apply_action(&mut state, action)
            .expect("raise accepted");
        assert_eq!(state.betting_round.current_bet, poker_domain::Chips(50));
        assert_eq!(state.pot.main_pot, poker_domain::Chips(50));
        assert_eq!(state.snapshot.pot_total, poker_domain::Chips(50));
    }

    #[test]
    fn call_only_adds_remaining_amount_to_pot() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.start(0);
        state.betting_round.current_bet = poker_domain::Chips(100);
        state
            .betting_round
            .player_bets
            .insert(0, poker_domain::Chips(40));
        state.pot.main_pot = poker_domain::Chips(140);
        state.snapshot.pot_total = poker_domain::Chips(140);

        let action = PlayerAction {
            room_id,
            hand_id: state.snapshot.hand_id,
            action_seq: 1,
            seat_id: 0,
            action_type: ActionType::Call,
            amount: None,
        };

        let _ = engine
            .apply_action(&mut state, action)
            .expect("call accepted");
        assert_eq!(
            state.betting_round.player_bets.get(&0),
            Some(&poker_domain::Chips(100))
        );
        assert_eq!(state.pot.main_pot, poker_domain::Chips(200));
        assert_eq!(state.snapshot.pot_total, poker_domain::Chips(200));
    }

    #[test]
    fn all_in_updates_bet_and_pot_like_raise() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.start(0);
        state.betting_round.current_bet = poker_domain::Chips(50);
        state
            .betting_round
            .player_bets
            .insert(0, poker_domain::Chips(20));

        let action = PlayerAction {
            room_id,
            hand_id: state.snapshot.hand_id,
            action_seq: 1,
            seat_id: 0,
            action_type: ActionType::AllIn,
            amount: Some(poker_domain::Chips(120)),
        };

        let _ = engine
            .apply_action(&mut state, action)
            .expect("all-in accepted");
        assert_eq!(state.betting_round.current_bet, poker_domain::Chips(120));
        assert_eq!(
            state.betting_round.player_bets.get(&0),
            Some(&poker_domain::Chips(120))
        );
        assert_eq!(state.pot.main_pot, poker_domain::Chips(100));
        assert_eq!(state.snapshot.pot_total, poker_domain::Chips(100));
    }

    #[test]
    fn all_in_requires_amount() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.start(0);

        let action = PlayerAction {
            room_id,
            hand_id: state.snapshot.hand_id,
            action_seq: 1,
            seat_id: 0,
            action_type: ActionType::AllIn,
            amount: None,
        };

        let err = engine
            .apply_action(&mut state, action)
            .expect_err("all-in without amount should fail");
        assert!(err.to_string().contains("action is not legal"));
    }

    #[test]
    fn action_rotates_to_next_seat_after_accept() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.seat_player(0);
        state.seat_player(1);
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
        assert_eq!(state.snapshot.acting_seat_id, Some(1));
    }

    #[test]
    fn fold_ends_hand_when_only_one_active_player_left() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.seat_player(0);
        state.seat_player(1);
        state.start(0);

        let action = PlayerAction {
            room_id,
            hand_id: state.snapshot.hand_id,
            action_seq: 1,
            seat_id: 0,
            action_type: ActionType::Fold,
            amount: None,
        };

        let _ = engine.apply_action(&mut state, action).expect("accepted");
        assert_eq!(state.snapshot.acting_seat_id, None);
        assert_eq!(state.snapshot.status, poker_domain::HandStatus::Settled);
    }

    #[test]
    fn fold_rotates_to_next_seat_in_order_when_multiple_players_remain() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.seat_player(0);
        state.seat_player(1);
        state.seat_player(2);
        state.start(1);

        let action = PlayerAction {
            room_id,
            hand_id: state.snapshot.hand_id,
            action_seq: 1,
            seat_id: 1,
            action_type: ActionType::Fold,
            amount: None,
        };

        let _ = engine.apply_action(&mut state, action).expect("accepted");
        assert_eq!(state.snapshot.status, poker_domain::HandStatus::Running);
        assert_eq!(state.snapshot.acting_seat_id, Some(2));
    }

    #[test]
    fn multi_player_action_chain_updates_pot_seq_and_turn_order() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.seat_player(0);
        state.seat_player(1);
        state.seat_player(2);
        state.start(0);
        let hand_id = state.snapshot.hand_id;

        let _ = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 1,
                    seat_id: 0,
                    action_type: ActionType::RaiseTo,
                    amount: Some(poker_domain::Chips(50)),
                },
            )
            .expect("seat 0 raise");
        assert_eq!(state.snapshot.next_action_seq, 2);
        assert_eq!(state.snapshot.acting_seat_id, Some(1));
        assert_eq!(state.pot.main_pot, poker_domain::Chips(50));

        let _ = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 2,
                    seat_id: 1,
                    action_type: ActionType::Call,
                    amount: None,
                },
            )
            .expect("seat 1 call");
        assert_eq!(state.snapshot.next_action_seq, 3);
        assert_eq!(state.snapshot.acting_seat_id, Some(2));
        assert_eq!(state.pot.main_pot, poker_domain::Chips(100));
        assert_eq!(
            state.betting_round.player_bets.get(&1),
            Some(&poker_domain::Chips(50))
        );

        let _ = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 3,
                    seat_id: 2,
                    action_type: ActionType::Fold,
                    amount: None,
                },
            )
            .expect("seat 2 fold");
        assert_eq!(state.snapshot.status, poker_domain::HandStatus::Running);
        assert_eq!(state.snapshot.next_action_seq, 4);
        assert_eq!(state.snapshot.acting_seat_id, Some(1));
        assert_eq!(state.pot.main_pot, poker_domain::Chips(100));
    }

    #[test]
    fn turn_rotation_skips_previously_folded_seat_on_later_actions() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.seat_player(0);
        state.seat_player(1);
        state.seat_player(2);
        state.start(0);

        let hand_id = state.snapshot.hand_id;
        let _ = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 1,
                    seat_id: 0,
                    action_type: ActionType::Check,
                    amount: None,
                },
            )
            .expect("seat 0 check");
        let _ = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 2,
                    seat_id: 1,
                    action_type: ActionType::Fold,
                    amount: None,
                },
            )
            .expect("seat 1 fold");
        assert_eq!(state.snapshot.acting_seat_id, Some(2));

        let _ = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 3,
                    seat_id: 2,
                    action_type: ActionType::Check,
                    amount: None,
                },
            )
            .expect("seat 2 check");

        assert_eq!(state.snapshot.acting_seat_id, Some(2));
        assert!(!state.hand.folded_seats.contains(&0));
        assert!(state.hand.folded_seats.contains(&1));
    }

    #[test]
    fn evaluate_showdown_returns_none_without_revealed_hole_cards() {
        let engine = PokerEngine::new();
        let result = engine.evaluate_showdown(&super::ShowdownInput {
            board: Vec::new(),
            revealed_hole_cards: Vec::new(),
        });
        assert!(result.is_none());
    }

    #[test]
    fn check_is_rejected_when_facing_bet() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.start(0);
        state.betting_round.current_bet = poker_domain::Chips(20);

        let action = PlayerAction {
            room_id,
            hand_id: state.snapshot.hand_id,
            action_seq: 1,
            seat_id: 0,
            action_type: ActionType::Check,
            amount: None,
        };

        let err = engine
            .apply_action(&mut state, action)
            .expect_err("illegal");
        assert!(err.to_string().contains("action is not legal"));
    }

    #[test]
    fn call_is_rejected_when_no_bet_to_call() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.start(0);

        let action = PlayerAction {
            room_id,
            hand_id: state.snapshot.hand_id,
            action_seq: 1,
            seat_id: 0,
            action_type: ActionType::Call,
            amount: None,
        };

        let err = engine
            .apply_action(&mut state, action)
            .expect_err("illegal");
        assert!(err.to_string().contains("action is not legal"));
    }

    #[test]
    fn raise_to_rejects_amount_below_min_raise() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.start(0);
        state.betting_round.current_bet = poker_domain::Chips(50);
        state.betting_round.min_raise_to = Some(poker_domain::Chips(100));

        let action = PlayerAction {
            room_id,
            hand_id: state.snapshot.hand_id,
            action_seq: 1,
            seat_id: 0,
            action_type: ActionType::RaiseTo,
            amount: Some(poker_domain::Chips(80)),
        };

        let err = engine
            .apply_action(&mut state, action)
            .expect_err("illegal");
        assert!(err.to_string().contains("action is not legal"));
    }

    #[test]
    fn showdown_picks_higher_ranked_hand() {
        let engine = PokerEngine::new();
        let board = FlatHand::new_from_str("AhKhQh2c3d")
            .expect("board")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let p1 = FlatHand::new_from_str("JhTh").expect("p1");
        let p2 = FlatHand::new_from_str("AdAc").expect("p2");

        let result = engine
            .evaluate_showdown(&super::ShowdownInput {
                board,
                revealed_hole_cards: vec![(0, [p1[0], p1[1]]), (1, [p2[0], p2[1]])],
            })
            .expect("showdown result");

        assert_eq!(result.winner_seats, vec![0]);
    }

    #[test]
    fn showdown_returns_multiple_winners_on_tie() {
        let engine = PokerEngine::new();
        let board = FlatHand::new_from_str("AhKhQhJhTh")
            .expect("board")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let p1 = FlatHand::new_from_str("2c3d").expect("p1");
        let p2 = FlatHand::new_from_str("4s5c").expect("p2");

        let result = engine
            .evaluate_showdown(&super::ShowdownInput {
                board,
                revealed_hole_cards: vec![(2, [p2[0], p2[1]]), (1, [p1[0], p1[1]])],
            })
            .expect("showdown result");

        assert_eq!(result.winner_seats, vec![1, 2]);
    }

    #[test]
    fn betting_round_completes_and_advances_to_flop() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.seat_player(0);
        state.seat_player(1);
        state.start(0);
        let hand_id = state.snapshot.hand_id;

        let result = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 1,
                    seat_id: 0,
                    action_type: ActionType::Check,
                    amount: None,
                },
            )
            .expect("seat 0 check");
        assert!(matches!(
            result,
            super::EngineActionResult::Accepted {
                street_changed: None
            }
        ));

        let result = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 2,
                    seat_id: 1,
                    action_type: ActionType::Check,
                    amount: None,
                },
            )
            .expect("seat 1 check");

        assert!(matches!(
            result,
            super::EngineActionResult::Accepted {
                street_changed: Some(poker_domain::Street::Flop)
            }
        ));
        assert_eq!(state.snapshot.street, poker_domain::Street::Flop);
        assert_eq!(state.betting_round.current_bet, poker_domain::Chips::ZERO);
        assert_eq!(state.snapshot.status, poker_domain::HandStatus::Running);
        assert_eq!(state.snapshot.acting_seat_id, Some(1));
    }

    #[test]
    fn river_round_completion_transitions_to_showdown_without_closing_hand() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.seat_player(0);
        state.seat_player(1);
        state.start(0);
        state.snapshot.street = poker_domain::Street::River;
        state.hand.street = poker_domain::Street::River;
        let hand_id = state.snapshot.hand_id;

        let _ = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 1,
                    seat_id: 0,
                    action_type: ActionType::Check,
                    amount: None,
                },
            )
            .expect("seat 0 check");
        let result = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 2,
                    seat_id: 1,
                    action_type: ActionType::Check,
                    amount: None,
                },
            )
            .expect("seat 1 check");

        assert!(matches!(
            result,
            super::EngineActionResult::Accepted {
                street_changed: Some(poker_domain::Street::Showdown)
            }
        ));
        assert_eq!(state.snapshot.street, poker_domain::Street::Showdown);
        assert_eq!(state.snapshot.status, poker_domain::HandStatus::Showdown);
        assert_eq!(state.snapshot.acting_seat_id, None);
    }

    #[test]
    fn uneven_contributions_rebuild_side_pots() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        let _ = room_id;
        state
            .pot
            .player_contributions
            .insert(0, poker_domain::Chips(100));
        state
            .pot
            .player_contributions
            .insert(1, poker_domain::Chips(100));
        state
            .pot
            .player_contributions
            .insert(2, poker_domain::Chips(40));
        state.pot.main_pot = poker_domain::Chips(240);
        engine.rebuild_side_pots(&mut state);
        assert_eq!(state.pot.main_pot, poker_domain::Chips(240));
        assert_eq!(
            state.pot.player_contributions.get(&0).copied(),
            Some(poker_domain::Chips(100))
        );
        assert_eq!(
            state.pot.player_contributions.get(&1).copied(),
            Some(poker_domain::Chips(100))
        );
        assert_eq!(
            state.pot.player_contributions.get(&2).copied(),
            Some(poker_domain::Chips(40))
        );
        assert_eq!(state.pot.side_pots.len(), 1);
        assert_eq!(state.pot.side_pots[0].amount, poker_domain::Chips(120));
        assert_eq!(state.pot.side_pots[0].eligible_seats, vec![0, 1]);
    }

    #[test]
    fn folded_seat_is_removed_from_side_pot_eligibility() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.hand.seated_players.extend([0, 1, 2]);
        state.hand.folded_seats.insert(1);
        state
            .pot
            .player_contributions
            .insert(0, poker_domain::Chips(100));
        state
            .pot
            .player_contributions
            .insert(1, poker_domain::Chips(100));
        state
            .pot
            .player_contributions
            .insert(2, poker_domain::Chips(40));
        state.pot.main_pot = poker_domain::Chips(240);

        let _ = room_id;
        engine.rebuild_side_pots(&mut state);

        assert_eq!(state.pot.side_pots.len(), 1);
        assert_eq!(state.pot.side_pots[0].eligible_seats, vec![0]);
    }

    #[test]
    fn short_all_in_below_current_bet_is_accepted_and_marks_seat_all_in() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.seat_player(0);
        state.seat_player(1);
        state.seat_player(2);
        state.start(2);
        state.betting_round.current_bet = poker_domain::Chips(100);
        state.betting_round.min_raise_to = Some(poker_domain::Chips(200));
        let hand_id = state.snapshot.hand_id;

        let result = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 1,
                    seat_id: 2,
                    action_type: ActionType::AllIn,
                    amount: Some(poker_domain::Chips(40)),
                },
            )
            .expect("short all-in should be accepted");

        assert!(matches!(
            result,
            super::EngineActionResult::Accepted {
                street_changed: None
            }
        ));
        assert_eq!(state.betting_round.current_bet, poker_domain::Chips(100));
        assert_eq!(
            state.betting_round.player_bets.get(&2).copied(),
            Some(poker_domain::Chips(40))
        );
        assert!(state.hand.all_in_seats.contains(&2));
        assert_ne!(state.snapshot.acting_seat_id, Some(2));
    }

    #[test]
    fn short_all_in_seat_is_exempt_from_matching_current_bet_for_round_completion() {
        let engine = PokerEngine::new();
        let room_id = RoomId::new();
        let mut state = EngineState::new(room_id, 1);
        state.seat_player(0);
        state.seat_player(1);
        state.seat_player(2);
        state.start(2);
        let hand_id = state.snapshot.hand_id;

        let _ = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 1,
                    seat_id: 2,
                    action_type: ActionType::AllIn,
                    amount: Some(poker_domain::Chips(40)),
                },
            )
            .expect("seat2 all-in");
        let _ = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 2,
                    seat_id: 0,
                    action_type: ActionType::RaiseTo,
                    amount: Some(poker_domain::Chips(100)),
                },
            )
            .expect("seat0 raise");
        let result = engine
            .apply_action(
                &mut state,
                PlayerAction {
                    room_id,
                    hand_id,
                    action_seq: 3,
                    seat_id: 1,
                    action_type: ActionType::Call,
                    amount: None,
                },
            )
            .expect("seat1 call");

        assert!(matches!(
            result,
            super::EngineActionResult::Accepted {
                street_changed: Some(poker_domain::Street::Flop)
            }
        ));
        assert_eq!(state.snapshot.street, poker_domain::Street::Flop);
        assert_eq!(state.pot.main_pot, poker_domain::Chips(240));
        assert_eq!(state.pot.side_pots.len(), 1);
        assert_eq!(state.pot.side_pots[0].amount, poker_domain::Chips(120));
        assert_eq!(state.pot.side_pots[0].eligible_seats, vec![0, 1]);
    }

    #[test]
    fn pot_layers_include_main_and_side_segments() {
        let engine = PokerEngine::new();
        let mut state = EngineState::new(RoomId::new(), 1);
        state
            .pot
            .player_contributions
            .insert(0, poker_domain::Chips(100));
        state
            .pot
            .player_contributions
            .insert(1, poker_domain::Chips(100));
        state
            .pot
            .player_contributions
            .insert(2, poker_domain::Chips(40));

        let layers = engine.pot_layers(&state);
        assert_eq!(layers.len(), 2);
        assert!(layers[0].is_main);
        assert_eq!(layers[0].amount, poker_domain::Chips(120));
        assert_eq!(layers[0].eligible_seats, vec![0, 1, 2]);
        assert!(!layers[1].is_main);
        assert_eq!(layers[1].amount, poker_domain::Chips(120));
        assert_eq!(layers[1].eligible_seats, vec![0, 1]);
    }

    #[test]
    fn showdown_for_eligible_seats_filters_non_eligible_players() {
        let engine = PokerEngine::new();
        let board = FlatHand::new_from_str("AhKhQh2c3d")
            .expect("board")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let strong = FlatHand::new_from_str("JhTh").expect("strong"); // royal flush
        let medium = FlatHand::new_from_str("AdAc").expect("medium");
        let weak = FlatHand::new_from_str("4c4d").expect("weak");
        let input = super::ShowdownInput {
            board,
            revealed_hole_cards: vec![
                (0, [strong[0], strong[1]]),
                (1, [medium[0], medium[1]]),
                (2, [weak[0], weak[1]]),
            ],
        };

        let filtered = engine
            .evaluate_showdown_for_eligible(&input, Some(&[1, 2]))
            .expect("result");
        assert_eq!(filtered.winner_seats, vec![1]);
    }

    #[test]
    fn pot_layers_split_multiple_side_pot_levels_deterministically() {
        let engine = PokerEngine::new();
        let mut state = EngineState::new(RoomId::new(), 1);
        state
            .pot
            .player_contributions
            .insert(0, poker_domain::Chips(300));
        state
            .pot
            .player_contributions
            .insert(1, poker_domain::Chips(200));
        state
            .pot
            .player_contributions
            .insert(2, poker_domain::Chips(100));
        state
            .pot
            .player_contributions
            .insert(3, poker_domain::Chips(50));

        let layers = engine.pot_layers(&state);
        // The engine does not emit a final single-eligible segment because it is uncontested.
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0].amount, poker_domain::Chips(200));
        assert_eq!(layers[0].eligible_seats, vec![0, 1, 2, 3]);
        assert!(layers[0].is_main);
        assert_eq!(layers[1].amount, poker_domain::Chips(150));
        assert_eq!(layers[1].eligible_seats, vec![0, 1, 2]);
        assert!(!layers[1].is_main);
        assert_eq!(layers[2].amount, poker_domain::Chips(200));
        assert_eq!(layers[2].eligible_seats, vec![0, 1]);
    }

    #[test]
    fn showdown_for_eligible_seats_preserves_tie_within_subset() {
        let engine = PokerEngine::new();
        let board = FlatHand::new_from_str("AhKhQhJhTh")
            .expect("board")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let s0 = FlatHand::new_from_str("2c3d").expect("s0");
        let s1 = FlatHand::new_from_str("4s5c").expect("s1");
        let s2 = FlatHand::new_from_str("AdAc").expect("s2");
        let input = super::ShowdownInput {
            board,
            revealed_hole_cards: vec![
                (0, [s0[0], s0[1]]),
                (1, [s1[0], s1[1]]),
                (2, [s2[0], s2[1]]),
            ],
        };

        let result = engine
            .evaluate_showdown_for_eligible(&input, Some(&[0, 1]))
            .expect("subset tie result");
        assert_eq!(result.winner_seats, vec![0, 1]);
    }
}
