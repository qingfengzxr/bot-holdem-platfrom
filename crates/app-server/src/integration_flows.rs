#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::time::Duration;

    use agent_auth::{
        SeatKeyBindingClaim, SignedRequestMeta, SigningMessageInput, build_seat_key_binding_message,
        build_signing_message, evm_address_from_secp256k1_verifying_key,
        sign_evm_personal_message_secp256k1,
    };
    use app_server_dummy_private_support::*;
    use chain_watcher::{
        EvmChainWatcher, ReqwestEvmJsonRpcClient, RetryPolicy, TxVerificationInput,
        VerificationCallbackSink,
    };
    use chrono::Utc;
    use headless_agent_client::{
        runtime::{
            SeatTurnRunnerInput, SingleActionRunnerConfig, prepare_seat, run_first_available_action,
        },
        wallet_adapter::JsonRpcEvmWalletAdapter,
    };
    use jsonrpsee::server::ServerBuilder;
    use k256::ecdsa::SigningKey as EvmSigningKey;
    use poker_domain::{HandStatus, RequestId, RoomId};
    use rpc_gateway::{
        GameActRequest, GameGetStateRequest, InMemoryEventBus, RoomBindAddressRequest,
        RoomBindSessionKeysRequest, RoomReadyRequest, RpcGateway,
    };
    use serde_json::Value;

    use crate::chain_callback_sink::AppChainCallbackSink;
    use crate::room_service_port::AppRoomService;
    use crate::rpc_request_signature_verifier::AppRpcRequestSignatureVerifier;

    // Local support types re-exported through a private submodule to keep imports explicit.
    mod app_server_dummy_private_support {
        pub use audit_store::InMemoryAuditRepository;
        pub use chain_watcher::{
            TxVerificationCallback, VerificationStatus,
        };
        pub use ledger_store::InMemoryChainTxRepository;
        pub use table_service::NoopRoomEventSink;
    }

    fn signed_meta() -> SignedRequestMeta {
        SignedRequestMeta {
            request_id: RequestId::new(),
            request_nonce: RequestId::new().0.to_string(),
            request_ts: Utc::now(),
            request_expiry_ms: 30_000,
            signature_pubkey_id: "k1".to_string(),
            signature: "sig".to_string(),
        }
    }

    fn signed_bind_address_request(
        room_id: RoomId,
        seat_id: u8,
        signing_key: &EvmSigningKey,
    ) -> RoomBindAddressRequest {
        let seat_address = evm_address_from_secp256k1_verifying_key(signing_key.verifying_key());
        let mut request_meta = signed_meta();
        request_meta.signature_pubkey_id = seat_address.clone();
        let params = serde_json::json!({
            "seat_id": seat_id,
            "seat_address": seat_address,
        });
        let message = build_signing_message(&SigningMessageInput {
            method: "room.bind_address",
            session_id: poker_domain::SessionId(uuid::Uuid::nil()),
            room_id: Some(room_id),
            hand_id: None,
            action_seq: None,
            params: &params,
            meta: &request_meta,
        })
        .expect("build signing message");
        request_meta.signature =
            sign_evm_personal_message_secp256k1(signing_key, message.as_bytes()).expect("sign");
        RoomBindAddressRequest {
            room_id,
            seat_id,
            seat_address: params["seat_address"]
                .as_str()
                .expect("seat address")
                .to_string(),
            request_meta,
        }
    }

    fn signed_room_ready_request(
        room_id: RoomId,
        seat_id: u8,
        signing_key: &EvmSigningKey,
    ) -> RoomReadyRequest {
        let seat_address = evm_address_from_secp256k1_verifying_key(signing_key.verifying_key());
        let mut request_meta = signed_meta();
        request_meta.signature_pubkey_id = seat_address;
        let params = serde_json::json!({
            "seat_id": seat_id,
        });
        let message = build_signing_message(&SigningMessageInput {
            method: "room.ready",
            session_id: poker_domain::SessionId(uuid::Uuid::nil()),
            room_id: Some(room_id),
            hand_id: None,
            action_seq: None,
            params: &params,
            meta: &request_meta,
        })
        .expect("build signing message");
        request_meta.signature =
            sign_evm_personal_message_secp256k1(signing_key, message.as_bytes()).expect("sign");
        RoomReadyRequest {
            room_id,
            seat_id,
            request_meta,
        }
    }

    fn signed_bind_session_keys_request(
        room_id: RoomId,
        seat_id: u8,
        signing_key: &EvmSigningKey,
    ) -> RoomBindSessionKeysRequest {
        let seat_address = evm_address_from_secp256k1_verifying_key(signing_key.verifying_key());
        let mut request_meta = signed_meta();
        request_meta.signature_pubkey_id = seat_address.clone();
        let card_encrypt_pubkey = "11".repeat(32);
        let request_verify_pubkey = "22".repeat(32);
        let key_algo = "x25519+evm".to_string();
        let claim = SeatKeyBindingClaim {
            agent_id: None,
            session_id: None,
            room_id,
            seat_id,
            seat_address: seat_address.clone(),
            card_encrypt_pubkey: card_encrypt_pubkey.clone(),
            request_verify_pubkey: request_verify_pubkey.clone(),
            key_algo: key_algo.clone(),
            issued_at: request_meta.request_ts,
            expires_at: None,
        };
        let proof_signature = sign_evm_personal_message_secp256k1(
            signing_key,
            build_seat_key_binding_message(&claim).as_bytes(),
        )
        .expect("proof sign");
        let params = serde_json::json!({
            "seat_id": seat_id,
            "seat_address": seat_address,
            "card_encrypt_pubkey": card_encrypt_pubkey,
            "request_verify_pubkey": request_verify_pubkey,
            "key_algo": key_algo,
            "proof_signature": proof_signature,
        });
        let message = build_signing_message(&SigningMessageInput {
            method: "room.bind_session_keys",
            session_id: poker_domain::SessionId(uuid::Uuid::nil()),
            room_id: Some(room_id),
            hand_id: None,
            action_seq: None,
            params: &params,
            meta: &request_meta,
        })
        .expect("build signing message");
        request_meta.signature =
            sign_evm_personal_message_secp256k1(signing_key, message.as_bytes()).expect("sign");
        RoomBindSessionKeysRequest {
            room_id,
            seat_id,
            seat_address: params["seat_address"].as_str().expect("seat").to_string(),
            card_encrypt_pubkey: params["card_encrypt_pubkey"]
                .as_str()
                .expect("encpk")
                .to_string(),
            request_verify_pubkey: params["request_verify_pubkey"]
                .as_str()
                .expect("sigpk")
                .to_string(),
            key_algo: params["key_algo"].as_str().expect("algo").to_string(),
            proof_signature: params["proof_signature"].as_str().expect("proof").to_string(),
            request_meta,
        }
    }

    fn signed_game_act_request(
        room_id: RoomId,
        hand_id: poker_domain::HandId,
        action_seq: u32,
        seat_id: u8,
        action_type: &str,
        tx_hash: Option<String>,
        signing_key: &EvmSigningKey,
    ) -> GameActRequest {
        let seat_address = evm_address_from_secp256k1_verifying_key(signing_key.verifying_key());
        let mut request_meta = signed_meta();
        request_meta.signature_pubkey_id = seat_address;
        let params = serde_json::json!({
            "seat_id": seat_id,
            "action_type": action_type,
            "amount": Value::Null,
            "tx_hash": tx_hash,
        });
        let message = build_signing_message(&SigningMessageInput {
            method: "game.act",
            session_id: poker_domain::SessionId(uuid::Uuid::nil()),
            room_id: Some(room_id),
            hand_id: Some(hand_id.0.to_string()),
            action_seq: Some(action_seq),
            params: &params,
            meta: &request_meta,
        })
        .expect("build signing message");
        request_meta.signature =
            sign_evm_personal_message_secp256k1(signing_key, message.as_bytes()).expect("sign");
        GameActRequest {
            room_id,
            hand_id,
            action_seq,
            seat_id,
            action_type: action_type.to_string(),
            amount: None,
            tx_hash: params["tx_hash"].as_str().map(ToString::to_string),
            request_meta,
        }
    }

    async fn http_rpc_call(endpoint: &str, method: &str, params: Value) -> Value {
        let resp = reqwest::Client::new()
            .post(endpoint)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params,
            }))
            .send()
            .await
            .expect("jsonrpc send")
            .json::<Value>()
            .await
            .expect("jsonrpc payload");
        let result = resp.get("result").cloned().expect("jsonrpc result");
        result
    }

    async fn anvil_accounts(endpoint: &str) -> Vec<String> {
        let resp = reqwest::Client::new()
            .post(endpoint)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_accounts",
                "params": [],
            }))
            .send()
            .await
            .expect("eth_accounts send")
            .json::<Value>()
            .await
            .expect("eth_accounts json");
        resp["result"]
            .as_array()
            .expect("accounts array")
            .iter()
            .filter_map(|v| v.as_str().map(ToString::to_string))
            .collect()
    }

    #[tokio::test]
    async fn room_flow_works_via_rpc_gateway_handlers() {
        let gateway = RpcGateway::new();
        let chain_repo = Arc::new(InMemoryChainTxRepository::new());
        let room_service = AppRoomService::with_sinks(Arc::new(NoopRoomEventSink), chain_repo);
        let room_id = RoomId::new();
        let room = room_service.ensure_room(room_id).expect("room");
        room.join(0).await.expect("join seat0");
        room.join(1).await.expect("join seat1");

        let bind_addr = gateway
            .handle_bind_address(
                &room_service,
                RoomBindAddressRequest {
                    room_id,
                    seat_id: 0,
                    seat_address: "cfx:seat0".to_string(),
                    request_meta: signed_meta(),
                },
            )
            .await;
        assert!(bind_addr.ok, "{bind_addr:?}");

        // Keep proof-generation complexity out of this integration test; keys are bound via room actor.
        room.bind_session_keys(
            0,
            "encpk".to_string(),
            "sigpk".to_string(),
            "x25519+evm".to_string(),
            "proof".to_string(),
        )
        .await
        .expect("bind keys");

        let ready = gateway
            .handle_room_ready(
                &room_service,
                RoomReadyRequest {
                    room_id,
                    seat_id: 0,
                    request_meta: signed_meta(),
                },
            )
            .await;
        assert!(ready.ok, "{ready:?}");

        let state = gateway
            .handle_get_state(
                &room_service,
                GameGetStateRequest {
                    room_id,
                    hand_id: None,
                    seat_id: None,
                },
            )
            .await;
        assert!(state.ok, "{state:?}");
        let snapshot = state.data.flatten().expect("snapshot");
        assert_eq!(snapshot.status, HandStatus::Running);
        assert_eq!(snapshot.acting_seat_id, Some(0));
    }

    #[tokio::test]
    async fn bind_address_with_valid_evm_signature_is_accepted_via_gateway_verifier() {
        let gateway = RpcGateway::new();
        let chain_repo = Arc::new(InMemoryChainTxRepository::new());
        let room_service = AppRoomService::with_sinks(Arc::new(NoopRoomEventSink), chain_repo);
        let sig_verifier = AppRpcRequestSignatureVerifier::new(room_service.clone());
        let room_id = RoomId::new();
        let room = room_service.ensure_room(room_id).expect("room");
        room.join(0).await.expect("join seat0");

        let evm_key = EvmSigningKey::from_slice(&[21_u8; 32]).expect("evm key");
        let req = signed_bind_address_request(room_id, 0, &evm_key);
        let resp = gateway
            .handle_bind_address_with_audit(&room_service, req, None, None, Some(&sig_verifier))
            .await;
        assert!(resp.ok, "{resp:?}");
    }

    #[tokio::test]
    async fn bind_address_with_invalid_evm_signature_is_rejected_via_gateway_verifier() {
        let gateway = RpcGateway::new();
        let chain_repo = Arc::new(InMemoryChainTxRepository::new());
        let room_service = AppRoomService::with_sinks(Arc::new(NoopRoomEventSink), chain_repo);
        let sig_verifier = AppRpcRequestSignatureVerifier::new(room_service.clone());
        let room_id = RoomId::new();
        let room = room_service.ensure_room(room_id).expect("room");
        room.join(0).await.expect("join seat0");

        let evm_key = EvmSigningKey::from_slice(&[22_u8; 32]).expect("evm key");
        let mut req = signed_bind_address_request(room_id, 0, &evm_key);
        req.request_meta.signature = "0xdeadbeef".to_string();
        let resp = gateway
            .handle_bind_address_with_audit(&room_service, req, None, None, Some(&sig_verifier))
            .await;
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.expect("error");
        assert_eq!(err.code, platform_core::ErrorCode::RequestInvalid);
    }

    #[tokio::test]
    async fn room_ready_with_valid_evm_signature_is_accepted_via_gateway_verifier() {
        let gateway = RpcGateway::new();
        let chain_repo = Arc::new(InMemoryChainTxRepository::new());
        let room_service = AppRoomService::with_sinks(Arc::new(NoopRoomEventSink), chain_repo);
        let sig_verifier = AppRpcRequestSignatureVerifier::new(room_service.clone());
        let room_id = RoomId::new();
        let room = room_service.ensure_room(room_id).expect("room");
        room.join(0).await.expect("join seat0");

        let evm_key = EvmSigningKey::from_slice(&[23_u8; 32]).expect("evm key");
        let bind = gateway
            .handle_bind_address_with_audit(
                &room_service,
                signed_bind_address_request(room_id, 0, &evm_key),
                None,
                None,
                Some(&sig_verifier),
            )
            .await;
        assert!(bind.ok, "{bind:?}");
        room.bind_session_keys(
            0,
            "encpk".to_string(),
            "sigpk".to_string(),
            "x25519+evm".to_string(),
            "proof".to_string(),
        )
        .await
        .expect("bind keys");

        let resp = gateway
            .handle_room_ready_with_audit(
                &room_service,
                signed_room_ready_request(room_id, 0, &evm_key),
                None,
                None,
                Some(&sig_verifier),
            )
            .await;
        assert!(resp.ok, "{resp:?}");
    }

    #[tokio::test]
    async fn bind_session_keys_with_valid_evm_signature_is_accepted_via_gateway_verifier() {
        let gateway = RpcGateway::new();
        let chain_repo = Arc::new(InMemoryChainTxRepository::new());
        let room_service = AppRoomService::with_sinks(Arc::new(NoopRoomEventSink), chain_repo);
        let sig_verifier = AppRpcRequestSignatureVerifier::new(room_service.clone());
        let room_id = RoomId::new();
        let room = room_service.ensure_room(room_id).expect("room");
        room.join(0).await.expect("join seat0");

        let evm_key = EvmSigningKey::from_slice(&[27_u8; 32]).expect("evm key");
        let bind = gateway
            .handle_bind_address_with_audit(
                &room_service,
                signed_bind_address_request(room_id, 0, &evm_key),
                None,
                None,
                Some(&sig_verifier),
            )
            .await;
        assert!(bind.ok, "{bind:?}");

        let resp = gateway
            .handle_bind_session_keys_with_audit(
                &room_service,
                signed_bind_session_keys_request(room_id, 0, &evm_key),
                None,
                None,
                Some(&sig_verifier),
            )
            .await;
        assert!(resp.ok, "{resp:?}");
    }

    #[tokio::test]
    async fn bind_session_keys_with_invalid_evm_signature_is_rejected_via_gateway_verifier() {
        let gateway = RpcGateway::new();
        let chain_repo = Arc::new(InMemoryChainTxRepository::new());
        let room_service = AppRoomService::with_sinks(Arc::new(NoopRoomEventSink), chain_repo);
        let sig_verifier = AppRpcRequestSignatureVerifier::new(room_service.clone());
        let room_id = RoomId::new();
        let room = room_service.ensure_room(room_id).expect("room");
        room.join(0).await.expect("join seat0");

        let evm_key = EvmSigningKey::from_slice(&[28_u8; 32]).expect("evm key");
        let bind = gateway
            .handle_bind_address_with_audit(
                &room_service,
                signed_bind_address_request(room_id, 0, &evm_key),
                None,
                None,
                Some(&sig_verifier),
            )
            .await;
        assert!(bind.ok, "{bind:?}");

        let mut req = signed_bind_session_keys_request(room_id, 0, &evm_key);
        req.request_meta.signature = "0xdeadbeef".to_string();
        let resp = gateway
            .handle_bind_session_keys_with_audit(&room_service, req, None, None, Some(&sig_verifier))
            .await;
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.expect("error");
        assert_eq!(err.code, platform_core::ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn room_ready_with_invalid_evm_signature_is_rejected_via_gateway_verifier() {
        let gateway = RpcGateway::new();
        let chain_repo = Arc::new(InMemoryChainTxRepository::new());
        let room_service = AppRoomService::with_sinks(Arc::new(NoopRoomEventSink), chain_repo);
        let sig_verifier = AppRpcRequestSignatureVerifier::new(room_service.clone());
        let room_id = RoomId::new();
        let room = room_service.ensure_room(room_id).expect("room");
        room.join(0).await.expect("join seat0");

        let evm_key = EvmSigningKey::from_slice(&[25_u8; 32]).expect("evm key");
        let bind = gateway
            .handle_bind_address_with_audit(
                &room_service,
                signed_bind_address_request(room_id, 0, &evm_key),
                None,
                None,
                Some(&sig_verifier),
            )
            .await;
        assert!(bind.ok, "{bind:?}");
        room.bind_session_keys(
            0,
            "encpk".to_string(),
            "sigpk".to_string(),
            "x25519+evm".to_string(),
            "proof".to_string(),
        )
        .await
        .expect("bind keys");

        let mut req = signed_room_ready_request(room_id, 0, &evm_key);
        req.request_meta.signature = "0xdeadbeef".to_string();
        let resp = gateway
            .handle_room_ready_with_audit(&room_service, req, None, None, Some(&sig_verifier))
            .await;
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.expect("error");
        assert_eq!(err.code, platform_core::ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn game_act_with_valid_evm_signature_is_accepted_via_gateway_verifier() {
        let gateway = RpcGateway::new();
        let chain_repo = Arc::new(InMemoryChainTxRepository::new());
        let room_service = AppRoomService::with_sinks(Arc::new(NoopRoomEventSink), chain_repo);
        let sig_verifier = AppRpcRequestSignatureVerifier::new(room_service.clone());
        let room_id = RoomId::new();
        let room = room_service.ensure_room(room_id).expect("room");
        room.join(0).await.expect("join seat0");

        let evm_key = EvmSigningKey::from_slice(&[24_u8; 32]).expect("evm key");
        let bind = gateway
            .handle_bind_address_with_audit(
                &room_service,
                signed_bind_address_request(room_id, 0, &evm_key),
                None,
                None,
                Some(&sig_verifier),
            )
            .await;
        assert!(bind.ok, "{bind:?}");
        room.bind_session_keys(
            0,
            "encpk".to_string(),
            "sigpk".to_string(),
            "x25519+evm".to_string(),
            "proof".to_string(),
        )
        .await
        .expect("bind keys");
        room.ready(0).await.expect("ready");

        let before = room.get_state().await.expect("state").expect("snapshot");
        let resp = gateway
            .handle_game_act_with_audit(
                &room_service,
                signed_game_act_request(
                    room_id,
                    before.hand_id,
                    before.next_action_seq,
                    before.acting_seat_id.expect("acting seat"),
                    "check",
                    None,
                    &evm_key,
                ),
                None,
                None,
                Some(&sig_verifier),
            )
            .await;
        assert!(resp.ok, "{resp:?}");

        let after = room.get_state().await.expect("state").expect("snapshot");
        assert_eq!(after.next_action_seq, before.next_action_seq + 1);
    }

    #[tokio::test]
    async fn game_act_with_invalid_evm_signature_is_rejected_via_gateway_verifier() {
        let gateway = RpcGateway::new();
        let chain_repo = Arc::new(InMemoryChainTxRepository::new());
        let room_service = AppRoomService::with_sinks(Arc::new(NoopRoomEventSink), chain_repo);
        let sig_verifier = AppRpcRequestSignatureVerifier::new(room_service.clone());
        let room_id = RoomId::new();
        let room = room_service.ensure_room(room_id).expect("room");
        let room_actor = room.clone();
        room_actor.join(0).await.expect("join seat0");

        let evm_key = EvmSigningKey::from_slice(&[26_u8; 32]).expect("evm key");
        let bind = gateway
            .handle_bind_address_with_audit(
                &room_service,
                signed_bind_address_request(room_id, 0, &evm_key),
                None,
                None,
                Some(&sig_verifier),
            )
            .await;
        assert!(bind.ok, "{bind:?}");
        room_actor
            .bind_session_keys(
                0,
                "encpk".to_string(),
                "sigpk".to_string(),
                "x25519+evm".to_string(),
                "proof".to_string(),
            )
            .await
            .expect("bind keys");
        room_actor.ready(0).await.expect("ready");

        let before = room_actor.get_state().await.expect("state").expect("snapshot");
        let mut req = signed_game_act_request(
            room_id,
            before.hand_id,
            before.next_action_seq,
            before.acting_seat_id.expect("acting seat"),
            "check",
            None,
            &evm_key,
        );
        req.request_meta.signature = "0xdeadbeef".to_string();
        let resp = gateway
            .handle_game_act_with_audit(&room_service, req, None, None, Some(&sig_verifier))
            .await;
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.expect("error");
        assert_eq!(err.code, platform_core::ErrorCode::Forbidden);

        let after = room_actor.get_state().await.expect("state").expect("snapshot");
        assert_eq!(after.next_action_seq, before.next_action_seq);
    }

    #[tokio::test]
    async fn game_act_with_tx_hash_then_matched_callback_advances_state() {
        let gateway = RpcGateway::new();
        let chain_repo = Arc::new(InMemoryChainTxRepository::new());
        let audit_repo = Arc::new(InMemoryAuditRepository::default());
        let room_service =
            AppRoomService::with_sinks(Arc::new(NoopRoomEventSink), chain_repo.clone());
        let room_id = RoomId::new();
        let room = room_service.ensure_room(room_id).expect("room");

        for seat_id in [0, 1] {
            room.join(seat_id).await.expect("join");
            room.bind_address(seat_id, format!("cfx:seat{seat_id}"))
                .await
                .expect("bind");
            room.bind_session_keys(
                seat_id,
                format!("encpk{seat_id}"),
                format!("sigpk{seat_id}"),
                "x25519+evm".to_string(),
                format!("proof{seat_id}"),
            )
            .await
            .expect("keys");
        }
        room.ready(0).await.expect("ready");

        let before = room.get_state().await.expect("state").expect("snapshot");
        let tx_hash = "0xabc123".to_string();
        let act = gateway
            .handle_game_act(
                &room_service,
                GameActRequest {
                    room_id,
                    hand_id: before.hand_id,
                    action_seq: before.next_action_seq,
                    seat_id: before.acting_seat_id.expect("seat"),
                    action_type: "call".to_string(),
                    amount: Some("100000000000000".to_string()),
                    tx_hash: Some(tx_hash.clone()),
                    request_meta: signed_meta(),
                },
            )
            .await;
        assert!(act.ok, "{act:?}");

        let still_before = room.get_state().await.expect("state").expect("snapshot");
        assert_eq!(still_before.next_action_seq, before.next_action_seq);

        let sink = AppChainCallbackSink::new(room_service.clone(), chain_repo.clone())
            .with_audit_repo(audit_repo.clone());
        sink.on_verification_result(TxVerificationCallback {
            room_id,
            hand_id: Some(before.hand_id),
            seat_id: before.acting_seat_id,
            action_seq: Some(before.next_action_seq),
            tx_hash,
            status: VerificationStatus::Matched,
            confirmations: 3,
            failure_reason: None,
        })
        .await
        .expect("callback");

        let after = room.get_state().await.expect("state").expect("snapshot");
        assert_eq!(after.next_action_seq, before.next_action_seq + 1);
        assert_eq!(chain_repo.tx_verifications_len(), 1);
        assert_eq!(audit_repo.behavior_events.lock().expect("lock").len(), 1);
    }

    #[tokio::test]
    #[ignore = "requires local anvil binary and free TCP ports"]
    async fn rpc_and_chain_path_end_to_end_with_real_jsonrpc_and_anvil() {
        let anvil_port = 38545_u16;
        let anvil_endpoint = format!("http://127.0.0.1:{anvil_port}");
        let mut anvil = Command::new("anvil")
            .arg("-p")
            .arg(anvil_port.to_string())
            .arg("--silent")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn anvil");
        tokio::time::sleep(Duration::from_millis(600)).await;

        let gateway = RpcGateway::new();
        let chain_repo = Arc::new(InMemoryChainTxRepository::new());
        let audit_repo = Arc::new(InMemoryAuditRepository::default());
        let room_service =
            AppRoomService::with_sinks(Arc::new(NoopRoomEventSink), chain_repo.clone());
        let room_id = RoomId::new();
        let room = room_service.ensure_room(room_id).expect("room");
        room.join(0).await.expect("join");

        let event_bus = Arc::new(InMemoryEventBus::new());
        let sig_verifier = Arc::new(AppRpcRequestSignatureVerifier::new(room_service.clone()));
        let module = gateway
            .build_rpc_module_with_native_pubsub_and_limits_and_audit(
                Arc::new(room_service.clone()),
                event_bus,
                rpc_gateway::RpcGatewayLimits::default(),
                None,
                None,
                Some(sig_verifier),
            )
            .expect("rpc module");
        let server = ServerBuilder::default()
            .build("127.0.0.1:0")
            .await;
        let server = match server {
            Ok(server) => server,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                let _ = anvil.kill();
                let _ = anvil.wait();
                eprintln!("skip e2e test in sandbox: cannot bind local rpc port ({err})");
                return;
            }
            Err(err) => panic!("build server: {err}"),
        };
        let rpc_addr = server.local_addr().expect("local addr");
        let handle = server.start(module);
        let rpc_endpoint = format!("http://{rpc_addr}");

        let accounts = anvil_accounts(&anvil_endpoint).await;
        assert!(accounts.len() >= 3);
        let seat0_addr = accounts[0].clone();
        let seat1_addr = accounts[1].clone();
        let room_chain_addr = accounts[2].clone();

        let wallet = JsonRpcEvmWalletAdapter::new(anvil_endpoint.clone());
        let seat0_cfg = SingleActionRunnerConfig {
            rpc_endpoint: rpc_endpoint.clone(),
            room_id,
            seat_id: 0,
            seat_address: seat0_addr.clone(),
            room_chain_address: room_chain_addr.clone(),
            chain_id: 31_337,
            tx_value: poker_domain::Chips(1),
            card_encrypt_pubkey_hex: "11".repeat(32),
            card_encrypt_secret_hex: None,
        };
        let seat1_cfg = SingleActionRunnerConfig {
            rpc_endpoint: rpc_endpoint.clone(),
            room_id,
            seat_id: 1,
            seat_address: seat1_addr.clone(),
            room_chain_address: room_chain_addr.clone(),
            chain_id: 31_337,
            tx_value: poker_domain::Chips(1),
            card_encrypt_pubkey_hex: "22".repeat(32),
            card_encrypt_secret_hex: None,
        };

        prepare_seat(seat0_cfg.clone(), &wallet)
            .await
            .expect("prepare seat0");
        prepare_seat(seat1_cfg.clone(), &wallet)
            .await
            .expect("prepare seat1");

        let watcher = EvmChainWatcher::new(
            ReqwestEvmJsonRpcClient::new(anvil_endpoint.clone()),
            RetryPolicy {
                max_attempts: 3,
                base_backoff_ms: 50,
                max_backoff_ms: 100,
            },
        );
        let sink = AppChainCallbackSink::new(room_service.clone(), chain_repo.clone())
            .with_audit_repo(audit_repo.clone());
        let mut acted_seats = HashSet::new();
        let mut action_count = 0_u32;
        let mut verification_count = 0_u32;
        let mut last_hand_id = None;
        let mut last_action_seq = None;

        for _ in 0..16 {
            let state_before: platform_core::ResponseEnvelope<Option<poker_domain::HandSnapshot>> =
                serde_json::from_value(http_rpc_call(
                    &rpc_endpoint,
                    "game.get_state",
                    serde_json::to_value(GameGetStateRequest {
                        room_id,
                        hand_id: None,
                        seat_id: Some(0),
                    })
                    .expect("state req json"),
                )
                .await)
                .expect("decode state_before");
            assert!(state_before.ok, "{state_before:?}");
            let before = state_before.data.flatten().expect("snapshot before");
            if before.status != HandStatus::Running {
                break;
            }
            let Some(acting) = before.acting_seat_id else {
                break;
            };
            let (acted_seat, outcome) = run_first_available_action(
                &[
                    SeatTurnRunnerInput {
                        cfg: &seat0_cfg,
                    },
                    SeatTurnRunnerInput {
                        cfg: &seat1_cfg,
                    },
                ],
                &wallet,
            )
            .await
            .expect("run first available action")
            .expect("some seat should be able to act");
            assert_eq!(acted_seat, acting, "runtime should act for current acting seat");
            let seat_addr = match acted_seat {
                0 => seat0_addr.clone(),
                1 => seat1_addr.clone(),
                other => panic!("unexpected acted seat {other}"),
            };

            if let Some(tx_hash) = outcome.tx_hash.clone() {
                let verify_result = watcher
                    .verify_and_dispatch(
                        &TxVerificationInput {
                            room_id,
                            hand_id: Some(outcome.hand_id),
                            seat_id: Some(acted_seat),
                            action_seq: Some(outcome.action_seq),
                            tx_hash,
                            expected_to: room_chain_addr.clone(),
                            expected_from: Some(seat_addr),
                            expected_amount: None,
                            min_confirmations: 1,
                        },
                        &sink,
                    )
                    .await
                    .expect("verify+dispatch");
                assert_eq!(verify_result.status, VerificationStatus::Matched);
                verification_count = verification_count.saturating_add(1);
            }

            acted_seats.insert(acted_seat);
            action_count += 1;
            last_hand_id = Some(outcome.hand_id);
            last_action_seq = Some(outcome.action_seq);
            if action_count >= 4 && acted_seats.len() >= 2 {
                break;
            }
        }

        assert!(action_count >= 1, "expected at least one action");

        let state_after: platform_core::ResponseEnvelope<Option<poker_domain::HandSnapshot>> =
            serde_json::from_value(http_rpc_call(
                &rpc_endpoint,
                "game.get_state",
                serde_json::to_value(GameGetStateRequest {
                    room_id,
                    hand_id: last_hand_id,
                    seat_id: Some(0),
                })
                .expect("state req json"),
            )
            .await)
            .expect("decode state_after");
        assert!(state_after.ok, "{state_after:?}");
        let after = state_after.data.flatten().expect("snapshot after");
        assert!(after.next_action_seq >= last_action_seq.expect("last action seq"));
        assert!(chain_repo.tx_verifications_len() >= verification_count as usize);
        assert!(
            audit_repo.behavior_events.lock().expect("lock").len() >= verification_count as usize
        );

        handle.stop().expect("stop rpc server");
        let _ = anvil.kill();
        let _ = anvil.wait();
    }
}
