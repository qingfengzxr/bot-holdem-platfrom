use poker_domain::{ActionSeq, Chips, HandId, HandSnapshot, HandStatus, RoomId, SeatId, Street};

#[derive(Debug, Clone)]
pub struct HandState {
    pub hand_id: HandId,
    pub street: Street,
    pub next_action_seq: ActionSeq,
    pub acting_seat_id: Option<SeatId>,
}

#[derive(Debug, Clone)]
pub struct BettingRoundState {
    pub current_bet: Chips,
    pub min_raise_to: Option<Chips>,
    pub acting_seat_id: Option<SeatId>,
}

#[derive(Debug, Clone)]
pub struct SidePot {
    pub amount: Chips,
    pub eligible_seats: Vec<SeatId>,
}

#[derive(Debug, Clone)]
pub struct PotState {
    pub main_pot: Chips,
    pub side_pots: Vec<SidePot>,
}

#[derive(Debug, Clone)]
pub struct EngineState {
    pub snapshot: HandSnapshot,
    pub hand: HandState,
    pub betting_round: BettingRoundState,
    pub pot: PotState,
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
            },
            betting_round: BettingRoundState {
                current_bet: Chips::ZERO,
                min_raise_to: None,
                acting_seat_id: None,
            },
            pot: PotState {
                main_pot: Chips::ZERO,
                side_pots: Vec::new(),
            },
        }
    }

    pub fn start(&mut self, acting_seat_id: SeatId) {
        self.snapshot.status = HandStatus::Running;
        self.snapshot.acting_seat_id = Some(acting_seat_id);
        self.hand.acting_seat_id = Some(acting_seat_id);
        self.betting_round.acting_seat_id = Some(acting_seat_id);
    }
}
