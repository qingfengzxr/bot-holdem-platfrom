use agent_auth::{SeatKeyBindingClaim, SigningMessageInput, build_seat_key_binding_message, build_signing_message};
use agent_sdk::{AgentSkill, AgentSkillConfig, LocalAgentSkill};
use platform_core::ResponseEnvelope;
use poker_domain::{ActionType, Chips, HandSnapshot, LegalAction, RoomId, SeatId};
use rpc_gateway::{GameActRequest, RoomBindAddressRequest, RoomBindSessionKeysRequest, RoomReadyRequest};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::policy_adapter::{PolicyAdapter, PolicyDecisionInput, RulePolicyAdapter};
use crate::wallet_adapter::{EvmTransferRequest, WalletAdapter, WalletAdapterError};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("agent skill error: {0}")]
    AgentSkill(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("rpc response missing result")]
    RpcMissingResult,
    #[error("rpc application error: {0}")]
    RpcApp(String),
    #[error("wallet error: {0}")]
    Wallet(String),
    #[error("not our turn")]
    NotOurTurn,
    #[error("state unavailable")]
    StateUnavailable,
    #[error("policy error: {0}")]
    Policy(String),
}

#[derive(Debug, Clone)]
pub struct SingleActionRunnerConfig {
    pub rpc_endpoint: String,
    pub room_id: RoomId,
    pub seat_id: SeatId,
    pub seat_address: String,
    pub room_chain_address: String,
    pub chain_id: u64,
    pub tx_value: Chips,
    pub card_encrypt_pubkey_hex: String,
}

#[derive(Debug, Clone)]
pub struct SingleActionRunOutcome {
    pub hand_id: poker_domain::HandId,
    pub action_seq: u32,
    pub tx_hash: String,
}

#[derive(Debug, Clone)]
pub struct PreparedSeatOutcome;

pub struct SeatTurnRunnerInput<'a> {
    pub cfg: &'a SingleActionRunnerConfig,
}

async fn http_rpc_call<T: DeserializeOwned>(
    endpoint: &str,
    method: &str,
    params: Value,
) -> Result<T, RuntimeError> {
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
        .map_err(|e| RuntimeError::Http(e.to_string()))?
        .json::<Value>()
        .await
        .map_err(|e| RuntimeError::Http(e.to_string()))?;
    let result = payload
        .get("result")
        .cloned()
        .ok_or(RuntimeError::RpcMissingResult)?;
    serde_json::from_value(result).map_err(|e| RuntimeError::Http(e.to_string()))
}

fn envelope_ok<T>(env: ResponseEnvelope<T>) -> Result<T, RuntimeError> {
    if !env.ok {
        let msg = env
            .error
            .map(|e| format!("{:?}: {}", e.code, e.message))
            .unwrap_or_else(|| "unknown rpc app error".to_string());
        return Err(RuntimeError::RpcApp(msg));
    }
    env.data.ok_or(RuntimeError::RpcMissingResult)
}

async fn connect_skill(cfg: &SingleActionRunnerConfig) -> Result<LocalAgentSkill, RuntimeError> {
    let mut skill = LocalAgentSkill::default();
    skill
        .connect(AgentSkillConfig {
            endpoint_http: cfg.rpc_endpoint.clone(),
            endpoint_ws: None,
            session_id: None,
        })
        .await
        .map_err(|e| RuntimeError::AgentSkill(e.to_string()))?;
    skill
        .join_room(cfg.room_id, cfg.seat_id)
        .await
        .map_err(|e| RuntimeError::AgentSkill(e.to_string()))?;
    Ok(skill)
}

async fn sign_request_meta_with_wallet<W>(
    cfg: &SingleActionRunnerConfig,
    wallet: &W,
    method: &str,
    hand_id: Option<String>,
    action_seq: Option<u32>,
    params: &Value,
    meta: &mut agent_auth::SignedRequestMeta,
) -> Result<(), RuntimeError>
where
    W: WalletAdapter + ?Sized,
{
    meta.signature_pubkey_id = cfg.seat_address.clone();
    let message = build_signing_message(&SigningMessageInput {
        method,
        session_id: poker_domain::SessionId(uuid::Uuid::nil()),
        room_id: Some(cfg.room_id),
        hand_id,
        action_seq,
        params,
        meta,
    })
    .map_err(|e| RuntimeError::AgentSkill(e.to_string()))?;
    meta.signature = wallet
        .sign_personal_message(&cfg.seat_address, message.as_bytes())
        .await
        .map_err(|e| RuntimeError::Wallet(e.to_string()))?;
    Ok(())
}

async fn build_signed_bind_address_request<W>(
    skill: &mut LocalAgentSkill,
    cfg: &SingleActionRunnerConfig,
    wallet: &W,
) -> Result<RoomBindAddressRequest, RuntimeError>
where
    W: WalletAdapter + ?Sized,
{
    let mut req = skill
        .bind_seat_address(cfg.seat_address.clone())
        .await
        .map_err(|e| RuntimeError::AgentSkill(e.to_string()))?;
    let params = serde_json::json!({
        "seat_id": req.seat_id,
        "seat_address": req.seat_address,
    });
    sign_request_meta_with_wallet(
        cfg,
        wallet,
        "room.bind_address",
        None,
        None,
        &params,
        &mut req.request_meta,
    )
    .await?;
    Ok(req)
}

async fn build_signed_bind_session_keys_request<W>(
    skill: &mut LocalAgentSkill,
    cfg: &SingleActionRunnerConfig,
    wallet: &W,
) -> Result<RoomBindSessionKeysRequest, RuntimeError>
where
    W: WalletAdapter + ?Sized,
{
    let mut req = skill
        .build_bind_session_keys_request_unsigned(
            cfg.card_encrypt_pubkey_hex.clone(),
            "x25519+evm".to_string(),
        )
        .map_err(|e| RuntimeError::AgentSkill(e.to_string()))?;
    let claim = SeatKeyBindingClaim {
        agent_id: None,
        session_id: None,
        room_id: req.room_id,
        seat_id: req.seat_id,
        seat_address: req.seat_address.clone(),
        card_encrypt_pubkey: req.card_encrypt_pubkey.clone(),
        request_verify_pubkey: req.request_verify_pubkey.clone(),
        key_algo: req.key_algo.clone(),
        issued_at: req.request_meta.request_ts,
        expires_at: None,
    };
    req.proof_signature = wallet
        .sign_personal_message(&cfg.seat_address, build_seat_key_binding_message(&claim).as_bytes())
        .await
        .map_err(|e| RuntimeError::Wallet(e.to_string()))?;
    let params = serde_json::json!({
        "seat_id": req.seat_id,
        "seat_address": req.seat_address,
        "card_encrypt_pubkey": req.card_encrypt_pubkey,
        "request_verify_pubkey": req.request_verify_pubkey,
        "key_algo": req.key_algo,
        "proof_signature": req.proof_signature,
    });
    sign_request_meta_with_wallet(
        cfg,
        wallet,
        "room.bind_session_keys",
        None,
        None,
        &params,
        &mut req.request_meta,
    )
    .await?;
    Ok(req)
}

async fn build_signed_room_ready_request<W>(
    skill: &LocalAgentSkill,
    cfg: &SingleActionRunnerConfig,
    wallet: &W,
) -> Result<RoomReadyRequest, RuntimeError>
where
    W: WalletAdapter + ?Sized,
{
    let mut req = skill
        .build_room_ready_request()
        .map_err(|e| RuntimeError::AgentSkill(e.to_string()))?;
    let params = serde_json::json!({ "seat_id": req.seat_id });
    sign_request_meta_with_wallet(cfg, wallet, "room.ready", None, None, &params, &mut req.request_meta).await?;
    Ok(req)
}

async fn build_signed_game_act_request<W>(
    skill: &LocalAgentSkill,
    cfg: &SingleActionRunnerConfig,
    wallet: &W,
    hand_id: poker_domain::HandId,
    action_seq: u32,
    action_type: ActionType,
    amount: Option<String>,
    tx_hash: Option<String>,
) -> Result<GameActRequest, RuntimeError>
where
    W: WalletAdapter + ?Sized,
{
    let mut req = skill
        .build_game_act_request(hand_id, action_seq, action_type, amount, tx_hash)
        .await
        .map_err(|e| RuntimeError::AgentSkill(e.to_string()))?;
    let params = serde_json::json!({
        "seat_id": req.seat_id,
        "action_type": req.action_type,
        "amount": req.amount,
        "tx_hash": req.tx_hash,
    });
    sign_request_meta_with_wallet(
        cfg,
        wallet,
        "game.act",
        Some(req.hand_id.0.to_string()),
        Some(req.action_seq),
        &params,
        &mut req.request_meta,
    )
    .await?;
    Ok(req)
}

pub async fn prepare_seat<W>(
    cfg: SingleActionRunnerConfig,
    wallet: &W,
) -> Result<PreparedSeatOutcome, RuntimeError>
where
    W: WalletAdapter + ?Sized,
{
    let mut skill = connect_skill(&cfg).await?;

    let bind_addr_req = build_signed_bind_address_request(&mut skill, &cfg, wallet).await?;
    let bind_addr_env: ResponseEnvelope<()> = http_rpc_call(
        &cfg.rpc_endpoint,
        "room.bind_address",
        serde_json::to_value(bind_addr_req).map_err(|e| RuntimeError::Http(e.to_string()))?,
    )
    .await?;
    envelope_ok(bind_addr_env)?;

    let bind_keys_req = build_signed_bind_session_keys_request(&mut skill, &cfg, wallet).await?;
    let bind_keys_env: ResponseEnvelope<()> = http_rpc_call(
        &cfg.rpc_endpoint,
        "room.bind_session_keys",
        serde_json::to_value(bind_keys_req).map_err(|e| RuntimeError::Http(e.to_string()))?,
    )
    .await?;
    envelope_ok(bind_keys_env)?;

    let ready_req = build_signed_room_ready_request(&skill, &cfg, wallet).await?;
    let ready_env: ResponseEnvelope<()> = http_rpc_call(
        &cfg.rpc_endpoint,
        "room.ready",
        serde_json::to_value(ready_req).map_err(|e| RuntimeError::Http(e.to_string()))?,
    )
    .await?;
    envelope_ok(ready_env)?;

    Ok(PreparedSeatOutcome)
}

pub async fn run_single_action_if_turn<W>(
    cfg: SingleActionRunnerConfig,
    wallet: &W,
) -> Result<Option<SingleActionRunOutcome>, RuntimeError>
where
    W: WalletAdapter + ?Sized,
{
    let skill = connect_skill(&cfg).await?;

    let state_req = skill
        .build_get_state_request(None)
        .map_err(|e| RuntimeError::AgentSkill(e.to_string()))?;
    let state_env: ResponseEnvelope<Option<HandSnapshot>> = http_rpc_call(
        &cfg.rpc_endpoint,
        "game.get_state",
        serde_json::to_value(state_req).map_err(|e| RuntimeError::Http(e.to_string()))?,
    )
    .await?;
    let state = envelope_ok(state_env)?.ok_or(RuntimeError::StateUnavailable)?;
    if state.acting_seat_id != Some(cfg.seat_id) {
        return Ok(None);
    }

    let legal_req = skill
        .build_get_legal_actions_request()
        .map_err(|e| RuntimeError::AgentSkill(e.to_string()))?;
    let legal_env: ResponseEnvelope<Vec<LegalAction>> = http_rpc_call(
        &cfg.rpc_endpoint,
        "game.get_legal_actions",
        serde_json::to_value(legal_req).map_err(|e| RuntimeError::Http(e.to_string()))?,
    )
    .await?;
    let legal_actions = envelope_ok(legal_env)?
        .into_iter()
        .map(|a| format!("{:?}", a.action_type).to_lowercase())
        .collect::<Vec<_>>();

    let policy = RulePolicyAdapter;
    let decision = policy
        .decide_action(&PolicyDecisionInput {
            room_id: cfg.room_id,
            hand_id: state.hand_id,
            seat_id: cfg.seat_id,
            legal_actions,
            public_state_json: serde_json::json!({}),
            private_state_json: None,
        })
        .await
        .map_err(|e| RuntimeError::Policy(e.to_string()))?
        .ok_or_else(|| RuntimeError::Policy("no policy decision".to_string()))?;
    let action_type = decision.action_type;
    if action_type != ActionType::Check {
        return Err(RuntimeError::Policy(format!(
            "unexpected policy action: {action_type:?}"
        )));
    }

    let tx_receipt = wallet
        .transfer_to_room(EvmTransferRequest {
            room_id: cfg.room_id,
            from: cfg.seat_address.clone(),
            to: cfg.room_chain_address.clone(),
            value: cfg.tx_value,
            chain_id: cfg.chain_id,
        })
        .await
        .map_err(|e: WalletAdapterError| RuntimeError::Wallet(e.to_string()))?;

    let act_req = build_signed_game_act_request(
        &skill,
        &cfg,
        wallet,
        state.hand_id,
        state.next_action_seq,
        ActionType::Check,
        None,
        Some(tx_receipt.tx_hash.clone()),
    )
    .await?;
    let act_env: ResponseEnvelope<()> = http_rpc_call(
        &cfg.rpc_endpoint,
        "game.act",
        serde_json::to_value(act_req).map_err(|e| RuntimeError::Http(e.to_string()))?,
    )
    .await?;
    envelope_ok(act_env)?;

    Ok(Some(SingleActionRunOutcome {
        hand_id: state.hand_id,
        action_seq: state.next_action_seq,
        tx_hash: tx_receipt.tx_hash,
    }))
}

pub async fn run_single_action_turn<W>(
    cfg: SingleActionRunnerConfig,
    wallet: &W,
) -> Result<SingleActionRunOutcome, RuntimeError>
where
    W: WalletAdapter + ?Sized,
{
    prepare_seat(cfg.clone(), wallet).await?;
    run_single_action_if_turn(cfg, wallet)
        .await?
        .ok_or(RuntimeError::NotOurTurn)
}

pub async fn run_first_available_action<W>(
    seats: &[SeatTurnRunnerInput<'_>],
    wallet: &W,
) -> Result<Option<(SeatId, SingleActionRunOutcome)>, RuntimeError>
where
    W: WalletAdapter + ?Sized,
{
    for seat in seats {
        if let Some(outcome) = run_single_action_if_turn(seat.cfg.clone(), wallet).await? {
            return Ok(Some((seat.cfg.seat_id, outcome)));
        }
    }
    Ok(None)
}
