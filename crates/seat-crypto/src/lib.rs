use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use poker_domain::{HandId, RoomId, SeatId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SeatCryptoError {
    #[error("invalid base64 payload")]
    InvalidBase64(#[from] base64::DecodeError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatEventAad {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub seat_id: SeatId,
    pub event_seq: u32,
}

impl SeatEventAad {
    #[must_use]
    pub fn as_bytes(&self) -> Vec<u8> {
        format!(
            "room_id={}|hand_id={}|seat_id={}|event_seq={}",
            self.room_id.0, self.hand_id.0, self.seat_id, self.event_seq
        )
        .into_bytes()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CipherEnvelope {
    pub key_id: String,
    pub algorithm: String,
    pub nonce_b64: String,
    pub ciphertext_b64: String,
}

impl CipherEnvelope {
    #[must_use]
    pub fn new_placeholder(key_id: impl Into<String>, plaintext: &[u8]) -> Self {
        Self {
            key_id: key_id.into(),
            algorithm: "placeholder".to_string(),
            nonce_b64: String::new(),
            ciphertext_b64: BASE64.encode(plaintext),
        }
    }

    pub fn decode_ciphertext(&self) -> Result<Vec<u8>, SeatCryptoError> {
        Ok(BASE64.decode(&self.ciphertext_b64)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoleCardsDealtCipherPayload {
    pub aad: SeatEventAad,
    pub envelope: CipherEnvelope,
}
