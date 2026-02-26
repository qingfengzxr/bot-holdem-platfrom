use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use poker_domain::{HandId, RoomId, SeatId};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use x25519_dalek::{PublicKey, StaticSecret};

const ALG_X25519_CHACHA20POLY1305: &str = "x25519+hkdf-sha256+chacha20poly1305";

#[derive(Debug, Error)]
pub enum SeatCryptoError {
    #[error("invalid base64 payload")]
    InvalidBase64(#[from] base64::DecodeError),
    #[error("invalid ephemeral public key length")]
    InvalidEphemeralPublicKeyLength,
    #[error("invalid nonce length")]
    InvalidNonceLength,
    #[error("aead error")]
    Aead,
    #[error("hkdf expansion failed")]
    Hkdf,
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
    pub ephemeral_pubkey_b64: Option<String>,
    pub nonce_b64: String,
    pub ciphertext_b64: String,
}

impl CipherEnvelope {
    #[must_use]
    pub fn new_placeholder(key_id: impl Into<String>, plaintext: &[u8]) -> Self {
        Self {
            key_id: key_id.into(),
            algorithm: "placeholder".to_string(),
            ephemeral_pubkey_b64: None,
            nonce_b64: String::new(),
            ciphertext_b64: BASE64.encode(plaintext),
        }
    }

    pub fn decode_ciphertext(&self) -> Result<Vec<u8>, SeatCryptoError> {
        Ok(BASE64.decode(&self.ciphertext_b64)?)
    }

    pub fn decrypt_with_x25519_secret(
        &self,
        recipient_secret: &[u8; 32],
        aad: &SeatEventAad,
    ) -> Result<Vec<u8>, SeatCryptoError> {
        if self.algorithm != ALG_X25519_CHACHA20POLY1305 {
            return self.decode_ciphertext();
        }

        let eph_b64 = self
            .ephemeral_pubkey_b64
            .as_ref()
            .ok_or(SeatCryptoError::InvalidEphemeralPublicKeyLength)?;
        let eph_pubkey_bytes = BASE64.decode(eph_b64)?;
        let eph_pubkey_arr: [u8; 32] = eph_pubkey_bytes
            .try_into()
            .map_err(|_| SeatCryptoError::InvalidEphemeralPublicKeyLength)?;
        let nonce_bytes = BASE64.decode(&self.nonce_b64)?;
        let nonce_arr: [u8; 12] = nonce_bytes
            .try_into()
            .map_err(|_| SeatCryptoError::InvalidNonceLength)?;
        let ciphertext = BASE64.decode(&self.ciphertext_b64)?;

        let recipient_secret = StaticSecret::from(*recipient_secret);
        let eph_pubkey = PublicKey::from(eph_pubkey_arr);
        let shared = recipient_secret.diffie_hellman(&eph_pubkey);
        let sym_key = derive_aead_key(shared.as_bytes(), &aad.as_bytes())?;

        let cipher = ChaCha20Poly1305::new(Key::from_slice(&sym_key));
        cipher
            .decrypt(
                Nonce::from_slice(&nonce_arr),
                Payload {
                    msg: &ciphertext,
                    aad: &aad.as_bytes(),
                },
            )
            .map_err(|_| SeatCryptoError::Aead)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoleCardsDealtCipherPayload {
    pub aad: SeatEventAad,
    pub envelope: CipherEnvelope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoleCardsDeliveryAuditEvent {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub seat_id: SeatId,
    pub event_seq: u32,
    pub key_id: String,
    pub algorithm: String,
    pub ciphertext_len: usize,
}

pub trait HoleCardsDeliveryAuditSink {
    fn on_hole_cards_encrypted(&self, event: &HoleCardsDeliveryAuditEvent) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X25519KeyPair {
    pub secret: [u8; 32],
    pub public: [u8; 32],
}

impl X25519KeyPair {
    #[must_use]
    pub fn generate() -> Self {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        Self {
            secret: secret.to_bytes(),
            public: public.to_bytes(),
        }
    }
}

pub fn encrypt_for_recipient_x25519(
    key_id: impl Into<String>,
    recipient_public: &[u8; 32],
    aad: &SeatEventAad,
    plaintext: &[u8],
) -> Result<CipherEnvelope, SeatCryptoError> {
    let eph_secret = StaticSecret::random_from_rng(OsRng);
    encrypt_for_recipient_x25519_with_ephemeral(
        key_id,
        recipient_public,
        aad,
        plaintext,
        eph_secret,
    )
}

pub fn encrypt_for_recipient_x25519_with_ephemeral(
    key_id: impl Into<String>,
    recipient_public: &[u8; 32],
    aad: &SeatEventAad,
    plaintext: &[u8],
    ephemeral_secret: StaticSecret,
) -> Result<CipherEnvelope, SeatCryptoError> {
    let recipient_public = PublicKey::from(*recipient_public);
    let eph_public = PublicKey::from(&ephemeral_secret);
    let shared = ephemeral_secret.diffie_hellman(&recipient_public);

    let aead_key = derive_aead_key(shared.as_bytes(), &aad.as_bytes())?;
    let nonce = derive_nonce(shared.as_bytes(), &aad.as_bytes())?;

    let cipher = ChaCha20Poly1305::new(Key::from_slice(&aead_key));
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &aad.as_bytes(),
            },
        )
        .map_err(|_| SeatCryptoError::Aead)?;

    Ok(CipherEnvelope {
        key_id: key_id.into(),
        algorithm: ALG_X25519_CHACHA20POLY1305.to_string(),
        ephemeral_pubkey_b64: Some(BASE64.encode(eph_public.as_bytes())),
        nonce_b64: BASE64.encode(nonce),
        ciphertext_b64: BASE64.encode(ciphertext),
    })
}

pub fn decrypt_with_recipient_x25519(
    recipient_secret: &[u8; 32],
    aad: &SeatEventAad,
    envelope: &CipherEnvelope,
) -> Result<Vec<u8>, SeatCryptoError> {
    envelope.decrypt_with_x25519_secret(recipient_secret, aad)
}

pub fn encrypt_hole_cards_dealt_payload(
    key_id: impl Into<String>,
    recipient_public: &[u8; 32],
    aad: SeatEventAad,
    plaintext_payload: &[u8],
) -> Result<HoleCardsDealtCipherPayload, SeatCryptoError> {
    let envelope = encrypt_for_recipient_x25519(key_id, recipient_public, &aad, plaintext_payload)?;
    Ok(HoleCardsDealtCipherPayload { aad, envelope })
}

pub fn encrypt_hole_cards_dealt_payload_with_audit(
    key_id: impl Into<String>,
    recipient_public: &[u8; 32],
    aad: SeatEventAad,
    plaintext_payload: &[u8],
    audit_sink: Option<&dyn HoleCardsDeliveryAuditSink>,
) -> Result<HoleCardsDealtCipherPayload, SeatCryptoError> {
    let key_id = key_id.into();
    let payload = encrypt_hole_cards_dealt_payload(
        key_id.clone(),
        recipient_public,
        aad.clone(),
        plaintext_payload,
    )?;
    if let Some(sink) = audit_sink {
        let _ = sink.on_hole_cards_encrypted(&HoleCardsDeliveryAuditEvent {
            room_id: aad.room_id,
            hand_id: aad.hand_id,
            seat_id: aad.seat_id,
            event_seq: aad.event_seq,
            key_id,
            algorithm: payload.envelope.algorithm.clone(),
            ciphertext_len: payload.envelope.ciphertext_b64.len(),
        });
    }
    Ok(payload)
}

fn derive_aead_key(shared_secret: &[u8], aad: &[u8]) -> Result<[u8; 32], SeatCryptoError> {
    let hk = Hkdf::<Sha256>::new(Some(b"seat-crypto-v1"), shared_secret);
    let mut out = [0_u8; 32];
    hk.expand(&[b"aead-key".as_slice(), aad].concat(), &mut out)
        .map_err(|_| SeatCryptoError::Hkdf)?;
    Ok(out)
}

fn derive_nonce(shared_secret: &[u8], aad: &[u8]) -> Result<[u8; 12], SeatCryptoError> {
    let hk = Hkdf::<Sha256>::new(Some(b"seat-crypto-v1"), shared_secret);
    let mut out = [0_u8; 12];
    hk.expand(&[b"aead-nonce".as_slice(), aad].concat(), &mut out)
        .map_err(|_| SeatCryptoError::Hkdf)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use poker_domain::{HandId, RoomId};
    use std::sync::{Arc, Mutex};

    #[test]
    fn placeholder_decode_roundtrip() {
        let env = CipherEnvelope::new_placeholder("k1", b"hello");
        let decoded = env.decode_ciphertext().expect("decode");
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn x25519_aead_roundtrip() {
        let recipient = X25519KeyPair::generate();
        let aad = SeatEventAad {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            seat_id: 2,
            event_seq: 9,
        };
        let plaintext = br#"{"cards":["As","Kd"]}"#;

        let envelope =
            encrypt_for_recipient_x25519("seat-key-1", &recipient.public, &aad, plaintext)
                .expect("encrypt");
        let decrypted =
            decrypt_with_recipient_x25519(&recipient.secret, &aad, &envelope).expect("decrypt");

        assert_eq!(decrypted, plaintext);
        assert_eq!(envelope.algorithm, ALG_X25519_CHACHA20POLY1305);
        assert!(envelope.ephemeral_pubkey_b64.is_some());
    }

    #[test]
    fn aad_mismatch_fails_decrypt() {
        let recipient = X25519KeyPair::generate();
        let aad = SeatEventAad {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            seat_id: 1,
            event_seq: 1,
        };
        let plaintext = b"secret";
        let envelope =
            encrypt_for_recipient_x25519("seat-key-1", &recipient.public, &aad, plaintext)
                .expect("encrypt");

        let wrong_aad = SeatEventAad {
            event_seq: 2,
            ..aad.clone()
        };
        let err = decrypt_with_recipient_x25519(&recipient.secret, &wrong_aad, &envelope)
            .expect_err("aad mismatch");
        assert!(matches!(err, SeatCryptoError::Aead));
    }

    #[test]
    fn hole_cards_payload_helper_encrypts_and_decrypts() {
        let recipient = X25519KeyPair::generate();
        let aad = SeatEventAad {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            seat_id: 3,
            event_seq: 1,
        };
        let plaintext = br#"{"hole_cards":["Ah","Qc"]}"#;
        let payload = encrypt_hole_cards_dealt_payload(
            "seat-key-2",
            &recipient.public,
            aad.clone(),
            plaintext,
        )
        .expect("encrypt payload");

        let decrypted = decrypt_with_recipient_x25519(&recipient.secret, &aad, &payload.envelope)
            .expect("decrypt payload");
        assert_eq!(decrypted, plaintext);
    }

    #[derive(Default)]
    struct InMemoryAuditSink {
        events: Arc<Mutex<Vec<HoleCardsDeliveryAuditEvent>>>,
    }

    impl HoleCardsDeliveryAuditSink for InMemoryAuditSink {
        fn on_hole_cards_encrypted(
            &self,
            event: &HoleCardsDeliveryAuditEvent,
        ) -> Result<(), String> {
            self.events
                .lock()
                .map_err(|_| "lock".to_string())?
                .push(event.clone());
            Ok(())
        }
    }

    #[test]
    fn hole_cards_payload_helper_with_audit_emits_event() {
        let recipient = X25519KeyPair::generate();
        let aad = SeatEventAad {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            seat_id: 4,
            event_seq: 2,
        };
        let sink = InMemoryAuditSink::default();
        let payload = encrypt_hole_cards_dealt_payload_with_audit(
            "seat-key-3",
            &recipient.public,
            aad.clone(),
            br#"{"hole_cards":["As","Kd"]}"#,
            Some(&sink),
        )
        .expect("encrypt payload");

        let events = sink.events.lock().expect("lock");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].room_id, aad.room_id);
        assert_eq!(events[0].hand_id, aad.hand_id);
        assert_eq!(events[0].seat_id, aad.seat_id);
        assert_eq!(events[0].event_seq, aad.event_seq);
        assert_eq!(events[0].algorithm, payload.envelope.algorithm);
        assert_eq!(events[0].key_id, "seat-key-3");
        assert!(events[0].ciphertext_len > 0);
    }
}
