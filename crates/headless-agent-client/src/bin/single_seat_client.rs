#![allow(unused_crate_dependencies)]

use std::collections::HashSet;
use std::env;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use headless_agent_client::runtime::{
    RuntimeError, SingleActionRunnerConfig, collect_turn_decision_input, execute_turn_decision,
    prepare_seat,
};
use headless_agent_client::wallet_adapter::JsonRpcEvmWalletAdapter;
use platform_core::ResponseEnvelope;
use headless_agent_client::policy_adapter::{
    CodexCliPolicyAdapter, DecisionContextEntry, DecisionContextManager, PolicyAdapter,
    RulePolicyAdapter,
};
use observability::init_tracing;
use poker_domain::{Chips, HandId, HandStatus, RoomId};
use rpc_gateway::{
    EventEnvelope, EventTopic, GameGetPrivatePayloadsRequest, GameGetStateRequest,
    PrivatePayloadEvent, RoomListRequest, RoomSummary, SubscribeRequest,
};
use seat_crypto::{HoleCardsDealtCipherPayload, decrypt_with_recipient_x25519};
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

fn card_index_to_notation(index: u64) -> Option<String> {
    if index >= 52 {
        return None;
    }
    let value = match index % 13 {
        0 => "2",
        1 => "3",
        2 => "4",
        3 => "5",
        4 => "6",
        5 => "7",
        6 => "8",
        7 => "9",
        8 => "T",
        9 => "J",
        10 => "Q",
        11 => "K",
        12 => "A",
        _ => return None,
    };
    let suit = match index / 13 {
        0 => "s",
        1 => "c",
        2 => "h",
        3 => "d",
        _ => return None,
    };
    Some(format!("{value}{suit}"))
}

fn pretty_hole_cards(private_state: &serde_json::Value) -> Option<Vec<Vec<String>>> {
    let entries = private_state.get("decrypted_hole_cards")?.as_array()?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let cards = entry.get("cards")?.as_array()?;
        let mut pair = Vec::with_capacity(cards.len());
        for c in cards {
            let idx = c.as_u64()?;
            let notation = card_index_to_notation(idx)?;
            pair.push(notation);
        }
        out.push(pair);
    }
    Some(out)
}

fn format_wei_as_eth(wei: u128) -> String {
    const WEI_PER_ETH: u128 = 1_000_000_000_000_000_000;
    let whole = wei / WEI_PER_ETH;
    let frac = wei % WEI_PER_ETH;
    if frac == 0 {
        return format!("{whole}.0 ETH");
    }
    let mut frac_str = format!("{frac:018}");
    while frac_str.ends_with('0') {
        let _ = frac_str.pop();
    }
    format!("{whole}.{frac_str} ETH")
}

fn payload_u64(payload: &serde_json::Value, key: &str) -> Option<u64> {
    payload.get(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
            .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
    })
}

fn parse_x25519_secret_hex(secret_hex: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(secret_hex).with_context(|| "invalid CARD_ENCRYPT_SECRET_HEX")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("CARD_ENCRYPT_SECRET_HEX must decode to 32 bytes"))?;
    Ok(arr)
}

fn envelope_ok<T>(env: ResponseEnvelope<T>, method: &str) -> Result<T> {
    if !env.ok {
        let msg = env
            .error
            .map(|e| format!("{:?}: {}", e.code, e.message))
            .unwrap_or_else(|| "unknown app error".to_string());
        bail!("{method} failed: {msg}");
    }
    env.data
        .ok_or_else(|| anyhow::anyhow!("{method} missing data payload"))
}

async fn sync_hole_cards_for_hand(
    cfg: &SingleActionRunnerConfig,
    hand_id: HandId,
    synced_hands: &mut HashSet<Uuid>,
) -> Result<()> {
    if synced_hands.contains(&hand_id.0) {
        return Ok(());
    }
    let Some(secret_hex) = cfg.card_encrypt_secret_hex.as_deref() else {
        return Ok(());
    };
    let secret = parse_x25519_secret_hex(secret_hex)?;
    let private_req = GameGetPrivatePayloadsRequest {
        room_id: cfg.room_id,
        seat_id: cfg.seat_id,
        hand_id: Some(hand_id),
    };
    let private_env: ResponseEnvelope<Vec<PrivatePayloadEvent>> = http_rpc_call(
        &cfg.rpc_endpoint,
        "game.get_private_payloads",
        serde_json::to_value(private_req).map_err(|e| anyhow::anyhow!(e.to_string()))?,
    )
    .await?;
    let events = envelope_ok(private_env, "game.get_private_payloads")?;
    let mut decrypted_hole_cards: Vec<serde_json::Value> = Vec::new();
    for event in events {
        if event.event_name != "hole_cards_dealt" {
            continue;
        }
        let Ok(payload) = serde_json::from_value::<HoleCardsDealtCipherPayload>(event.payload) else {
            continue;
        };
        let Ok(plaintext) = decrypt_with_recipient_x25519(&secret, &payload.aad, &payload.envelope)
        else {
            continue;
        };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&plaintext) else {
            continue;
        };
        decrypted_hole_cards.push(v);
    }
    if decrypted_hole_cards.is_empty() {
        return Ok(());
    }
    let private_state = serde_json::json!({
        "decrypted_hole_cards": decrypted_hole_cards,
    });
    let Some(cards_raw) = private_state.get("decrypted_hole_cards") else {
        return Ok(());
    };
    let pretty_cards = pretty_hole_cards(&private_state);
    info!(
        hand_id = %hand_id.0,
        seat_id = cfg.seat_id,
        hole_cards_raw = %cards_raw,
        hole_cards_pretty = ?pretty_cards,
        "hole cards synced on hand start"
    );
    synced_hands.insert(hand_id.0);
    Ok(())
}

async fn sync_hole_cards_for_current_hand(
    cfg: &SingleActionRunnerConfig,
    synced_hands: &mut HashSet<Uuid>,
) -> Result<()> {
    let state_req = GameGetStateRequest {
        room_id: cfg.room_id,
        hand_id: None,
        seat_id: Some(cfg.seat_id),
    };
    let state_env: ResponseEnvelope<Option<poker_domain::HandSnapshot>> = http_rpc_call(
        &cfg.rpc_endpoint,
        "game.get_state",
        serde_json::to_value(state_req).map_err(|e| anyhow::anyhow!(e.to_string()))?,
    )
    .await?;
    let state = envelope_ok(state_env, "game.get_state")?;
    let Some(state) = state else {
        return Ok(());
    };
    if !matches!(
        state.status,
        HandStatus::Running | HandStatus::Showdown | HandStatus::Settling
    ) {
        return Ok(());
    }
    sync_hole_cards_for_hand(cfg, state.hand_id, synced_hands).await
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
        info!(
            hand_id = %turn.hand_id.0,
            action_seq = turn.action_seq,
            seat_id = turn.policy_input.seat_id,
            "skip duplicate turn attempt (already attempted this action_seq)"
        );
        return Ok(());
    }
    if let Some(private_state) = turn.policy_input.private_state_json.as_ref()
        && let Some(cards) = private_state.get("decrypted_hole_cards")
    {
        let pretty_cards = pretty_hole_cards(private_state);
        info!(
            hand_id = %turn.hand_id.0,
            action_seq = turn.action_seq,
            hole_cards_raw = %cards,
            hole_cards_pretty = ?pretty_cards,
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
        decision_amount = ?decision.amount.map(|v| v.as_u128()),
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
                action_type = %format!("{:?}", decision.action_type).to_ascii_lowercase(),
                submitted_amount_wei = ?outcome.submitted_amount.map(|v| v.as_u128()),
                submitted_amount_eth = ?outcome
                    .submitted_amount
                    .map(|v| format_wei_as_eth(v.as_u128())),
                tx_hash = ?outcome.tx_hash,
                decision_source = ?decision.source,
                "action decision submitted"
            );
        }
        Err(err) => {
            warn!(error = %err, "execute decision failed");
            if matches!(err, RuntimeError::Http(_) | RuntimeError::Wallet(_)) {
                *last_attempted_turn = None;
                warn!(
                    hand_id = %turn.hand_id.0,
                    action_seq = turn.action_seq,
                    "transient execution error, will retry on next event/poll"
                );
            }
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
    let policy_timeout_ms = env_parse_u64("POLICY_TIMEOUT_MS", 180_000)?;
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
    let mut synced_hands: HashSet<Uuid> = HashSet::new();
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
        if let Err(err) = sync_hole_cards_for_current_hand(&cfg, &mut synced_hands).await {
            warn!(error = %err, "sync hole cards for current hand failed");
        }

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
                            info!(
                                event_name = %event.event_name,
                                topic = ?event.topic,
                                room_id = %event.room_id.0,
                                hand_id = ?event.hand_id.map(|v| v.0),
                                envelope_seat_id = ?event.seat_id,
                                payload_seat_id = ?payload_u64(&event.payload, "seat_id"),
                                acting_seat_id = ?payload_u64(&event.payload, "acting_seat_id"),
                                action_seq = ?payload_u64(&event.payload, "action_seq"),
                                legal_actions = ?event.payload.get("legal_actions"),
                                payload = %event.payload,
                                "event notification received"
                            );
                            if event.event_name == "turn_started" {
                                info!(
                                    room_id = %event.room_id.0,
                                    hand_id = ?event.hand_id.map(|v| v.0),
                                    seat_id = ?event.seat_id,
                                    "turn started event received"
                                );
                            }
                            if matches!(event.event_name.as_str(), "hand_started" | "turn_started")
                                && let Some(hand_id) = event.hand_id
                                && let Err(err) =
                                    sync_hole_cards_for_hand(&cfg, hand_id, &mut synced_hands).await
                            {
                                warn!(error = %err, hand_id = %hand_id.0, "sync hole cards for hand failed");
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
