use async_trait::async_trait;
use poker_domain::{ActionType, Chips, HandId, RoomId, SeatId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PolicyAdapterError {
    #[error("policy backend unavailable")]
    BackendUnavailable,
    #[error("invalid policy decision")]
    InvalidDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDecisionInput {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub seat_id: SeatId,
    pub legal_actions: Vec<String>,
    pub public_state_json: serde_json::Value,
    pub private_state_json: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub action_type: ActionType,
    pub amount: Option<Chips>,
    pub rationale: Option<String>,
    pub source: PolicySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicySource {
    Rule,
    Llm,
    Script,
}

#[async_trait]
pub trait PolicyAdapter: Send + Sync {
    async fn decide_action(
        &self,
        input: &PolicyDecisionInput,
    ) -> Result<Option<PolicyDecision>, PolicyAdapterError>;
}

#[derive(Debug, Default, Clone)]
pub struct RulePolicyAdapter;

#[async_trait]
impl PolicyAdapter for RulePolicyAdapter {
    async fn decide_action(
        &self,
        input: &PolicyDecisionInput,
    ) -> Result<Option<PolicyDecision>, PolicyAdapterError> {
        let legal = &input.legal_actions;
        let choose = if legal.iter().any(|a| a == "check") {
            Some(ActionType::Check)
        } else if legal.iter().any(|a| a == "call") {
            Some(ActionType::Call)
        } else if legal.iter().any(|a| a == "fold") {
            Some(ActionType::Fold)
        } else {
            None
        };

        Ok(choose.map(|action_type| PolicyDecision {
            action_type,
            amount: None,
            rationale: Some("rule_policy_simple_priority".to_string()),
            source: PolicySource::Rule,
        }))
    }
}

#[derive(Debug, Default, Clone)]
pub struct LlmCodexPolicyAdapter;

#[async_trait]
impl PolicyAdapter for LlmCodexPolicyAdapter {
    async fn decide_action(
        &self,
        _input: &PolicyDecisionInput,
    ) -> Result<Option<PolicyDecision>, PolicyAdapterError> {
        // Placeholder: future implementation can call a local tool runner / LLM backend.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rule_policy_prefers_check_then_call_then_fold() {
        let policy = RulePolicyAdapter;
        let input = PolicyDecisionInput {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            seat_id: 1,
            legal_actions: vec!["fold".to_string(), "check".to_string()],
            public_state_json: serde_json::json!({}),
            private_state_json: None,
        };

        let decision = policy
            .decide_action(&input)
            .await
            .expect("decision")
            .expect("some");
        assert_eq!(decision.action_type, ActionType::Check);
        assert_eq!(decision.source, PolicySource::Rule);
    }
}
