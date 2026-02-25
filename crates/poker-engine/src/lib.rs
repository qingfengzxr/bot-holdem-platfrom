mod engine;
mod state;

pub use engine::{EngineError, PokerEngine, ShowdownInput, ShowdownResult};
pub use state::{BettingRoundState, EngineState, HandState, PotState, SidePot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineActionResult {
    Accepted,
}
