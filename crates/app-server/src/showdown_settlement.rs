use poker_engine::{EngineState, PokerEngine, ShowdownInput};
use settlement::{PotAwardInput, RakePolicy, SettlementPlan, SettlementService};

pub fn build_settlement_plan_from_showdown<L>(
    engine: &PokerEngine,
    settlement_service: &SettlementService<L>,
    state: &EngineState,
    showdown_input: &ShowdownInput,
    rake_policy: RakePolicy,
) -> Result<SettlementPlan, String>
where
    L: ledger_store::LedgerRepository,
{
    let layers = engine.pot_layers(state);
    if layers.is_empty() && state.snapshot.pot_total.as_u128() > 0 {
        return Err("pot_total > 0 but no pot layers derived".to_string());
    }

    let mut pot_awards = Vec::with_capacity(layers.len());
    for layer in layers {
        if layer.amount.as_u128() == 0 {
            continue;
        }
        let showdown = engine
            .evaluate_showdown_for_eligible(showdown_input, Some(&layer.eligible_seats))
            .ok_or_else(|| "unable to evaluate showdown for pot layer".to_string())?;
        pot_awards.push(PotAwardInput {
            amount: layer.amount,
            eligible_seats: layer.eligible_seats,
            winner_seats: showdown.winner_seats,
        });
    }

    settlement_service
        .build_plan_from_pot_awards(
            state.snapshot.room_id,
            state.snapshot.hand_id,
            &pot_awards,
            rake_policy,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ledger_store::NoopLedgerRepository;
    use poker_domain::{Chips, RoomId};
    use rs_poker::core::FlatHand;

    #[test]
    fn builds_settlement_plan_from_main_and_side_pots_via_showdown() {
        let engine = PokerEngine::new();
        let settlement = SettlementService::new(NoopLedgerRepository);
        let mut state = EngineState::new(RoomId::new(), 1);
        state.pot.player_contributions.insert(0, Chips(100));
        state.pot.player_contributions.insert(1, Chips(100));
        state.pot.player_contributions.insert(2, Chips(40));
        state.pot.main_pot = Chips(240);
        state.snapshot.pot_total = Chips(240);
        state.hand.folded_seats.clear();

        let board = FlatHand::new_from_str("AhKhQh2c3d")
            .expect("board")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let seat0 = FlatHand::new_from_str("4c4d").expect("s0"); // weak in side pot
        let seat1 = FlatHand::new_from_str("AdAc").expect("s1"); // wins side pot [0,1]
        let seat2 = FlatHand::new_from_str("JhTh").expect("s2"); // wins main pot but not side pot eligible
        let showdown = ShowdownInput {
            board,
            revealed_hole_cards: vec![
                (0, [seat0[0], seat0[1]]),
                (1, [seat1[0], seat1[1]]),
                (2, [seat2[0], seat2[1]]),
            ],
        };

        let plan = build_settlement_plan_from_showdown(
            &engine,
            &settlement,
            &state,
            &showdown,
            RakePolicy::zero(),
        )
        .expect("plan");

        assert_eq!(plan.payout_total().expect("sum"), Chips(240));
        assert_eq!(plan.payouts.len(), 2);
        assert_eq!(plan.payouts[0].seat_id, 1);
        assert_eq!(plan.payouts[0].amount, Chips(120));
        assert_eq!(plan.payouts[1].seat_id, 2);
        assert_eq!(plan.payouts[1].amount, Chips(120));
    }
}
