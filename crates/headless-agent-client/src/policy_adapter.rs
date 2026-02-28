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
            "instruction": "Choose exactly one legal action. private_state_json.decrypted_hole_cards_pretty uses human-readable cards (e.g. As, Td); if unavailable, decode decrypted_hole_cards via private_state_json.card_index_encoding. Output ONLY JSON: {\"action_type\":\"fold|check|call|raise_to|all_in\",\"amount\":\"u128 string or null\",\"rationale\":\"short\"}",
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

    #[tokio::test]
    async fn codex_cli_policy_adapter_can_exchange_stdio_with_cli_process() {
        let bin = std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_string());
        let args = vec![
            "-c".to_string(),
            "import json,sys; _=sys.stdin.read(); print(json.dumps({'action_type':'call','amount':None,'rationale':'mock_cli_ok'}))".to_string(),
        ];
        let policy = CodexCliPolicyAdapter::new(bin, args, Duration::from_secs(5));
        let input = PolicyDecisionInput {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            seat_id: 1,
            legal_actions: vec!["fold".to_string(), "call".to_string()],
            public_state_json: serde_json::json!({"pot_total": 10}),
            private_state_json: None,
            decision_context_json: None,
        };
        eprintln!("=== codex cli connectivity test ===");
        eprintln!(
            "policy_input:\n{}",
            serde_json::to_string_pretty(&input).expect("serialize input")
        );

        let decision = policy
            .decide_action(&input)
            .await
            .expect("decision")
            .expect("some");
        eprintln!(
            "policy_decision:\n{}",
            serde_json::to_string_pretty(&decision).expect("serialize decision")
        );
        assert_eq!(decision.action_type, ActionType::Call);
        assert_eq!(decision.source, PolicySource::Llm);
        assert_eq!(decision.rationale.as_deref(), Some("mock_cli_ok"));
    }

    #[tokio::test]
    async fn codex_cli_raw_stdio_output_is_visible_in_test_logs() {
        let payload = serde_json::json!({
            "task": "poker_action_decision",
            "input": {
                "legal_actions": ["fold", "call"]
            }
        });
        let payload_text = payload.to_string();
        let bin = std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_string());
        let mut child = tokio::process::Command::new(bin)
            .args([
                "-c",
                "import json,sys; p=sys.stdin.read(); print('RAW_STDOUT_BEGIN'); print(p); print(json.dumps({'action_type':'call','amount':None,'rationale':'raw_stdio_ok'})); print('RAW_STDERR_BEGIN', file=sys.stderr)",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn mock cli");
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt as _;
            stdin
                .write_all(payload_text.as_bytes())
                .await
                .expect("write stdin");
        }
        let output = child.wait_with_output();

        let output = tokio::time::timeout(Duration::from_secs(5), output)
            .await
            .expect("wait timeout")
            .expect("wait output");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        eprintln!("=== raw cli stdio test ===");
        eprintln!("raw_stdout:\n{stdout}");
        eprintln!("raw_stderr:\n{stderr}");

        let decision_line = stdout
            .lines()
            .rev()
            .find(|line| line.contains("\"action_type\""))
            .expect("decision json line");
        let parsed: CodexDecisionRaw = serde_json::from_str(decision_line).expect("parse decision json");
        assert_eq!(parsed.action_type, "call");
        assert_eq!(parsed.rationale.as_deref(), Some("raw_stdio_ok"));
        assert!(stderr.contains("RAW_STDERR_BEGIN"));
        assert!(stdout.contains(&payload_text));
    }

    #[tokio::test]
    #[ignore = "requires real codex cli/auth/network; run manually"]
    async fn codex_cli_real_decision_via_wrapper_must_not_fallback() {
        let python_bin = std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_string());
        let wrapper_path = format!(
            "{}/../../scripts/codex_decide_wrapper.py",
            env!("CARGO_MANIFEST_DIR")
        );
        assert!(
            std::path::Path::new(&wrapper_path).exists(),
            "wrapper not found: {wrapper_path}"
        );

        eprintln!("=== real codex decision test ===");
        eprintln!("python_bin={python_bin}");
        eprintln!("wrapper_path={wrapper_path}");
        eprintln!(
            "env CODEX_WRAPPER_CMD={}",
            std::env::var("CODEX_WRAPPER_CMD").unwrap_or_else(|_| "<unset, default=codex>".to_string())
        );
        eprintln!(
            "env CODEX_WRAPPER_ARGS={}",
            std::env::var("CODEX_WRAPPER_ARGS").unwrap_or_else(|_| "<unset>".to_string())
        );

        let policy = CodexCliPolicyAdapter::new(
            python_bin,
            vec![wrapper_path],
            Duration::from_secs(180),
        );
        let input = PolicyDecisionInput {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            seat_id: 0,
            legal_actions: vec!["fold".to_string(), "check".to_string(), "call".to_string()],
            public_state_json: serde_json::json!({
                "pot_total": 30,
                "stack": 970,
                "to_call": 10
            }),
            private_state_json: Some(serde_json::json!({
                "decrypted_hole_cards": [{"cards":[1,3]}]
            })),
            decision_context_json: None,
        };

        eprintln!(
            "policy_input:\n{}",
            serde_json::to_string_pretty(&input).expect("serialize input")
        );

        let decision = policy
            .decide_action(&input)
            .await
            .expect("real codex decision backend")
            .expect("real codex decision output");

        eprintln!(
            "policy_decision:\n{}",
            serde_json::to_string_pretty(&decision).expect("serialize decision")
        );

        let rationale = decision.rationale.clone().unwrap_or_default();
        assert!(
            rationale != "wrapper_fallback_rule" && rationale != "wrapper_invalid_input",
            "wrapper fallback detected, real codex decision not observed: rationale={rationale}"
        );
        let action = format!("{:?}", decision.action_type).to_ascii_lowercase();
        assert!(
            input.legal_actions.iter().any(|a| a == &action),
            "decision action not legal: {action}"
        );
    }
}
