#![allow(unused_crate_dependencies)]

use std::env;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
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
use rpc_gateway::{EventEnvelope, EventTopic, RoomListRequest, RoomSummary, SubscribeRequest};
use tokio::net::TcpStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{info, warn};
use uuid::Uuid;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

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

fn derive_ws_endpoint(http_endpoint: &str) -> String {
    if let Some(rest) = http_endpoint.strip_prefix("http://") {
        return format!("ws://{rest}");
    }
    if let Some(rest) = http_endpoint.strip_prefix("https://") {
        return format!("wss://{rest}");
    }
    http_endpoint.to_string()
}

async fn maybe_execute_turn<W>(
    cfg: &SingleActionRunnerConfig,
    wallet: &W,
    decision_ctx: &mut DecisionContextManager,
    policy_mode: PolicyMode,
    codex_policy: &CodexCliPolicyAdapter,
    rule_policy: &RulePolicyAdapter,
    last_attempted_turn: &mut Option<(Uuid, u32)>,
) -> Result<()>
where
    W: headless_agent_client::wallet_adapter::WalletAdapter + ?Sized,
{
    let context_json = Some(decision_ctx.as_json());
    let Some(turn) = collect_turn_decision_input(cfg.clone(), context_json).await? else {
        return Ok(());
    };

    let turn_key = (turn.hand_id.0, turn.action_seq);
    if *last_attempted_turn == Some(turn_key) {
        return Ok(());
    }
    if let Some(private_state) = turn.policy_input.private_state_json.as_ref()
        && let Some(cards) = private_state.get("decrypted_hole_cards")
    {
        info!(
            hand_id = %turn.hand_id.0,
            action_seq = turn.action_seq,
            hole_cards = %cards,
            "received hole cards"
        );
    }

    let decision = match policy_mode {
        PolicyMode::Rule => rule_policy.decide_action(&turn.policy_input).await.ok().flatten(),
        PolicyMode::CodexCli => match codex_policy.decide_action(&turn.policy_input).await {
            Ok(Some(v)) => Some(v),
            _ => rule_policy.decide_action(&turn.policy_input).await.ok().flatten(),
        },
    };
    let Some(decision) = decision else {
        warn!(
            hand_id = %turn.hand_id.0,
            action_seq = turn.action_seq,
            seat_id = turn.policy_input.seat_id,
            legal_actions = ?turn.policy_input.legal_actions,
            policy_mode = ?policy_mode,
            "no policy decision available"
        );
        return Ok(());
    };
    info!(
        hand_id = %turn.hand_id.0,
        action_seq = turn.action_seq,
        action_type = %format!("{:?}", decision.action_type).to_ascii_lowercase(),
        amount = ?decision.amount.map(|v| v.as_u128()),
        rationale = ?decision.rationale,
        source = ?decision.source,
        "policy decision resolved"
    );

    let Ok(latest_turn) = collect_turn_decision_input(cfg.clone(), None).await else {
        return Ok(());
    };
    let Some(latest_turn) = latest_turn else {
        return Ok(());
    };
    if latest_turn.hand_id != turn.hand_id || latest_turn.action_seq != turn.action_seq {
        return Ok(());
    }

    *last_attempted_turn = Some(turn_key);
    match execute_turn_decision(cfg.clone(), wallet, &turn, &decision).await {
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

    Ok(())
}

async fn connect_ws(ws_endpoint: &str) -> Result<WsStream> {
    let (stream, _) = connect_async(ws_endpoint)
        .await
        .with_context(|| format!("failed to connect websocket endpoint: {ws_endpoint}"))?;
    Ok(stream)
}

async fn subscribe_turn_events(
    ws_stream: &mut WsStream,
    cfg: &SingleActionRunnerConfig,
) -> Result<String> {
    let subscribe_req = SubscribeRequest {
        topic: EventTopic::HandEvents,
        room_id: cfg.room_id,
        hand_id: None,
        seat_id: None,
    };
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1_u64,
        "method": "subscribe.events.native",
        "params": subscribe_req,
    });
    ws_stream
        .send(Message::Text(req.to_string()))
        .await
        .context("failed to send websocket subscribe request")?;

    while let Some(msg) = ws_stream.next().await {
        let msg = msg.context("websocket read failed while waiting subscribe response")?;
        if !msg.is_text() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(msg.to_text().unwrap_or_default())
            .context("invalid websocket json frame")?;
        if value.get("id") != Some(&serde_json::json!(1_u64)) {
            continue;
        }
        if let Some(err) = value.get("error") {
            bail!("websocket subscribe returned error: {err}");
        }
        if let Some(result) = value.get("result") {
            if let Some(subscription_id) = result.as_str() {
                return Ok(subscription_id.to_string());
            }
            if let Some(subscription_id) = result.as_u64() {
                return Ok(subscription_id.to_string());
            }
            if let Some(subscription_id) = result.as_i64() {
                return Ok(subscription_id.to_string());
            }
        }
        if let Some(subscription_id) = value
            .get("result")
            .and_then(|v| v.get("data"))
            .and_then(serde_json::Value::as_str)
        {
            return Ok(subscription_id.to_string());
        }
        if let Some(subscription_id) = value
            .get("result")
            .and_then(|v| v.get("subscription_id"))
            .and_then(serde_json::Value::as_str)
        {
            return Ok(subscription_id.to_string());
        }
        let compact = serde_json::to_string(&value).unwrap_or_else(|_| "<invalid-json>".to_string());
        bail!("websocket subscribe response missing subscription id: {compact}");
    }

    bail!("websocket closed before subscribe acknowledged")
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("single-seat-client");

    let wallet_rpc = env_optional("WALLET_RPC_URL").unwrap_or_else(|| "http://127.0.0.1:8545".to_string());
    let wallet = JsonRpcEvmWalletAdapter::new(wallet_rpc.clone());
    let chain_id = env_parse_u64("CHAIN_ID", 31_337)?;
    let policy_timeout_ms = env_parse_u64("POLICY_TIMEOUT_MS", 1_500)?;
    let safety_poll_ms = env_parse_u64("CLIENT_SAFETY_POLL_MS", 2_000)?;
    let context_max_entries = env_parse_u64("POLICY_CONTEXT_MAX_ENTRIES", 16)? as usize;
    let policy_mode = parse_policy_mode();
    let codex_cli_bin = env_optional("CODEX_CLI_BIN").unwrap_or_else(|| "codex".to_string());
    let codex_cli_args = parse_codex_cli_args();
    let rpc_endpoint = env_required("RPC_ENDPOINT")?;
    let room_id = resolve_room_id(&rpc_endpoint, env_parse_room_id_optional("ROOM_ID")?).await?;
    let cfg = build_config(chain_id, room_id)?;
    let ws_endpoint =
        env_optional("RPC_WS_ENDPOINT").unwrap_or_else(|| derive_ws_endpoint(&cfg.rpc_endpoint));

    info!(
        rpc_endpoint = %cfg.rpc_endpoint,
        ws_endpoint = %ws_endpoint,
        wallet_rpc = %wallet_rpc,
        room_id = %cfg.room_id.0,
        seat_id = cfg.seat_id,
        chain_id = cfg.chain_id,
        safety_poll_ms,
        policy_mode = ?policy_mode,
        "single seat client starting"
    );

    prepare_seat(cfg.clone(), &wallet).await.context("prepare_seat failed")?;
    info!("seat prepared, entering action loop");

    let mut decision_ctx = DecisionContextManager::with_max_entries(context_max_entries);
    let mut last_attempted_turn: Option<(Uuid, u32)> = None;
    let codex_policy = CodexCliPolicyAdapter::new(
        codex_cli_bin,
        codex_cli_args,
        Duration::from_millis(policy_timeout_ms),
    );
    let rule_policy = RulePolicyAdapter;

    let mut reconnect_backoff_ms = 500_u64;
    loop {
        let mut ws_stream = match connect_ws(&ws_endpoint).await {
            Ok(stream) => {
                reconnect_backoff_ms = 500;
                stream
            }
            Err(err) => {
                warn!(error = %err, backoff_ms = reconnect_backoff_ms, "websocket connect failed");
                tokio::time::sleep(Duration::from_millis(reconnect_backoff_ms)).await;
                reconnect_backoff_ms = (reconnect_backoff_ms.saturating_mul(2)).min(10_000);
                continue;
            }
        };

        let subscription_id = match subscribe_turn_events(&mut ws_stream, &cfg).await {
            Ok(v) => v,
            Err(err) => {
                warn!(error = %err, "subscribe events failed");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        info!(subscription_id = %subscription_id, "subscribed to turn events");

        let _ = maybe_execute_turn(
            &cfg,
            &wallet,
            &mut decision_ctx,
            policy_mode,
            &codex_policy,
            &rule_policy,
            &mut last_attempted_turn,
        )
        .await;

        let mut safety_ticker = tokio::time::interval(Duration::from_millis(safety_poll_ms));
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("single seat client received ctrl-c, exiting");
                    return Ok(());
                }
                _ = safety_ticker.tick() => {
                    if let Err(err) = maybe_execute_turn(
                        &cfg,
                        &wallet,
                        &mut decision_ctx,
                        policy_mode,
                        &codex_policy,
                        &rule_policy,
                        &mut last_attempted_turn,
                    ).await {
                        warn!(error = %err, "safety poll turn check failed");
                    }
                }
                maybe_event = ws_stream.next() => {
                    match maybe_event {
                        Some(Ok(Message::Text(text))) => {
                            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                                continue;
                            };
                            let Some(method) = value.get("method").and_then(serde_json::Value::as_str) else {
                                continue;
                            };
                            if method != "events" {
                                continue;
                            }
                            let Some(event_json) = value.get("params").and_then(|p| p.get("result")) else {
                                continue;
                            };
                            let Ok(event) = serde_json::from_value::<EventEnvelope>(event_json.clone()) else {
                                continue;
                            };
                            if event.event_name == "turn_started" {
                                info!(
                                    room_id = %event.room_id.0,
                                    hand_id = ?event.hand_id.map(|v| v.0),
                                    seat_id = ?event.seat_id,
                                    "turn started event received"
                                );
                            }
                            if event.event_name == "hand_closed" {
                                info!(
                                    hand_id = ?event.hand_id.map(|v| v.0),
                                    "hand closed event received"
                                );
                            }
                            if event.event_name != "turn_started" {
                                continue;
                            }
                            if let Err(err) = maybe_execute_turn(
                                &cfg,
                                &wallet,
                                &mut decision_ctx,
                                policy_mode,
                                &codex_policy,
                                &rule_policy,
                                &mut last_attempted_turn,
                            ).await {
                                warn!(error = %err, "collect turn input failed");
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            if ws_stream.send(Message::Pong(payload)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Close(_))) => {
                            warn!("event websocket closed by server, reconnecting");
                            break;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(err)) => {
                            warn!(error = %err, "event subscription stream failed, reconnecting");
                            break;
                        }
                        None => {
                            warn!("event subscription closed, reconnecting");
                            break;
                        }
                    }
                }
            }
        }
    }
}
