pub mod action;
pub mod errors;
pub mod events;
pub mod game;
pub mod ids;
pub mod money;

pub use action::{ActionType, LegalAction, PlayerAction};
pub use errors::DomainError;
pub use events::{AuditBehaviorEvent, AuditBehaviorEventKind, HandEvent, HandEventKind};
pub use game::{ActionSeq, HandSnapshot, HandStatus, SeatId, Street};
pub use ids::{AgentId, HandId, RequestId, RoomId, SessionId, TraceId};
pub use money::{Chips, MoneyError};

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;

    #[test]
    fn action_type_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_value(ActionType::RaiseTo).expect("serialize"),
            json!("raise_to")
        );
        assert_eq!(
            serde_json::to_value(ActionType::AllIn).expect("serialize"),
            json!("all_in")
        );
    }

    #[test]
    fn street_and_hand_status_serialize_as_snake_case() {
        assert_eq!(
            serde_json::to_value(Street::Preflop).expect("serialize"),
            json!("preflop")
        );
        assert_eq!(
            serde_json::to_value(HandStatus::Settling).expect("serialize"),
            json!("settling")
        );
    }

    #[test]
    fn hand_event_kind_variant_names_are_stable_snake_case() {
        let ev = HandEvent {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            event_seq: 1,
            trace_id: TraceId::new(),
            occurred_at: Utc::now(),
            kind: HandEventKind::StreetChanged {
                street: Street::Flop,
            },
        };
        let value = serde_json::to_value(ev).expect("serialize");
        assert_eq!(value["kind"]["street_changed"]["street"], json!("flop"));
    }

    #[test]
    fn audit_behavior_event_kind_variant_names_are_stable_snake_case() {
        let ev = AuditBehaviorEvent {
            room_id: Some(RoomId::new()),
            hand_id: Some(HandId::new()),
            seat_id: Some(1),
            trace_id: TraceId::new(),
            occurred_at: Utc::now(),
            kind: AuditBehaviorEventKind::ChainTxVerificationCallback {
                seat_id: Some(1),
                action_seq: Some(2),
                tx_hash: "0xabc".to_string(),
                status: "matched".to_string(),
                confirmations: 3,
                failure_reason: None,
            },
        };
        let value = serde_json::to_value(ev).expect("serialize");
        assert_eq!(
            value["kind"]["chain_tx_verification_callback"]["tx_hash"],
            json!("0xabc")
        );
    }
}
