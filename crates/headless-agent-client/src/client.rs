use poker_domain::{ActionType, HandId, RoomId, SeatId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnContext {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub seat_id: SeatId,
    pub legal_actions: Vec<String>,
    pub action_deadline_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateCache {
    pub current_turn: Option<TurnContext>,
    pub last_public_state_json: Option<String>,
}

impl StateCache {
    pub fn update_turn(&mut self, turn: TurnContext) {
        self.current_turn = Some(turn);
    }

    pub fn clear_turn(&mut self) {
        self.current_turn = None;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionSpec {
    pub topic: String,
    pub room_id: RoomId,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionState {
    pub spec: SubscriptionSpec,
    pub remote_subscription_id: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ReconnectPolicy {
    pub max_retries: u32,
    pub base_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_retries: 10,
            base_backoff_ms: 500,
            max_backoff_ms: 10_000,
        }
    }
}

impl ReconnectPolicy {
    #[must_use]
    pub fn backoff_ms(&self, attempt: u32) -> u64 {
        let exp = self
            .base_backoff_ms
            .saturating_mul(2_u64.saturating_pow(attempt.min(10)));
        exp.min(self.max_backoff_ms)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WsSessionState {
    pub connected: bool,
    pub reconnect_attempt: u32,
    pub subscriptions: HashMap<String, SubscriptionState>,
}

impl WsSessionState {
    pub fn mark_connected(&mut self) {
        self.connected = true;
        self.reconnect_attempt = 0;
        for sub in self.subscriptions.values_mut() {
            sub.active = false;
            sub.remote_subscription_id = None;
        }
    }

    pub fn mark_disconnected(&mut self) {
        self.connected = false;
        self.reconnect_attempt = self.reconnect_attempt.saturating_add(1);
        for sub in self.subscriptions.values_mut() {
            sub.active = false;
            sub.remote_subscription_id = None;
        }
    }

    pub fn register_subscription(&mut self, local_id: String, spec: SubscriptionSpec) {
        let _ = self.subscriptions.insert(
            local_id,
            SubscriptionState {
                spec,
                remote_subscription_id: None,
                active: false,
            },
        );
    }

    pub fn mark_subscription_active(&mut self, local_id: &str, remote_id: String) {
        if let Some(sub) = self.subscriptions.get_mut(local_id) {
            sub.remote_subscription_id = Some(remote_id);
            sub.active = true;
        }
    }

    #[must_use]
    pub fn subscriptions_to_restore(&self) -> Vec<(String, SubscriptionSpec)> {
        self.subscriptions
            .iter()
            .filter(|(_, sub)| !sub.active)
            .map(|(id, sub)| (id.clone(), sub.spec.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FallbackPolicy {
    pub default_action: ActionType,
}

impl Default for FallbackPolicy {
    fn default() -> Self {
        Self {
            default_action: ActionType::Fold,
        }
    }
}

impl FallbackPolicy {
    pub fn choose_action(&self, legal_actions: &[String]) -> ActionType {
        let fallback = format!("{:?}", self.default_action).to_lowercase();
        if legal_actions.iter().any(|a| a == &fallback) {
            self.default_action
        } else if legal_actions.iter().any(|a| a == "check") {
            ActionType::Check
        } else {
            ActionType::Fold
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_policy_backoff_is_capped() {
        let policy = ReconnectPolicy {
            base_backoff_ms: 100,
            max_backoff_ms: 500,
            ..ReconnectPolicy::default()
        };
        assert_eq!(policy.backoff_ms(0), 100);
        assert_eq!(policy.backoff_ms(1), 200);
        assert_eq!(policy.backoff_ms(3), 500);
    }

    #[test]
    fn ws_session_restores_subscriptions_after_reconnect() {
        let mut ws = WsSessionState::default();
        let spec = SubscriptionSpec {
            topic: "seat_events".to_string(),
            room_id: RoomId::new(),
            hand_id: None,
            seat_id: Some(1),
        };
        ws.register_subscription("local-1".to_string(), spec.clone());
        ws.mark_connected();
        let to_restore = ws.subscriptions_to_restore();
        assert_eq!(to_restore.len(), 1);
        assert_eq!(to_restore[0].1, spec);

        ws.mark_subscription_active("local-1", "remote-1".to_string());
        assert!(ws.subscriptions_to_restore().is_empty());

        ws.mark_disconnected();
        assert_eq!(ws.reconnect_attempt, 1);
        assert_eq!(ws.subscriptions_to_restore().len(), 1);
    }
}
