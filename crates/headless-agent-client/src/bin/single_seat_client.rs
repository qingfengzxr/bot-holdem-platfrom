#![allow(unused_crate_dependencies)]

use std::env;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use headless_agent_client::runtime::{
    SingleActionRunnerConfig, collect_turn_decision_input, execute_turn_decision, prepare_seat,
};
use headless_agent_client::wallet_adapter::JsonRpcEvmWalletAdapter;
use platform_core::ResponseEnvelope;
use headless_agent_client::policy_adapter::{
    CodexCliPolicyAdapter, DecisionContextEntry, DecisionContextManager, PolicyAdapter,
    RulePolicyAdapter,
};
use observability::init_tracing;
use poker_domain::{Chips, RoomId};
use rpc_gateway::{RoomListRequest, RoomSummary};
use tracing::{info, warn};
use uuid::Uuid;

fn env_required(name: &str) -> Result<String> {
    let v = env::var(name).with_context(|| format!("missing required env: {name}"))?;
    if v.trim().is_empty() {
        bail!("required env {name} is empty");
    }
    Ok(v)
}

fn env_optional(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn env_parse_u64(name: &str, default: u64) -> Result<u64> {
    match env_optional(name) {
        Some(v) => v
            .parse::<u64>()
            .with_context(|| format!("invalid {name}: {v}")),
        None => Ok(default),
    }
}

fn env_parse_room_id_optional(name: &str) -> Result<Option<RoomId>> {
    let Some(raw) = env_optional(name) else {
        return Ok(None);
    };
    let id = Uuid::parse_str(&raw).with_context(|| format!("invalid {name}: {raw}"))?;
    Ok(Some(RoomId(id)))
}

async fn http_rpc_call<T: serde::de::DeserializeOwned>(
    endpoint: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<T> {
    let payload = reqwest::Client::new()
        .post(endpoint)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }))
        .send()
        .await
        .with_context(|| format!("json-rpc {method} request failed"))?
        .json::<serde_json::Value>()
        .await
        .with_context(|| format!("json-rpc {method} decode failed"))?;

    if let Some(err) = payload.get("error") {
        bail!("json-rpc {method} returned error: {err}");
    }
    let result = payload
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("json-rpc {method} missing result"))?;
    serde_json::from_value(result)
        .with_context(|| format!("json-rpc {method} result decode failed"))
}

async fn resolve_room_id(rpc_endpoint: &str, room_id_from_env: Option<RoomId>) -> Result<RoomId> {
    if let Some(room_id) = room_id_from_env {
        return Ok(room_id);
    }

    let list_env: ResponseEnvelope<Vec<RoomSummary>> = http_rpc_call(
        rpc_endpoint,
        "room.list",
        serde_json::json!(RoomListRequest {
            include_inactive: Some(true),
        }),
    )
    .await?;

    if !list_env.ok {
        let msg = list_env
            .error
            .map(|e| format!("{:?}: {}", e.code, e.message))
            .unwrap_or_else(|| "unknown app error".to_string());
        bail!("room.list failed: {msg}");
    }

    let rooms = list_env.data.unwrap_or_default();
    if rooms.is_empty() {
        bail!("room.list returned no rooms; provide ROOM_ID or create a room first");
    }
    let selected = rooms
        .iter()
        .find(|r| r.status.eq_ignore_ascii_case("active"))
        .or_else(|| rooms.first())
        .cloned()
        .context("room.list returned no selectable room")?;
    info!(
        room_count = rooms.len(),
        selected_room_id = %selected.room_id.0,
        selected_room_status = %selected.status,
        "auto-selected room from room.list"
    );
    Ok(selected.room_id)
}

fn build_config(chain_id: u64, room_id: RoomId) -> Result<SingleActionRunnerConfig> {
    Ok(SingleActionRunnerConfig {
        rpc_endpoint: env_required("RPC_ENDPOINT")?,
        room_id,
        seat_id: env_parse_u64("SEAT_ID", 0)? as u8,
        seat_address: env_required("SEAT_ADDRESS")?,
        room_chain_address: env_required("ROOM_CHAIN_ADDRESS")?,
        chain_id,
        tx_value: Chips(env_parse_u64("TX_VALUE", 1)? as u128),
        card_encrypt_pubkey_hex: env_required("CARD_ENCRYPT_PUBKEY_HEX")?,
        card_encrypt_secret_hex: env_optional("CARD_ENCRYPT_SECRET_HEX"),
    })
}

#[derive(Debug, Clone, Copy)]
enum PolicyMode {
    Rule,
    CodexCli,
}

fn parse_policy_mode() -> PolicyMode {
    match env_optional("POLICY_MODE")
        .unwrap_or_else(|| "rule".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "codex_cli" => PolicyMode::CodexCli,
        _ => PolicyMode::Rule,
    }
}

fn parse_codex_cli_args() -> Vec<String> {
    env_optional("CODEX_CLI_ARGS")
        .map(|v| {
            v.split_whitespace()
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("single-seat-client");

    let wallet_rpc = env_optional("WALLET_RPC_URL").unwrap_or_else(|| "http://127.0.0.1:8545".to_string());
    let wallet = JsonRpcEvmWalletAdapter::new(wallet_rpc.clone());
    let chain_id = env_parse_u64("CHAIN_ID", 31_337)?;
    let tick_ms = env_parse_u64("TICK_MS", 800)?;
    let policy_timeout_ms = env_parse_u64("POLICY_TIMEOUT_MS", 1_500)?;
    let context_max_entries = env_parse_u64("POLICY_CONTEXT_MAX_ENTRIES", 16)? as usize;
    let policy_mode = parse_policy_mode();
    let codex_cli_bin = env_optional("CODEX_CLI_BIN").unwrap_or_else(|| "codex".to_string());
    let codex_cli_args = parse_codex_cli_args();
    let rpc_endpoint = env_required("RPC_ENDPOINT")?;
    let room_id = resolve_room_id(&rpc_endpoint, env_parse_room_id_optional("ROOM_ID")?).await?;
    let cfg = build_config(chain_id, room_id)?;

    info!(
        rpc_endpoint = %cfg.rpc_endpoint,
        wallet_rpc = %wallet_rpc,
        room_id = %cfg.room_id.0,
        seat_id = cfg.seat_id,
        chain_id = cfg.chain_id,
        tick_ms,
        policy_mode = ?policy_mode,
        "single seat client starting"
    );

    prepare_seat(cfg.clone(), &wallet).await.context("prepare_seat failed")?;
    info!("seat prepared, entering action loop");

    let mut decision_ctx = DecisionContextManager::with_max_entries(context_max_entries);
    let codex_policy = CodexCliPolicyAdapter::new(
        codex_cli_bin,
        codex_cli_args,
        Duration::from_millis(policy_timeout_ms),
    );
    let rule_policy = RulePolicyAdapter;

    let mut ticker = tokio::time::interval(Duration::from_millis(tick_ms));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("single seat client received ctrl-c, exiting");
                break;
            }
            _ = ticker.tick() => {
                let context_json = Some(decision_ctx.as_json());
                match collect_turn_decision_input(cfg.clone(), context_json).await {
                    Ok(Some(turn)) => {
                        let decision = match policy_mode {
                            PolicyMode::Rule => rule_policy
                                .decide_action(&turn.policy_input)
                                .await
                                .ok()
                                .flatten(),
                            PolicyMode::CodexCli => match codex_policy
                                .decide_action(&turn.policy_input)
                                .await
                            {
                                Ok(Some(v)) => Some(v),
                                _ => rule_policy
                                    .decide_action(&turn.policy_input)
                                    .await
                                    .ok()
                                    .flatten(),
                            },
                        };
                        let Some(decision) = decision else {
                            warn!("no policy decision available");
                            continue;
                        };
                        match execute_turn_decision(cfg.clone(), &wallet, &turn, &decision).await {
                            Ok(outcome) => {
                                decision_ctx.push(DecisionContextEntry {
                                    hand_id: turn.hand_id,
                                    seat_id: turn.policy_input.seat_id,
                                    legal_actions: turn.policy_input.legal_actions.clone(),
                                    chosen_action: Some(format!("{:?}", decision.action_type).to_ascii_lowercase()),
                                    rationale: decision.rationale.clone(),
                                });
                                info!(
                                    hand_id = %outcome.hand_id.0,
                                    action_seq = outcome.action_seq,
                                    tx_hash = ?outcome.tx_hash,
                                    decision_source = ?decision.source,
                                    "action submitted"
                                );
                            }
                            Err(err) => {
                                warn!(error = %err, "execute decision failed");
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        warn!(error = %err, "collect turn input failed");
                    }
                }
            }
        }
    }

    Ok(())
}
