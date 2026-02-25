use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("invalid room state")]
    InvalidRoomState,
    #[error("invalid hand state")]
    InvalidHandState,
    #[error("action is not legal")]
    ActionIllegal,
    #[error("not your turn")]
    NotYourTurn,
}
