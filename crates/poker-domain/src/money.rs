use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Chips(pub u128);

impl Chips {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub fn as_u128(self) -> u128 {
        self.0
    }

    pub fn checked_add(self, rhs: Self) -> Result<Self, MoneyError> {
        self.0
            .checked_add(rhs.0)
            .map(Self)
            .ok_or(MoneyError::Overflow)
    }

    pub fn checked_sub(self, rhs: Self) -> Result<Self, MoneyError> {
        self.0
            .checked_sub(rhs.0)
            .map(Self)
            .ok_or(MoneyError::Underflow)
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum MoneyError {
    #[error("amount overflow")]
    Overflow,
    #[error("amount underflow")]
    Underflow,
}
