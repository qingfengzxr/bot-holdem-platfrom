use async_trait::async_trait;
use poker_domain::{ActionType, Chips, HandId, RoomId, SeatId};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

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
    pub decision_context_json: Option<serde_json::Value>,
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

#[derive(Debug, Clone)]
pub struct CodexCliPolicyAdapter {
    pub bin: String,
    pub args: Vec<String>,
    pub timeout: Duration,
}

impl CodexCliPolicyAdapter {
    #[must_use]
    pub fn new(bin: impl Into<String>, args: Vec<String>, timeout: Duration) -> Self {
        Self {
            bin: bin.into(),
            args,
            timeout,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionContextEntry {
    pub hand_id: HandId,
    pub seat_id: SeatId,
    pub legal_actions: Vec<String>,
    pub chosen_action: Option<String>,
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DecisionContextManager {
    max_entries: usize,
    entries: VecDeque<DecisionContextEntry>,
}

impl DecisionContextManager {
    #[must_use]
    pub fn with_max_entries(max_entries: usize) -> Self {
        Self {
            max_entries: max_entries.max(1),
            entries: VecDeque::new(),
        }
    }

    pub fn push(&mut self, entry: DecisionContextEntry) {
        self.entries.push_back(entry);
        while self.entries.len() > self.max_entries {
            let _ = self.entries.pop_front();
        }
    }

    #[must_use]
    pub fn as_json(&self) -> serde_json::Value {
        serde_json::json!(self.entries)
    }
}

#[derive(Debug, Deserialize)]
struct CodexDecisionRaw {
    action_type: String,
    amount: Option<String>,
    rationale: Option<String>,
}

fn parse_action_type(raw: &str) -> Option<ActionType> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "fold" => Some(ActionType::Fold),
        "check" => Some(ActionType::Check),
        "call" => Some(ActionType::Call),
        "raise_to" => Some(ActionType::RaiseTo),
        "all_in" => Some(ActionType::AllIn),
        _ => None,
    }
}

fn parse_amount(raw: Option<String>) -> Result<Option<Chips>, PolicyAdapterError> {
    match raw {
        Some(v) => {
            let n = v
                .parse::<u128>()
                .map_err(|_| PolicyAdapterError::InvalidDecision)?;
            Ok(Some(Chips(n)))
        }
        None => Ok(None),
    }
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then_some(&text[start..=end])
}

#[async_trait]
impl PolicyAdapter for CodexCliPolicyAdapter {
    async fn decide_action(
        &self,
        input: &PolicyDecisionInput,
    ) -> Result<Option<PolicyDecision>, PolicyAdapterError> {
        let prompt = serde_json::json!({
            "task": "poker_action_decision",
            "instruction": "Choose exactly one legal action. Output ONLY JSON: {\"action_type\":\"fold|check|call|raise_to|all_in\",\"amount\":\"u128 string or null\",\"rationale\":\"short\"}",
            "input": input,
        })
        .to_string();

        let mut child = Command::new(&self.bin);
        child
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = child
            .spawn()
            .map_err(|_| PolicyAdapterError::BackendUnavailable)?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|_| PolicyAdapterError::BackendUnavailable)?;
        }

        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| PolicyAdapterError::BackendUnavailable)?
            .map_err(|_| PolicyAdapterError::BackendUnavailable)?;

        if !output.status.success() {
            return Err(PolicyAdapterError::BackendUnavailable);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let raw: CodexDecisionRaw = serde_json::from_str(&stdout)
            .or_else(|_| {
                extract_json_object(&stdout)
                    .ok_or_else(|| serde_json::Error::io(std::io::Error::other("no json object")))
                    .and_then(serde_json::from_str)
            })
            .map_err(|_| PolicyAdapterError::InvalidDecision)?;

        let action_type = parse_action_type(&raw.action_type).ok_or(PolicyAdapterError::InvalidDecision)?;
        let action_name = format!("{:?}", action_type).to_ascii_lowercase();
        if !input.legal_actions.iter().any(|a| a == &action_name) {
            return Err(PolicyAdapterError::InvalidDecision);
        }

        Ok(Some(PolicyDecision {
            action_type,
            amount: parse_amount(raw.amount)?,
            rationale: raw.rationale,
            source: PolicySource::Llm,
        }))
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
            decision_context_json: None,
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
