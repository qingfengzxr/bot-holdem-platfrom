mod audit;
mod client;
mod policy_adapter;
mod wallet_adapter;

use agent_sdk::{AgentSkill, AgentSkillConfig, LocalAgentSkill};
use anyhow::Result;
use audit::{ClientAuditRecord, LocalAuditLogger, now_unix_ms};
use client::{FallbackPolicy, StateCache, TurnContext};
use observability::init_tracing;
use poker_domain::{ActionType, Chips, HandId, RoomId};
use policy_adapter::{
    LlmCodexPolicyAdapter, PolicyAdapter, PolicyDecisionInput, RulePolicyAdapter,
};
use tracing::info;
use wallet_adapter::{EvmTransferRequest, MockEvmWalletAdapter, WalletAdapter};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("headless-agent-client");

    let mut skill = LocalAgentSkill::default();
    let mut state_cache = StateCache::default();
    let fallback_policy = FallbackPolicy::default();
    let wallet = MockEvmWalletAdapter;
    let llm_policy = LlmCodexPolicyAdapter;
    let rule_policy = RulePolicyAdapter;
    let audit = LocalAuditLogger::open("headless-agent-client.audit.ndjson")?;
    skill
        .connect(AgentSkillConfig {
            endpoint_http: "http://127.0.0.1:9000".to_string(),
            endpoint_ws: Some("ws://127.0.0.1:9000".to_string()),
            session_id: None,
        })
        .await?;
    audit.append(&ClientAuditRecord {
        ts_unix_ms: now_unix_ms(),
        event_kind: "client_connected".to_string(),
        room_id: None,
        hand_id: None,
        seat_id: None,
        payload: serde_json::json!({
            "endpoint_http": "http://127.0.0.1:9000",
            "has_ws": true
        }),
    })?;

    let turn = TurnContext {
        room_id: RoomId::new(),
        hand_id: HandId::new(),
        seat_id: 0,
        legal_actions: vec!["check".to_string(), "fold".to_string()],
        action_deadline_unix_ms: None,
    };
    state_cache.update_turn(turn.clone());
    audit.append(&ClientAuditRecord {
        ts_unix_ms: now_unix_ms(),
        event_kind: "turn_context_updated".to_string(),
        room_id: Some(turn.room_id),
        hand_id: Some(turn.hand_id),
        seat_id: Some(turn.seat_id),
        payload: serde_json::to_value(&turn)?,
    })?;

    let policy_input = PolicyDecisionInput {
        room_id: turn.room_id,
        hand_id: turn.hand_id,
        seat_id: turn.seat_id,
        legal_actions: turn.legal_actions.clone(),
        public_state_json: serde_json::json!({}),
        private_state_json: None,
    };
    let policy_decision = llm_policy
        .decide_action(&policy_input)
        .await?
        .or(rule_policy.decide_action(&policy_input).await?);
    let fallback_action = policy_decision
        .as_ref()
        .map(|d| d.action_type)
        .unwrap_or_else(|| fallback_policy.choose_action(&policy_input.legal_actions));
    audit.append(&ClientAuditRecord {
        ts_unix_ms: now_unix_ms(),
        event_kind: "fallback_action_selected".to_string(),
        room_id: state_cache.current_turn.as_ref().map(|t| t.room_id),
        hand_id: state_cache.current_turn.as_ref().map(|t| t.hand_id),
        seat_id: state_cache.current_turn.as_ref().map(|t| t.seat_id),
        payload: serde_json::json!({
            "action": format!("{fallback_action:?}").to_lowercase(),
            "policy_decision_source": policy_decision.as_ref().map(|d| format!("{:?}", d.source).to_lowercase())
        }),
    })?;

    let deposit_receipt = wallet
        .transfer_to_room(EvmTransferRequest {
            room_id: turn.room_id,
            from: "0xseat".to_string(),
            to: "0xroom".to_string(),
            value: Chips(10),
            chain_id: 71,
        })
        .await?;
    audit.append(&ClientAuditRecord {
        ts_unix_ms: now_unix_ms(),
        event_kind: "wallet_tx_submitted".to_string(),
        room_id: Some(turn.room_id),
        hand_id: Some(turn.hand_id),
        seat_id: Some(turn.seat_id),
        payload: serde_json::to_value(&deposit_receipt)?,
    })?;

    info!(
        fallback_action = %format!("{fallback_action:?}"),
        deposit_tx_hash = %deposit_receipt.tx_hash,
        default_is_fold = matches!(fallback_policy.default_action, ActionType::Fold),
        "headless agent client bootstrap complete"
    );
    Ok(())
}
