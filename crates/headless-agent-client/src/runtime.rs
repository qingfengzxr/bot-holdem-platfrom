use agent_auth::{SeatKeyBindingClaim, SigningMessageInput, build_seat_key_binding_message, build_signing_message};
use agent_sdk::{AgentSkill, AgentSkillConfig, LocalAgentSkill};
use platform_core::ResponseEnvelope;
use poker_domain::{ActionType, Chips, HandSnapshot, LegalAction, RoomId, SeatId};
use rpc_gateway::{
    GameActRequest, GameGetPrivatePayloadsRequest, PrivatePayloadEvent, RoomBindAddressRequest,
    RoomBindSessionKeysRequest, RoomReadyRequest,
};
use seat_crypto::{HoleCardsDealtCipherPayload, decrypt_with_recipient_x25519};
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
    #[error("crypto error: {0}")]
    Crypto(String),
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
    pub card_encrypt_secret_hex: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SingleActionRunOutcome {
    pub hand_id: poker_domain::HandId,
    pub action_seq: u32,
    pub tx_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PreparedSeatOutcome;

#[derive(Debug, Clone)]
pub struct TurnDecisionInput {
    pub policy_input: PolicyDecisionInput,
    pub hand_id: poker_domain::HandId,
    pub action_seq: u32,
    pub legal_actions_raw: Vec<LegalAction>,
}

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
    if let Some(err) = payload.get("error") {
        let code = err.get("code").cloned().unwrap_or(Value::Null);
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown json-rpc error");
        let data = err.get("data").cloned().unwrap_or(Value::Null);
        return Err(RuntimeError::RpcApp(format!(
            "jsonrpc {method} failed: code={code} message={message} data={data}"
        )));
    }
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

fn envelope_ok_unit(env: ResponseEnvelope<()>) -> Result<(), RuntimeError> {
    if !env.ok {
        let msg = env
            .error
            .map(|e| format!("{:?}: {}", e.code, e.message))
            .unwrap_or_else(|| "unknown rpc app error".to_string());
        return Err(RuntimeError::RpcApp(msg));
    }
    Ok(())
}

fn parse_x25519_secret_hex(secret_hex: &str) -> Result<[u8; 32], RuntimeError> {
    let bytes = hex::decode(secret_hex).map_err(|e| RuntimeError::Crypto(e.to_string()))?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| {
        RuntimeError::Crypto("x25519 secret hex must decode to 32 bytes".to_string())
    })?;
    Ok(arr)
}

fn build_private_state_json(
    events: Vec<PrivatePayloadEvent>,
    card_encrypt_secret_hex: Option<&str>,
) -> Result<Option<Value>, RuntimeError> {
    if events.is_empty() {
        return Ok(None);
    }

    let secret = card_encrypt_secret_hex.map(parse_x25519_secret_hex).transpose()?;
    let mut decrypted_hole_cards = Vec::new();
    let mut decrypt_failures = 0_u32;

    for event in &events {
        if event.event_name != "hole_cards_dealt" {
            continue;
        }
        let payload: HoleCardsDealtCipherPayload = match serde_json::from_value(event.payload.clone()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(secret) = secret.as_ref() else {
            continue;
        };
        match decrypt_with_recipient_x25519(secret, &payload.aad, &payload.envelope) {
            Ok(plaintext) => {
                if let Ok(v) = serde_json::from_slice::<Value>(&plaintext) {
                    decrypted_hole_cards.push(v);
                }
            }
            Err(_) => {
                decrypt_failures = decrypt_failures.saturating_add(1);
            }
        }
    }

    Ok(Some(serde_json::json!({
        "events": events,
        "decrypted_hole_cards": decrypted_hole_cards,
        "decrypt_failures": decrypt_failures,
    })))
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

async fn build_signed_room_join_request<W>(
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
    sign_request_meta_with_wallet(cfg, wallet, "room.join", None, None, &params, &mut req.request_meta)
        .await?;
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

    let join_req = build_signed_room_join_request(&skill, &cfg, wallet).await?;
    let join_env: ResponseEnvelope<()> = http_rpc_call(
        &cfg.rpc_endpoint,
        "room.join",
        serde_json::to_value(join_req).map_err(|e| RuntimeError::Http(e.to_string()))?,
    )
    .await?;
    envelope_ok_unit(join_env)?;

    let bind_addr_req = build_signed_bind_address_request(&mut skill, &cfg, wallet).await?;
    let bind_addr_env: ResponseEnvelope<()> = http_rpc_call(
        &cfg.rpc_endpoint,
        "room.bind_address",
        serde_json::to_value(bind_addr_req).map_err(|e| RuntimeError::Http(e.to_string()))?,
    )
    .await?;
    envelope_ok_unit(bind_addr_env)?;

    let bind_keys_req = build_signed_bind_session_keys_request(&mut skill, &cfg, wallet).await?;
    let bind_keys_env: ResponseEnvelope<()> = http_rpc_call(
        &cfg.rpc_endpoint,
        "room.bind_session_keys",
        serde_json::to_value(bind_keys_req).map_err(|e| RuntimeError::Http(e.to_string()))?,
    )
    .await?;
    envelope_ok_unit(bind_keys_env)?;

    let ready_req = build_signed_room_ready_request(&skill, &cfg, wallet).await?;
    let ready_env: ResponseEnvelope<()> = http_rpc_call(
        &cfg.rpc_endpoint,
        "room.ready",
        serde_json::to_value(ready_req).map_err(|e| RuntimeError::Http(e.to_string()))?,
    )
    .await?;
    envelope_ok_unit(ready_env)?;

    Ok(PreparedSeatOutcome)
}

pub async fn run_single_action_if_turn<W>(
    cfg: SingleActionRunnerConfig,
    wallet: &W,
) -> Result<Option<SingleActionRunOutcome>, RuntimeError>
where
    W: WalletAdapter + ?Sized,
{
    let Some(turn) = collect_turn_decision_input(cfg.clone(), None).await? else {
        return Ok(None);
    };
    let policy = RulePolicyAdapter;
    let decision = policy
        .decide_action(&turn.policy_input)
        .await
        .map_err(|e| RuntimeError::Policy(e.to_string()))?
        .ok_or_else(|| RuntimeError::Policy("no policy decision".to_string()))?;
    execute_turn_decision(cfg, wallet, &turn, &decision).await.map(Some)
}

pub async fn collect_turn_decision_input(
    cfg: SingleActionRunnerConfig,
    decision_context_json: Option<serde_json::Value>,
) -> Result<Option<TurnDecisionInput>, RuntimeError> {
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
    let legal_actions_raw = envelope_ok(legal_env)?;
    let legal_actions = legal_actions_raw
        .iter()
        .map(|a| format!("{:?}", a.action_type).to_lowercase())
        .collect::<Vec<_>>();
    let private_payload_req = GameGetPrivatePayloadsRequest {
        room_id: cfg.room_id,
        seat_id: cfg.seat_id,
        hand_id: Some(state.hand_id),
    };
    let private_payloads_env: ResponseEnvelope<Vec<PrivatePayloadEvent>> = http_rpc_call(
        &cfg.rpc_endpoint,
        "game.get_private_payloads",
        serde_json::to_value(private_payload_req).map_err(|e| RuntimeError::Http(e.to_string()))?,
    )
    .await?;
    let private_state_json = build_private_state_json(
        envelope_ok(private_payloads_env)?,
        cfg.card_encrypt_secret_hex.as_deref(),
    )?;

    Ok(Some(TurnDecisionInput {
        policy_input: PolicyDecisionInput {
            room_id: cfg.room_id,
            hand_id: state.hand_id,
            seat_id: cfg.seat_id,
            legal_actions,
            public_state_json: serde_json::json!({}),
            private_state_json,
            decision_context_json,
        },
        hand_id: state.hand_id,
        action_seq: state.next_action_seq,
        legal_actions_raw,
    }))
}

fn resolve_action_amount_from_legal_actions(
    decision: &crate::policy_adapter::PolicyDecision,
    legal_actions_raw: &[LegalAction],
) -> Result<Option<Chips>, RuntimeError> {
    match decision.action_type {
        ActionType::Fold | ActionType::Check => Ok(None),
        ActionType::Call => {
            let call = legal_actions_raw
                .iter()
                .find(|a| a.action_type == ActionType::Call)
                .ok_or_else(|| RuntimeError::Policy("call not present in legal actions".to_string()))?;
            Ok(call.min_amount.or(decision.amount))
        }
        ActionType::RaiseTo | ActionType::AllIn => Ok(Some(decision.amount.ok_or(
            RuntimeError::Policy("raise_to/all_in requires explicit amount".to_string()),
        )?)),
    }
}

pub async fn execute_turn_decision<W>(
    cfg: SingleActionRunnerConfig,
    wallet: &W,
    turn: &TurnDecisionInput,
    decision: &crate::policy_adapter::PolicyDecision,
) -> Result<SingleActionRunOutcome, RuntimeError>
where
    W: WalletAdapter + ?Sized,
{
    let skill = connect_skill(&cfg).await?;

    let resolved_amount = resolve_action_amount_from_legal_actions(decision, &turn.legal_actions_raw)?;
    let amount = resolved_amount.map(|v| v.as_u128().to_string());
    let tx_hash = if let Some(amount_chips) = resolved_amount {
        let tx_receipt = wallet
            .transfer_to_room(EvmTransferRequest {
                room_id: cfg.room_id,
                from: cfg.seat_address.clone(),
                to: cfg.room_chain_address.clone(),
                value: amount_chips,
                chain_id: cfg.chain_id,
            })
            .await
            .map_err(|e: WalletAdapterError| RuntimeError::Wallet(e.to_string()))?;
        Some(tx_receipt.tx_hash)
    } else {
        None
    };
    let act_req = build_signed_game_act_request(
        &skill,
        &cfg,
        wallet,
        turn.hand_id,
        turn.action_seq,
        decision.action_type,
        amount,
        tx_hash.clone(),
    )
    .await?;
    let act_env: ResponseEnvelope<()> = http_rpc_call(
        &cfg.rpc_endpoint,
        "game.act",
        serde_json::to_value(act_req).map_err(|e| RuntimeError::Http(e.to_string()))?,
    )
    .await?;
    envelope_ok_unit(act_env)?;

    Ok(SingleActionRunOutcome {
        hand_id: turn.hand_id,
        action_seq: turn.action_seq,
        tx_hash,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use platform_core::ResponseEnvelope;
    use seat_crypto::{SeatEventAad, X25519KeyPair, encrypt_hole_cards_dealt_payload};

    #[test]
    fn envelope_ok_unit_accepts_ok_with_null_data() {
        let env = ResponseEnvelope::<()> {
            ok: true,
            data: None,
            error: None,
        };
        let res = envelope_ok_unit(env);
        assert!(res.is_ok());
    }

    #[test]
    fn private_state_contains_events_without_secret() {
        let event = PrivatePayloadEvent {
            room_id: RoomId::new(),
            hand_id: poker_domain::HandId::new(),
            seat_id: 0,
            event_name: "hole_cards_dealt".to_string(),
            payload: serde_json::json!({"foo":"bar"}),
        };
        let state = build_private_state_json(vec![event], None)
            .expect("private state")
            .expect("some");
        assert_eq!(state["events"].as_array().map_or(0, Vec::len), 1);
        assert_eq!(state["decrypted_hole_cards"].as_array().map_or(0, Vec::len), 0);
    }

    #[test]
    fn private_state_decrypts_hole_cards_when_secret_provided() {
        let pair = X25519KeyPair::generate();
        let aad = SeatEventAad {
            room_id: RoomId::new(),
            hand_id: poker_domain::HandId::new(),
            seat_id: 1,
            event_seq: 1,
        };
        let payload = encrypt_hole_cards_dealt_payload(
            "k1",
            &pair.public,
            aad.clone(),
            br#"{"cards":[7,42]}"#,
        )
        .expect("encrypt");
        let event = PrivatePayloadEvent {
            room_id: aad.room_id,
            hand_id: aad.hand_id,
            seat_id: aad.seat_id,
            event_name: "hole_cards_dealt".to_string(),
            payload: serde_json::to_value(payload).expect("payload json"),
        };
        let state = build_private_state_json(vec![event], Some(&hex::encode(pair.secret)))
            .expect("private state")
            .expect("some");
        assert_eq!(state["decrypted_hole_cards"][0]["cards"][0], 7);
        assert_eq!(state["decrypted_hole_cards"][0]["cards"][1], 42);
        assert_eq!(state["decrypt_failures"], 0);
    }

    #[test]
    fn resolve_amount_ignores_non_monetary_amounts_for_fold_and_check() {
        let legal_actions = vec![
            LegalAction {
                action_type: ActionType::Fold,
                min_amount: None,
                max_amount: None,
            },
            LegalAction {
                action_type: ActionType::Check,
                min_amount: None,
                max_amount: None,
            },
        ];
        let fold = crate::policy_adapter::PolicyDecision {
            action_type: ActionType::Fold,
            amount: Some(Chips(0)),
            rationale: None,
            source: crate::policy_adapter::PolicySource::Llm,
        };
        let check = crate::policy_adapter::PolicyDecision {
            action_type: ActionType::Check,
            amount: Some(Chips(0)),
            rationale: None,
            source: crate::policy_adapter::PolicySource::Llm,
        };

        assert_eq!(
            resolve_action_amount_from_legal_actions(&fold, &legal_actions).expect("fold amount"),
            None
        );
        assert_eq!(
            resolve_action_amount_from_legal_actions(&check, &legal_actions).expect("check amount"),
            None
        );
    }

    #[test]
    fn resolve_amount_uses_call_min_amount() {
        let legal_actions = vec![LegalAction {
            action_type: ActionType::Call,
            min_amount: Some(Chips(200)),
            max_amount: Some(Chips(200)),
        }];
        let call = crate::policy_adapter::PolicyDecision {
            action_type: ActionType::Call,
            amount: None,
            rationale: None,
            source: crate::policy_adapter::PolicySource::Llm,
        };

        assert_eq!(
            resolve_action_amount_from_legal_actions(&call, &legal_actions).expect("call amount"),
            Some(Chips(200))
        );
    }
}
