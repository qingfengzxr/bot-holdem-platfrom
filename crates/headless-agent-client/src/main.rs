use agent_skill::{AgentSkill, AgentSkillConfig, LocalAgentSkill};
use anyhow::Result;
use observability::init_tracing;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("headless-agent-client");

    let mut skill = LocalAgentSkill::default();
    skill
        .connect(AgentSkillConfig {
            endpoint_http: "http://127.0.0.1:9000".to_string(),
            endpoint_ws: Some("ws://127.0.0.1:9000".to_string()),
            session_id: None,
        })
        .await?;

    info!("headless agent client bootstrap complete");
    Ok(())
}
