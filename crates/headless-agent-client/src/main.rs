mod client;

use agent_sdk::{AgentSkill, AgentSkillConfig, LocalAgentSkill};
use anyhow::Result;
use client::{FallbackPolicy, StateCache, TurnContext};
use observability::init_tracing;
use poker_domain::{ActionType, HandId, RoomId};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("headless-agent-client");

    let mut skill = LocalAgentSkill::default();
    let mut state_cache = StateCache::default();
    let fallback_policy = FallbackPolicy::default();
    skill
        .connect(AgentSkillConfig {
            endpoint_http: "http://127.0.0.1:9000".to_string(),
            endpoint_ws: Some("ws://127.0.0.1:9000".to_string()),
            session_id: None,
        })
        .await?;

    state_cache.update_turn(TurnContext {
        room_id: RoomId::new(),
        hand_id: HandId::new(),
        seat_id: 0,
        legal_actions: vec!["check".to_string(), "fold".to_string()],
        action_deadline_unix_ms: None,
    });
    let fallback_action = fallback_policy.choose_action(
        &state_cache
            .current_turn
            .as_ref()
            .map(|t| t.legal_actions.clone())
            .unwrap_or_default(),
    );

    info!(
        fallback_action = %format!("{fallback_action:?}"),
        default_is_fold = matches!(fallback_policy.default_action, ActionType::Fold),
        "headless agent client bootstrap complete"
    );
    Ok(())
}
