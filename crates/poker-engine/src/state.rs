use poker_domain::{ActionSeq, Chips, HandId, HandSnapshot, HandStatus, RoomId, SeatId, Street};
use rs_poker::core::Card;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct HandState {
    pub hand_id: HandId,
    pub street: Street,
    pub next_action_seq: ActionSeq,
    pub acting_seat_id: Option<SeatId>,
    pub seated_players: HashSet<SeatId>,
    pub folded_seats: HashSet<SeatId>,
    pub all_in_seats: HashSet<SeatId>,
}

#[derive(Debug, Clone)]
pub struct BettingRoundState {
    pub current_bet: Chips,
    pub min_raise_to: Option<Chips>,
    pub acting_seat_id: Option<SeatId>,
    pub player_bets: HashMap<SeatId, Chips>,
    pub acted_seats: HashSet<SeatId>,
}

#[derive(Debug, Clone)]
pub struct SidePot {
    pub amount: Chips,
    pub eligible_seats: Vec<SeatId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PotLayer {
    pub amount: Chips,
    pub eligible_seats: Vec<SeatId>,
    pub is_main: bool,
}

#[derive(Debug, Clone)]
pub struct PotState {
    pub main_pot: Chips,
    pub side_pots: Vec<SidePot>,
    pub player_contributions: HashMap<SeatId, Chips>,
}

#[derive(Debug, Clone)]
pub struct DealingState {
    pub undealt_deck: Vec<Card>,
    pub burn_cards: Vec<Card>,
    pub board_cards: Vec<Card>,
    pub hole_cards: HashMap<SeatId, [Card; 2]>,
}

#[derive(Debug, Clone)]
pub struct EngineState {
    pub snapshot: HandSnapshot,
    pub hand: HandState,
    pub betting_round: BettingRoundState,
    pub pot: PotState,
    pub dealing: Option<DealingState>,
}

impl EngineState {
    #[must_use]
    pub fn new(room_id: RoomId, hand_no: u64) -> Self {
        let hand_id = HandId::new();
        Self {
            snapshot: HandSnapshot {
                room_id,
                hand_id,
                hand_no,
                status: HandStatus::Created,
                street: Street::Preflop,
                acting_seat_id: None,
                next_action_seq: 1_u32 as ActionSeq,
                pot_total: Chips::ZERO,
            },
            hand: HandState {
                hand_id,
                street: Street::Preflop,
                next_action_seq: 1,
                acting_seat_id: None,
                seated_players: HashSet::new(),
                folded_seats: HashSet::new(),
                all_in_seats: HashSet::new(),
            },
            betting_round: BettingRoundState {
                current_bet: Chips::ZERO,
                min_raise_to: None,
                acting_seat_id: None,
                player_bets: HashMap::new(),
                acted_seats: HashSet::new(),
            },
            pot: PotState {
                main_pot: Chips::ZERO,
                side_pots: Vec::new(),
                player_contributions: HashMap::new(),
            },
            dealing: None,
        }
    }

    pub fn start(&mut self, acting_seat_id: SeatId) {
        self.snapshot.status = HandStatus::Running;
        self.snapshot.acting_seat_id = Some(acting_seat_id);
        self.hand.acting_seat_id = Some(acting_seat_id);
        self.betting_round.acting_seat_id = Some(acting_seat_id);
        self.betting_round.acted_seats.clear();
        self.hand.folded_seats.clear();
        self.hand.all_in_seats.clear();
        self.dealing = None;
        self.hand.seated_players.insert(acting_seat_id);
    }

    pub fn seat_player(&mut self, seat_id: SeatId) {
        self.hand.seated_players.insert(seat_id);
    }
}
