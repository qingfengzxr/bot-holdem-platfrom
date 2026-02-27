use std::{
    collections::HashMap,
    future::Future,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant},
};

use agent_auth::{
    ReplayNonceKey, ReplayNonceStore, SeatKeyBindingClaim, SignedRequestMeta, validate_request_window,
    verify_seat_key_binding_proof_evm,
};
use async_trait::async_trait;
use chrono::Utc;
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::{
    Extensions, RpcModule,
    core::server::SubscriptionMessage,
    server::{ServerBuilder, ServerHandle},
};
use platform_core::{ErrorCode, ResponseEnvelope};
use poker_domain::{
    ActionSeq, ActionType, AgentId, Chips, HandId, HandSnapshot, LegalAction, PlayerAction,
    RequestId, RoomId, SeatId, SessionId, TraceId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::info;

#[derive(Debug, Error)]
pub enum RpcGatewayError {
    #[error("method not implemented")]
    NotImplemented,
    #[error("room not found")]
    RoomNotFound,
    #[error("invalid action type")]
    InvalidActionType,
    #[error("invalid amount")]
    InvalidAmount,
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("forbidden")]
    Forbidden,
    #[error("invalid key binding proof")]
    InvalidKeyBindingProof,
    #[error("jsonrpc registration failed: {0}")]
    JsonRpcRegistration(String),
    #[error("too many concurrent requests")]
    TooManyConcurrentRequests,
    #[error("too many requests")]
    TooManyRequests,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameActRequest {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub action_seq: ActionSeq,
    pub seat_id: SeatId,
    pub action_type: String,
    pub amount: Option<String>,
    pub tx_hash: Option<String>,
    pub request_meta: SignedRequestMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameGetStateRequest {
    pub room_id: RoomId,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameGetLegalActionsRequest {
    pub room_id: RoomId,
    pub seat_id: SeatId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameGetPrivatePayloadsRequest {
    pub room_id: RoomId,
    pub seat_id: SeatId,
    pub hand_id: Option<HandId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivatePayloadEvent {
    pub room_id: RoomId,
    pub hand_id: HandId,
    pub seat_id: SeatId,
    pub event_name: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomBindAddressRequest {
    pub room_id: RoomId,
    pub seat_id: SeatId,
    pub seat_address: String,
    pub request_meta: SignedRequestMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomReadyRequest {
    pub room_id: RoomId,
    pub seat_id: SeatId,
    pub request_meta: SignedRequestMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomListRequest {
    pub include_inactive: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoomCreateRequest {
    pub room_id: Option<RoomId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomSummary {
    pub room_id: RoomId,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomBindSessionKeysRequest {
    pub room_id: RoomId,
    pub seat_id: SeatId,
    pub seat_address: String,
    pub card_encrypt_pubkey: String,
    pub request_verify_pubkey: String,
    pub key_algo: String,
    pub proof_signature: String,
    pub request_meta: SignedRequestMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequestEnvelope {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventTopic {
    PublicRoomEvents,
    SeatEvents,
    HandEvents,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeRequest {
    pub topic: EventTopic,
    pub room_id: RoomId,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsubscribeRequest {
    pub subscription_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub topic: EventTopic,
    pub room_id: RoomId,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
    pub event_name: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Agent,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodScope {
    Public,
    Agent,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodPolicy {
    pub scope: MethodScope,
    pub requires_signature: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct RequestAuthContext {
    pub role: Role,
    pub has_valid_signature: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RequestContext {
    pub trace_id: TraceId,
    pub request_id: Option<RequestId>,
    pub session_id: Option<SessionId>,
    pub agent_id: Option<AgentId>,
}

pub type RpcResult<T> = ResponseEnvelope<T>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestRejectReason {
    RequestExpiredOrInvalidWindow,
    MissingOrInvalidSignature,
    InvalidKeyBindingProof,
    ReplayRejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRejectAuditEvent {
    pub method: String,
    pub reason: RequestRejectReason,
    pub trace_id: TraceId,
    pub request_id: Option<RequestId>,
    pub request_nonce: Option<String>,
    pub room_id: Option<RoomId>,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
    pub action_seq: Option<ActionSeq>,
    pub detail: Option<String>,
}

#[async_trait]
pub trait RequestRejectAuditSink: Send + Sync {
    async fn on_request_rejected(&self, event: RequestRejectAuditEvent) -> Result<(), String>;
}

#[async_trait]
pub trait RequestSignatureVerifier: Send + Sync {
    async fn verify_bind_address(&self, _request: &RoomBindAddressRequest) -> Result<(), String> {
        Ok(())
    }

    async fn verify_bind_session_keys(
        &self,
        _request: &RoomBindSessionKeysRequest,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn verify_room_ready(&self, _request: &RoomReadyRequest) -> Result<(), String> {
        Ok(())
    }

    async fn verify_game_act(&self, _request: &GameActRequest) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RpcGatewayLimits {
    pub max_concurrent_per_connection: usize,
    pub max_requests_per_second_per_connection: usize,
}

impl Default for RpcGatewayLimits {
    fn default() -> Self {
        Self {
            max_concurrent_per_connection: 64,
            max_requests_per_second_per_connection: 256,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionConcurrencyLimiter {
    per_connection_limit: usize,
    counts: Arc<Mutex<HashMap<String, usize>>>,
}

#[derive(Debug, Clone)]
pub struct ConnectionRateLimiter {
    max_requests_per_window: usize,
    window: StdDuration,
    state: Arc<Mutex<HashMap<String, (Instant, usize)>>>,
}

impl ConnectionRateLimiter {
    #[must_use]
    pub fn new(max_requests_per_window: usize, window: StdDuration) -> Self {
        Self {
            max_requests_per_window: max_requests_per_window.max(1),
            window: if window.is_zero() {
                StdDuration::from_secs(1)
            } else {
                window
            },
            state: Arc::default(),
        }
    }

    pub fn check_and_record(
        &self,
        connection_id: impl Into<String>,
    ) -> Result<(), RpcGatewayError> {
        let connection_id = connection_id.into();
        let now = Instant::now();
        let mut guard = self
            .state
            .lock()
            .map_err(|_| RpcGatewayError::Upstream("rate limiter lock poisoned".to_string()))?;
        let entry = guard.entry(connection_id).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        if entry.1 >= self.max_requests_per_window {
            return Err(RpcGatewayError::TooManyRequests);
        }
        entry.1 += 1;
        Ok(())
    }

    pub fn count_for(&self, connection_id: &str) -> usize {
        self.state
            .lock()
            .ok()
            .and_then(|m| m.get(connection_id).map(|(_, c)| *c))
            .unwrap_or(0)
    }
}

impl ConnectionConcurrencyLimiter {
    #[must_use]
    pub fn new(per_connection_limit: usize) -> Self {
        Self {
            per_connection_limit: per_connection_limit.max(1),
            counts: Arc::default(),
        }
    }

    pub fn try_acquire(
        &self,
        connection_id: impl Into<String>,
    ) -> Result<ConnectionConcurrencyPermit, RpcGatewayError> {
        let connection_id = connection_id.into();
        let mut guard = self.counts.lock().map_err(|_| {
            RpcGatewayError::Upstream("connection limiter lock poisoned".to_string())
        })?;
        let count = guard.entry(connection_id.clone()).or_insert(0);
        if *count >= self.per_connection_limit {
            return Err(RpcGatewayError::TooManyConcurrentRequests);
        }
        *count += 1;
        Ok(ConnectionConcurrencyPermit {
            limiter: self.clone(),
            connection_id,
            released: false,
        })
    }

    pub fn active_for(&self, connection_id: &str) -> usize {
        self.counts
            .lock()
            .ok()
            .and_then(|m| m.get(connection_id).copied())
            .unwrap_or(0)
    }

    fn release(&self, connection_id: &str) {
        if let Ok(mut guard) = self.counts.lock()
            && let Some(count) = guard.get_mut(connection_id)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                let _ = guard.remove(connection_id);
            }
        }
    }
}

#[derive(Debug)]
pub struct ConnectionConcurrencyPermit {
    limiter: ConnectionConcurrencyLimiter,
    connection_id: String,
    released: bool,
}

impl ConnectionConcurrencyPermit {
    pub fn release(mut self) {
        if !self.released {
            self.limiter.release(&self.connection_id);
            self.released = true;
        }
    }
}

impl Drop for ConnectionConcurrencyPermit {
    fn drop(&mut self) {
        if !self.released {
            self.limiter.release(&self.connection_id);
            self.released = true;
        }
    }
}

#[derive(Clone)]
pub struct RpcContext<S, E> {
    pub room_service: Arc<S>,
    pub event_bus: Arc<E>,
    pub connection_limiter: Option<Arc<ConnectionConcurrencyLimiter>>,
    pub connection_rate_limiter: Option<Arc<ConnectionRateLimiter>>,
    pub request_reject_audit_sink: Option<Arc<dyn RequestRejectAuditSink>>,
    pub replay_nonce_store: Option<Arc<dyn ReplayNonceStore>>,
    pub request_signature_verifier: Option<Arc<dyn RequestSignatureVerifier>>,
}

#[async_trait]
pub trait RoomServicePort: Send + Sync {
    async fn create_room(
        &self,
        request: RoomCreateRequest,
    ) -> Result<RoomSummary, RpcGatewayError>;
    async fn list_rooms(
        &self,
        request: RoomListRequest,
    ) -> Result<Vec<RoomSummary>, RpcGatewayError>;
    async fn join(&self, request: RoomReadyRequest) -> Result<(), RpcGatewayError>;
    async fn ready(&self, request: RoomReadyRequest) -> Result<(), RpcGatewayError>;
    async fn bind_address(&self, request: RoomBindAddressRequest) -> Result<(), RpcGatewayError>;
    async fn bind_session_keys(
        &self,
        request: RoomBindSessionKeysRequest,
    ) -> Result<(), RpcGatewayError>;
    async fn get_state(
        &self,
        request: GameGetStateRequest,
    ) -> Result<Option<HandSnapshot>, RpcGatewayError>;
    async fn get_legal_actions(
        &self,
        room_id: RoomId,
        seat_id: SeatId,
    ) -> Result<Vec<LegalAction>, RpcGatewayError>;
    async fn get_private_payloads(
        &self,
        request: GameGetPrivatePayloadsRequest,
    ) -> Result<Vec<PrivatePayloadEvent>, RpcGatewayError>;
    async fn act(&self, request: GameActRequest) -> Result<(), RpcGatewayError>;
}

#[async_trait]
pub trait EventSubscriptionPort: Send + Sync {
    async fn subscribe(&self, request: SubscribeRequest) -> Result<String, RpcGatewayError>;
    async fn unsubscribe(&self, subscription_id: &str) -> Result<(), RpcGatewayError>;
    async fn publish(&self, event: EventEnvelope) -> Result<usize, RpcGatewayError>;
}

#[derive(Debug, Clone)]
pub struct InMemoryEventBus {
    subscriptions: Arc<Mutex<HashMap<String, SubscribeRequest>>>,
    live_subscribers: Arc<Mutex<HashMap<String, (SubscribeRequest, mpsc::Sender<EventEnvelope>)>>>,
    published_events: Arc<Mutex<Vec<EventEnvelope>>>,
    stream_buffer_capacity: usize,
    slow_subscriber_disconnects: Arc<Mutex<u64>>,
}

impl Default for InMemoryEventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryEventBus {
    #[must_use]
    pub fn new() -> Self {
        Self {
            subscriptions: Arc::default(),
            live_subscribers: Arc::default(),
            published_events: Arc::default(),
            stream_buffer_capacity: 64,
            slow_subscriber_disconnects: Arc::default(),
        }
    }

    #[must_use]
    pub fn with_stream_buffer_capacity(mut self, capacity: usize) -> Self {
        self.stream_buffer_capacity = capacity.max(1);
        self
    }

    pub fn subscription_count(&self) -> usize {
        self.subscriptions.lock().map_or(0, |m| m.len())
    }

    pub fn published_count(&self) -> usize {
        self.published_events.lock().map_or(0, |v| v.len())
    }

    pub fn live_subscription_count(&self) -> usize {
        self.live_subscribers.lock().map_or(0, |m| m.len())
    }

    pub fn slow_disconnect_count(&self) -> u64 {
        self.slow_subscriber_disconnects.lock().map_or(0, |v| *v)
    }

    pub fn subscribe_stream(
        &self,
        request: SubscribeRequest,
    ) -> Result<(String, mpsc::Receiver<EventEnvelope>), RpcGatewayError> {
        let (tx, rx) = mpsc::channel(self.stream_buffer_capacity);
        let subscription_id = RequestId::new().0.to_string();
        self.live_subscribers
            .lock()
            .map_err(|_| RpcGatewayError::Upstream("live_subscribers lock poisoned".to_string()))?
            .insert(subscription_id.clone(), (request, tx));
        Ok((subscription_id, rx))
    }

    pub fn unsubscribe_stream(&self, subscription_id: &str) -> Result<(), RpcGatewayError> {
        let mut guard = self
            .live_subscribers
            .lock()
            .map_err(|_| RpcGatewayError::Upstream("live_subscribers lock poisoned".to_string()))?;
        let _ = guard.remove(subscription_id);
        Ok(())
    }

    fn event_matches(sub: &SubscribeRequest, event: &EventEnvelope) -> bool {
        if sub.topic != event.topic || sub.room_id != event.room_id {
            return false;
        }
        if let Some(hand_id) = sub.hand_id
            && event.hand_id != Some(hand_id)
        {
            return false;
        }
        if let Some(seat_id) = sub.seat_id
            && event.seat_id != Some(seat_id)
        {
            return false;
        }
        true
    }
}

#[async_trait]
impl EventSubscriptionPort for InMemoryEventBus {
    async fn subscribe(&self, request: SubscribeRequest) -> Result<String, RpcGatewayError> {
        let mut guard = self
            .subscriptions
            .lock()
            .map_err(|_| RpcGatewayError::Upstream("subscriptions lock poisoned".to_string()))?;
        let subscription_id = RequestId::new().0.to_string();
        let _ = guard.insert(subscription_id.clone(), request);
        Ok(subscription_id)
    }

    async fn unsubscribe(&self, subscription_id: &str) -> Result<(), RpcGatewayError> {
        let mut guard = self
            .subscriptions
            .lock()
            .map_err(|_| RpcGatewayError::Upstream("subscriptions lock poisoned".to_string()))?;
        let _ = guard.remove(subscription_id);
        Ok(())
    }

    async fn publish(&self, event: EventEnvelope) -> Result<usize, RpcGatewayError> {
        let matched_control = {
            let guard = self.subscriptions.lock().map_err(|_| {
                RpcGatewayError::Upstream("subscriptions lock poisoned".to_string())
            })?;
            guard
                .values()
                .filter(|sub| Self::event_matches(sub, &event))
                .count()
        };
        let matched_stream = {
            let mut to_remove = Vec::new();
            let guard = self.live_subscribers.lock().map_err(|_| {
                RpcGatewayError::Upstream("live_subscribers lock poisoned".to_string())
            })?;
            let mut delivered = 0_usize;
            for (sub_id, (sub, tx)) in guard.iter() {
                if Self::event_matches(sub, &event) {
                    match tx.try_send(event.clone()) {
                        Ok(()) => {
                            delivered += 1;
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            to_remove.push(sub_id.clone());
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            to_remove.push(sub_id.clone());
                        }
                    }
                }
            }
            drop(guard);
            if !to_remove.is_empty() {
                let removed_count = to_remove.len();
                let mut guard = self.live_subscribers.lock().map_err(|_| {
                    RpcGatewayError::Upstream("live_subscribers lock poisoned".to_string())
                })?;
                for sub_id in to_remove {
                    let _ = guard.remove(&sub_id);
                }
                let mut slow = self.slow_subscriber_disconnects.lock().map_err(|_| {
                    RpcGatewayError::Upstream(
                        "slow_subscriber_disconnects lock poisoned".to_string(),
                    )
                })?;
                *slow += u64::try_from(removed_count).unwrap_or(0);
            }
            delivered
        };
        let mut pub_guard = self
            .published_events
            .lock()
            .map_err(|_| RpcGatewayError::Upstream("published_events lock poisoned".to_string()))?;
        pub_guard.push(event);
        Ok(matched_control + matched_stream)
    }
}

#[derive(Debug, Default)]
pub struct RpcGateway;

impl RpcGateway {
    #[must_use]
    async fn audit_request_rejection(
        &self,
        audit_sink: Option<&dyn RequestRejectAuditSink>,
        event: RequestRejectAuditEvent,
    ) {
        if let Some(sink) = audit_sink {
            let _ = sink.on_request_rejected(event).await;
        }
    }

    async fn validate_request_meta_with_audit(
        &self,
        method: &'static str,
        meta: &SignedRequestMeta,
        audit_sink: Option<&dyn RequestRejectAuditSink>,
        replay_nonce_store: Option<&dyn ReplayNonceStore>,
        room_id: Option<RoomId>,
        hand_id: Option<HandId>,
        seat_id: Option<SeatId>,
        action_seq: Option<ActionSeq>,
    ) -> Result<(), RpcGatewayError> {
        match validate_request_meta(meta) {
            Ok(()) => {
                if let Some(store) = replay_nonce_store {
                    if let Err(err) = store.consume_once(ReplayNonceKey {
                        // Temporary scope until authenticated agent_id is threaded into RPC context.
                        agent_id: AgentId(meta.request_id.0),
                        request_id: meta.request_id,
                        request_nonce: meta.request_nonce.clone(),
                    }) {
                        let _ = self
                            .audit_request_rejection(
                                audit_sink,
                                RequestRejectAuditEvent {
                                    method: method.to_string(),
                                    reason: RequestRejectReason::ReplayRejected,
                                    trace_id: TraceId::new(),
                                    request_id: Some(meta.request_id),
                                    request_nonce: Some(meta.request_nonce.clone()),
                                    room_id,
                                    hand_id,
                                    seat_id,
                                    action_seq,
                                    detail: Some(err.to_string()),
                                },
                            )
                            .await;
                        return Err(RpcGatewayError::Upstream(err.to_string()));
                    }
                }
                Ok(())
            }
            Err(err) => {
                let _ = self
                    .audit_request_rejection(
                        audit_sink,
                        RequestRejectAuditEvent {
                            method: method.to_string(),
                            reason: RequestRejectReason::RequestExpiredOrInvalidWindow,
                            trace_id: TraceId::new(),
                            request_id: Some(meta.request_id),
                            request_nonce: Some(meta.request_nonce.clone()),
                            room_id,
                            hand_id,
                            seat_id,
                            action_seq,
                            detail: Some(err.to_string()),
                        },
                    )
                    .await;
                Err(err)
            }
        }
    }

    async fn verify_game_act_signature_with_audit(
        &self,
        request: &GameActRequest,
        verifier: Option<&dyn RequestSignatureVerifier>,
        audit_sink: Option<&dyn RequestRejectAuditSink>,
    ) -> Result<(), RpcGatewayError> {
        let Some(verifier) = verifier else {
            return Ok(());
        };
        if let Err(err) = verifier.verify_game_act(request).await {
            let _ = self
                .audit_request_rejection(
                    audit_sink,
                    RequestRejectAuditEvent {
                        method: "game.act".to_string(),
                        reason: RequestRejectReason::MissingOrInvalidSignature,
                        trace_id: TraceId::new(),
                        request_id: Some(request.request_meta.request_id),
                        request_nonce: Some(request.request_meta.request_nonce.clone()),
                        room_id: Some(request.room_id),
                        hand_id: Some(request.hand_id),
                        seat_id: Some(request.seat_id),
                        action_seq: Some(request.action_seq),
                        detail: Some(err),
                    },
                )
                .await;
            return Err(RpcGatewayError::Forbidden);
        }
        Ok(())
    }

    async fn verify_bind_address_signature_with_audit(
        &self,
        request: &RoomBindAddressRequest,
        verifier: Option<&dyn RequestSignatureVerifier>,
        audit_sink: Option<&dyn RequestRejectAuditSink>,
    ) -> Result<(), RpcGatewayError> {
        let Some(verifier) = verifier else {
            return Ok(());
        };
        if let Err(err) = verifier.verify_bind_address(request).await {
            let _ = self
                .audit_request_rejection(
                    audit_sink,
                    RequestRejectAuditEvent {
                        method: "room.bind_address".to_string(),
                        reason: RequestRejectReason::MissingOrInvalidSignature,
                        trace_id: TraceId::new(),
                        request_id: Some(request.request_meta.request_id),
                        request_nonce: Some(request.request_meta.request_nonce.clone()),
                        room_id: Some(request.room_id),
                        hand_id: None,
                        seat_id: Some(request.seat_id),
                        action_seq: None,
                        detail: Some(err),
                    },
                )
                .await;
            return Err(RpcGatewayError::Forbidden);
        }
        Ok(())
    }

    async fn verify_room_ready_signature_with_audit(
        &self,
        request: &RoomReadyRequest,
        verifier: Option<&dyn RequestSignatureVerifier>,
        audit_sink: Option<&dyn RequestRejectAuditSink>,
    ) -> Result<(), RpcGatewayError> {
        let Some(verifier) = verifier else {
            return Ok(());
        };
        if let Err(err) = verifier.verify_room_ready(request).await {
            let _ = self
                .audit_request_rejection(
                    audit_sink,
                    RequestRejectAuditEvent {
                        method: "room.ready".to_string(),
                        reason: RequestRejectReason::MissingOrInvalidSignature,
                        trace_id: TraceId::new(),
                        request_id: Some(request.request_meta.request_id),
                        request_nonce: Some(request.request_meta.request_nonce.clone()),
                        room_id: Some(request.room_id),
                        hand_id: None,
                        seat_id: Some(request.seat_id),
                        action_seq: None,
                        detail: Some(err),
                    },
                )
                .await;
            return Err(RpcGatewayError::Forbidden);
        }
        Ok(())
    }

    async fn verify_bind_session_keys_signature_with_audit(
        &self,
        request: &RoomBindSessionKeysRequest,
        verifier: Option<&dyn RequestSignatureVerifier>,
        audit_sink: Option<&dyn RequestRejectAuditSink>,
    ) -> Result<(), RpcGatewayError> {
        let Some(verifier) = verifier else {
            return Ok(());
        };
        if let Err(err) = verifier.verify_bind_session_keys(request).await {
            let _ = self
                .audit_request_rejection(
                    audit_sink,
                    RequestRejectAuditEvent {
                        method: "room.bind_session_keys".to_string(),
                        reason: RequestRejectReason::MissingOrInvalidSignature,
                        trace_id: TraceId::new(),
                        request_id: Some(request.request_meta.request_id),
                        request_nonce: Some(request.request_meta.request_nonce.clone()),
                        room_id: Some(request.room_id),
                        hand_id: None,
                        seat_id: Some(request.seat_id),
                        action_seq: None,
                        detail: Some(err),
                    },
                )
                .await;
            return Err(RpcGatewayError::Forbidden);
        }
        Ok(())
    }

    pub async fn authorize_method_with_audit(
        &self,
        method: &str,
        ctx: RequestAuthContext,
        request_meta: Option<&SignedRequestMeta>,
        audit_sink: Option<&dyn RequestRejectAuditSink>,
    ) -> Result<(), RpcGatewayError> {
        match self.authorize_method(method, ctx) {
            Ok(()) => Ok(()),
            Err(err) => {
                if matches!(err, RpcGatewayError::Forbidden) {
                    let _ = self
                        .audit_request_rejection(
                            audit_sink,
                            RequestRejectAuditEvent {
                                method: method.to_string(),
                                reason: RequestRejectReason::MissingOrInvalidSignature,
                                trace_id: TraceId::new(),
                                request_id: request_meta.map(|m| m.request_id),
                                request_nonce: request_meta.map(|m| m.request_nonce.clone()),
                                room_id: None,
                                hand_id: None,
                                seat_id: None,
                                action_seq: None,
                                detail: Some("forbidden by method policy".to_string()),
                            },
                        )
                        .await;
                }
                Err(err)
            }
        }
    }

    pub fn new() -> Self {
        Self
    }

    pub fn describe_methods(&self) -> Vec<&'static str> {
        vec![
            "auth.register_agent",
            "auth.get_challenge",
            "room.create",
            "room.list",
            "room.join",
            "room.ready",
            "room.bind_address",
            "room.bind_session_keys",
            "game.get_state",
            "game.get_legal_actions",
            "game.get_private_payloads",
            "game.act",
            "game.get_hand_history",
            "game.get_ledger",
            "subscribe.events",
            "unsubscribe.events",
        ]
    }

    pub fn log_bootstrap(&self) {
        info!(
            methods = self.describe_methods().len(),
            "rpc gateway initialized"
        );
    }

    pub fn method_policy(&self, method: &str) -> MethodPolicy {
        match method {
            "room.create"
            | "room.list"
            | "game.get_state"
            | "game.get_legal_actions"
            | "game.get_private_payloads" => MethodPolicy {
                scope: MethodScope::Agent,
                requires_signature: false,
            },
            "room.bind_address"
            | "room.bind_session_keys"
            | "room.join"
            | "room.ready"
            | "game.act" => {
                MethodPolicy {
                    scope: MethodScope::Agent,
                    requires_signature: true,
                }
            }
            "admin.dump_state" => MethodPolicy {
                scope: MethodScope::Admin,
                requires_signature: true,
            },
            _ => MethodPolicy {
                scope: MethodScope::Public,
                requires_signature: false,
            },
        }
    }

    pub fn authorize_method(
        &self,
        method: &str,
        ctx: RequestAuthContext,
    ) -> Result<(), RpcGatewayError> {
        let policy = self.method_policy(method);

        let role_allowed = match policy.scope {
            MethodScope::Public => true,
            MethodScope::Agent => matches!(ctx.role, Role::Agent | Role::Admin),
            MethodScope::Admin => matches!(ctx.role, Role::Admin),
        };

        if !role_allowed {
            return Err(RpcGatewayError::Forbidden);
        }
        if policy.requires_signature && !ctx.has_valid_signature {
            return Err(RpcGatewayError::Forbidden);
        }

        Ok(())
    }

    pub async fn run_with_connection_limit<T, Fut>(
        &self,
        limiter: &ConnectionConcurrencyLimiter,
        connection_id: &str,
        fut: Fut,
    ) -> Result<T, RpcGatewayError>
    where
        Fut: Future<Output = Result<T, RpcGatewayError>>,
    {
        let _permit = limiter.try_acquire(connection_id.to_string())?;
        fut.await
    }

    fn connection_id_from_extensions(ext: &Extensions) -> String {
        ext.get::<jsonrpsee::core::server::ConnectionId>()
            .map(|id| id.0.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn try_acquire_ctx_connection_permit<S, E>(
        &self,
        ctx: &RpcContext<S, E>,
        ext: &Extensions,
    ) -> Result<Option<ConnectionConcurrencyPermit>, RpcGatewayError> {
        let connection_id = Self::connection_id_from_extensions(ext);
        if let Some(rate_limiter) = ctx.connection_rate_limiter.as_ref() {
            rate_limiter.check_and_record(connection_id.clone())?;
        }
        match ctx.connection_limiter.as_ref() {
            Some(limiter) => limiter.try_acquire(connection_id).map(Some),
            None => Ok(None),
        }
    }

    pub async fn handle_bind_address<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: RoomBindAddressRequest,
    ) -> RpcResult<()> {
        if let Err(err) = self
            .validate_request_meta_with_audit(
                "room.bind_address",
                &request.request_meta,
                None,
                None,
                Some(request.room_id),
                None,
                Some(request.seat_id),
                None,
            )
            .await
        {
            return RpcResult::err(ErrorCode::RequestInvalid, err.to_string());
        }
        match room_service.bind_address(request).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err(ErrorCode::RoomBindAddressFailed, err.to_string()),
        }
    }

    pub async fn handle_bind_address_with_audit<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: RoomBindAddressRequest,
        audit_sink: Option<&dyn RequestRejectAuditSink>,
        replay_nonce_store: Option<&dyn ReplayNonceStore>,
        signature_verifier: Option<&dyn RequestSignatureVerifier>,
    ) -> RpcResult<()> {
        if let Err(err) = self
            .validate_request_meta_with_audit(
                "room.bind_address",
                &request.request_meta,
                audit_sink,
                replay_nonce_store,
                Some(request.room_id),
                None,
                Some(request.seat_id),
                None,
            )
            .await
        {
            return RpcResult::err(ErrorCode::RequestInvalid, err.to_string());
        }
        if let Err(err) = self
            .verify_bind_address_signature_with_audit(&request, signature_verifier, audit_sink)
            .await
        {
            return RpcResult::err(ErrorCode::RequestInvalid, err.to_string());
        }
        match room_service.bind_address(request).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err(ErrorCode::RoomBindAddressFailed, err.to_string()),
        }
    }

    pub async fn handle_room_list<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: RoomListRequest,
    ) -> RpcResult<Vec<RoomSummary>> {
        match room_service.list_rooms(request).await {
            Ok(rooms) => RpcResult::ok(rooms),
            Err(err) => RpcResult::err(ErrorCode::RoomListFailed, err.to_string()),
        }
    }

    pub async fn handle_room_create<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: RoomCreateRequest,
    ) -> RpcResult<RoomSummary> {
        match room_service.create_room(request).await {
            Ok(room) => RpcResult::ok(room),
            Err(err) => RpcResult::err(ErrorCode::InternalError, err.to_string()),
        }
    }

    pub async fn handle_room_ready<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: RoomReadyRequest,
    ) -> RpcResult<()> {
        if let Err(err) = self
            .validate_request_meta_with_audit(
                "room.ready",
                &request.request_meta,
                None,
                None,
                Some(request.room_id),
                None,
                Some(request.seat_id),
                None,
            )
            .await
        {
            return RpcResult::err(ErrorCode::RequestInvalid, err.to_string());
        }
        match room_service.ready(request).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err(ErrorCode::RoomReadyFailed, err.to_string()),
        }
    }

    pub async fn handle_room_join<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: RoomReadyRequest,
    ) -> RpcResult<()> {
        if let Err(err) = self
            .validate_request_meta_with_audit(
                "room.join",
                &request.request_meta,
                None,
                None,
                Some(request.room_id),
                None,
                Some(request.seat_id),
                None,
            )
            .await
        {
            return RpcResult::err(ErrorCode::RequestInvalid, err.to_string());
        }
        match room_service.join(request).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err(ErrorCode::RoomReadyFailed, err.to_string()),
        }
    }

    pub async fn handle_room_join_with_audit<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: RoomReadyRequest,
        audit_sink: Option<&dyn RequestRejectAuditSink>,
        replay_nonce_store: Option<&dyn ReplayNonceStore>,
        signature_verifier: Option<&dyn RequestSignatureVerifier>,
    ) -> RpcResult<()> {
        if let Err(err) = self
            .validate_request_meta_with_audit(
                "room.join",
                &request.request_meta,
                audit_sink,
                replay_nonce_store,
                Some(request.room_id),
                None,
                Some(request.seat_id),
                None,
            )
            .await
        {
            return RpcResult::err(ErrorCode::RequestInvalid, err.to_string());
        }
        if let Err(err) = self
            .verify_room_ready_signature_with_audit(&request, signature_verifier, audit_sink)
            .await
        {
            return RpcResult::err(ErrorCode::Forbidden, err.to_string());
        }
        match room_service.join(request).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err(ErrorCode::RoomReadyFailed, err.to_string()),
        }
    }

    pub async fn handle_room_ready_with_audit<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: RoomReadyRequest,
        audit_sink: Option<&dyn RequestRejectAuditSink>,
        replay_nonce_store: Option<&dyn ReplayNonceStore>,
        signature_verifier: Option<&dyn RequestSignatureVerifier>,
    ) -> RpcResult<()> {
        if let Err(err) = self
            .validate_request_meta_with_audit(
                "room.ready",
                &request.request_meta,
                audit_sink,
                replay_nonce_store,
                Some(request.room_id),
                None,
                Some(request.seat_id),
                None,
            )
            .await
        {
            return RpcResult::err(ErrorCode::RequestInvalid, err.to_string());
        }
        if let Err(err) = self
            .verify_room_ready_signature_with_audit(&request, signature_verifier, audit_sink)
            .await
        {
            return RpcResult::err(ErrorCode::Forbidden, err.to_string());
        }
        match room_service.ready(request).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err(ErrorCode::RoomReadyFailed, err.to_string()),
        }
    }

    pub async fn handle_bind_session_keys<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: RoomBindSessionKeysRequest,
    ) -> RpcResult<()> {
        if let Err(err) = self
            .validate_request_meta_with_audit(
                "room.bind_session_keys",
                &request.request_meta,
                None,
                None,
                Some(request.room_id),
                None,
                Some(request.seat_id),
                None,
            )
            .await
        {
            return RpcResult::err(ErrorCode::RequestInvalid, err.to_string());
        }
        if let Err(err) = validate_bind_session_keys_proof(&request) {
            return RpcResult::err(ErrorCode::KeyBindingProofInvalid, err.to_string());
        }
        match room_service.bind_session_keys(request).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err(ErrorCode::RoomBindKeysFailed, err.to_string()),
        }
    }

    pub async fn handle_bind_session_keys_with_audit<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: RoomBindSessionKeysRequest,
        audit_sink: Option<&dyn RequestRejectAuditSink>,
        replay_nonce_store: Option<&dyn ReplayNonceStore>,
        signature_verifier: Option<&dyn RequestSignatureVerifier>,
    ) -> RpcResult<()> {
        if let Err(err) = self
            .validate_request_meta_with_audit(
                "room.bind_session_keys",
                &request.request_meta,
                audit_sink,
                replay_nonce_store,
                Some(request.room_id),
                None,
                Some(request.seat_id),
                None,
            )
            .await
        {
            return RpcResult::err(ErrorCode::RequestInvalid, err.to_string());
        }
        if let Err(err) = validate_bind_session_keys_proof(&request) {
            let _ = self
                .audit_request_rejection(
                    audit_sink,
                    RequestRejectAuditEvent {
                        method: "room.bind_session_keys".to_string(),
                        reason: RequestRejectReason::InvalidKeyBindingProof,
                        trace_id: TraceId::new(),
                        request_id: Some(request.request_meta.request_id),
                        request_nonce: Some(request.request_meta.request_nonce.clone()),
                        room_id: Some(request.room_id),
                        hand_id: None,
                        seat_id: Some(request.seat_id),
                        action_seq: None,
                        detail: Some(err.to_string()),
                    },
                )
                .await;
            return RpcResult::err(ErrorCode::KeyBindingProofInvalid, err.to_string());
        }
        if let Err(err) = self
            .verify_bind_session_keys_signature_with_audit(&request, signature_verifier, audit_sink)
            .await
        {
            return RpcResult::err(ErrorCode::Forbidden, err.to_string());
        }
        match room_service.bind_session_keys(request).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err(ErrorCode::RoomBindKeysFailed, err.to_string()),
        }
    }

    pub async fn handle_get_state<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: GameGetStateRequest,
    ) -> RpcResult<Option<HandSnapshot>> {
        match room_service.get_state(request).await {
            Ok(state) => RpcResult::ok(state),
            Err(err) => RpcResult::err(ErrorCode::GameGetStateFailed, err.to_string()),
        }
    }

    pub async fn handle_get_legal_actions<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: GameGetLegalActionsRequest,
    ) -> RpcResult<Vec<LegalAction>> {
        match room_service
            .get_legal_actions(request.room_id, request.seat_id)
            .await
        {
            Ok(actions) => RpcResult::ok(actions),
            Err(err) => RpcResult::err(ErrorCode::GameGetStateFailed, err.to_string()),
        }
    }

    pub async fn handle_get_private_payloads<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: GameGetPrivatePayloadsRequest,
    ) -> RpcResult<Vec<PrivatePayloadEvent>> {
        match room_service.get_private_payloads(request).await {
            Ok(payloads) => RpcResult::ok(payloads),
            Err(err) => RpcResult::err(ErrorCode::GameGetStateFailed, err.to_string()),
        }
    }

    pub async fn handle_game_act<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: GameActRequest,
    ) -> RpcResult<()> {
        if let Err(err) = validate_game_act_funding_fields(&request) {
            return RpcResult::err(ErrorCode::RequestInvalid, err.to_string());
        }
        if let Err(err) = self
            .validate_request_meta_with_audit(
                "game.act",
                &request.request_meta,
                None,
                None,
                Some(request.room_id),
                Some(request.hand_id),
                Some(request.seat_id),
                Some(request.action_seq),
            )
            .await
        {
            return RpcResult::err(ErrorCode::RequestInvalid, err.to_string());
        }
        match room_service.act(request).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err(ErrorCode::GameActFailed, err.to_string()),
        }
    }

    pub async fn handle_game_act_with_audit<S: RoomServicePort>(
        &self,
        room_service: &S,
        request: GameActRequest,
        audit_sink: Option<&dyn RequestRejectAuditSink>,
        replay_nonce_store: Option<&dyn ReplayNonceStore>,
        signature_verifier: Option<&dyn RequestSignatureVerifier>,
    ) -> RpcResult<()> {
        if let Err(err) = validate_game_act_funding_fields(&request) {
            return RpcResult::err(ErrorCode::RequestInvalid, err.to_string());
        }
        if let Err(err) = self
            .validate_request_meta_with_audit(
                "game.act",
                &request.request_meta,
                audit_sink,
                replay_nonce_store,
                Some(request.room_id),
                Some(request.hand_id),
                Some(request.seat_id),
                Some(request.action_seq),
            )
            .await
        {
            return RpcResult::err(ErrorCode::RequestInvalid, err.to_string());
        }
        if let Err(err) = self
            .verify_game_act_signature_with_audit(&request, signature_verifier, audit_sink)
            .await
        {
            return RpcResult::err(ErrorCode::Forbidden, err.to_string());
        }
        match room_service.act(request).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err(ErrorCode::GameActFailed, err.to_string()),
        }
    }

    pub async fn handle_subscribe_events<E: EventSubscriptionPort>(
        &self,
        event_bus: &E,
        request: SubscribeRequest,
    ) -> RpcResult<String> {
        match event_bus.subscribe(request).await {
            Ok(subscription_id) => RpcResult::ok(subscription_id),
            Err(err) => RpcResult::err(ErrorCode::SubscribeFailed, err.to_string()),
        }
    }

    pub async fn handle_unsubscribe_events<E: EventSubscriptionPort>(
        &self,
        event_bus: &E,
        request: UnsubscribeRequest,
    ) -> RpcResult<()> {
        match event_bus.unsubscribe(&request.subscription_id).await {
            Ok(()) => RpcResult::ok(()),
            Err(err) => RpcResult::err(ErrorCode::UnsubscribeFailed, err.to_string()),
        }
    }

    pub fn build_rpc_module<S>(
        &self,
        room_service: Arc<S>,
    ) -> Result<RpcModule<Arc<S>>, RpcGatewayError>
    where
        S: RoomServicePort + 'static,
    {
        let mut module = RpcModule::new(room_service);

        module
            .register_async_method("room.create", |params, ctx, _| async move {
                let req: RoomCreateRequest = match params.parse() {
                    Ok(v) => v,
                    Err(_) => RoomCreateRequest::default(),
                };
                let gateway = RpcGateway::new();
                Ok::<_, ErrorObjectOwned>(
                    gateway.handle_room_create(ctx.as_ref().as_ref(), req).await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("room.list", |params, ctx, _| async move {
                let req: RoomListRequest = match params.parse() {
                    Ok(v) => v,
                    Err(_) => RoomListRequest {
                        include_inactive: None,
                    },
                };
                let gateway = RpcGateway::new();
                Ok::<_, ErrorObjectOwned>(
                    gateway.handle_room_list(ctx.as_ref().as_ref(), req).await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("room.join", |params, ctx, _| async move {
                let req: RoomReadyRequest = params.parse()?;
                let gateway = RpcGateway::new();
                Ok::<_, ErrorObjectOwned>(
                    gateway.handle_room_join(ctx.as_ref().as_ref(), req).await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("room.ready", |params, ctx, _| async move {
                let req: RoomReadyRequest = params.parse()?;
                let gateway = RpcGateway::new();
                Ok::<_, ErrorObjectOwned>(
                    gateway.handle_room_ready(ctx.as_ref().as_ref(), req).await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("room.bind_address", |_params, ctx, _| async move {
                let req: RoomBindAddressRequest = _params.parse()?;
                let gateway = RpcGateway::new();
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_bind_address(ctx.as_ref().as_ref(), req)
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("room.bind_session_keys", |params, ctx, _| async move {
                let req: RoomBindSessionKeysRequest = params.parse()?;
                let gateway = RpcGateway::new();
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_bind_session_keys(ctx.as_ref().as_ref(), req)
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("game.get_state", |params, ctx, _| async move {
                let req: GameGetStateRequest = params.parse()?;
                let gateway = RpcGateway::new();
                Ok::<_, ErrorObjectOwned>(
                    gateway.handle_get_state(ctx.as_ref().as_ref(), req).await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("game.act", |params, ctx, _| async move {
                let req: GameActRequest = params.parse()?;
                let gateway = RpcGateway::new();
                Ok::<_, ErrorObjectOwned>(gateway.handle_game_act(ctx.as_ref().as_ref(), req).await)
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("game.get_private_payloads", |params, ctx, _| async move {
                let req: GameGetPrivatePayloadsRequest = params.parse()?;
                let gateway = RpcGateway::new();
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_get_private_payloads(ctx.as_ref().as_ref(), req)
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        Ok(module)
    }

    pub fn build_rpc_module_with_subscriptions<S, E>(
        &self,
        room_service: Arc<S>,
        event_bus: Arc<E>,
    ) -> Result<RpcModule<Arc<RpcContext<S, E>>>, RpcGatewayError>
    where
        S: RoomServicePort + 'static,
        E: EventSubscriptionPort + 'static,
    {
        self.build_rpc_module_with_subscriptions_and_limits(
            room_service,
            event_bus,
            RpcGatewayLimits::default(),
        )
    }

    pub fn build_rpc_module_with_subscriptions_and_limits<S, E>(
        &self,
        room_service: Arc<S>,
        event_bus: Arc<E>,
        limits: RpcGatewayLimits,
    ) -> Result<RpcModule<Arc<RpcContext<S, E>>>, RpcGatewayError>
    where
        S: RoomServicePort + 'static,
        E: EventSubscriptionPort + 'static,
    {
        self.build_rpc_module_with_subscriptions_and_limits_and_audit(
            room_service,
            event_bus,
            limits,
            None,
            None,
            None,
        )
    }

    pub fn build_rpc_module_with_subscriptions_and_limits_and_audit<S, E>(
        &self,
        room_service: Arc<S>,
        event_bus: Arc<E>,
        limits: RpcGatewayLimits,
        request_reject_audit_sink: Option<Arc<dyn RequestRejectAuditSink>>,
        replay_nonce_store: Option<Arc<dyn ReplayNonceStore>>,
        request_signature_verifier: Option<Arc<dyn RequestSignatureVerifier>>,
    ) -> Result<RpcModule<Arc<RpcContext<S, E>>>, RpcGatewayError>
    where
        S: RoomServicePort + 'static,
        E: EventSubscriptionPort + 'static,
    {
        let ctx = Arc::new(RpcContext {
            room_service,
            event_bus,
            connection_limiter: Some(Arc::new(ConnectionConcurrencyLimiter::new(
                limits.max_concurrent_per_connection,
            ))),
            connection_rate_limiter: Some(Arc::new(ConnectionRateLimiter::new(
                limits.max_requests_per_second_per_connection,
                StdDuration::from_secs(1),
            ))),
            request_reject_audit_sink,
            replay_nonce_store,
            request_signature_verifier,
        });
        let mut module = RpcModule::new(ctx);

        module
            .register_async_method("room.create", |params, ctx, ext| async move {
                let gateway = RpcGateway::new();
                let _permit = match gateway.try_acquire_ctx_connection_permit(ctx.as_ref(), &ext) {
                    Ok(v) => v,
                    Err(err) => {
                        return Ok::<_, ErrorObjectOwned>(RpcResult::err(
                            ErrorCode::Forbidden,
                            err.to_string(),
                        ));
                    }
                };
                let req: RoomCreateRequest = match params.parse() {
                    Ok(v) => v,
                    Err(_) => RoomCreateRequest::default(),
                };
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_room_create(ctx.as_ref().room_service.as_ref(), req)
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("room.list", |params, ctx, ext| async move {
                let gateway = RpcGateway::new();
                let _permit = match gateway.try_acquire_ctx_connection_permit(ctx.as_ref(), &ext) {
                    Ok(v) => v,
                    Err(err) => {
                        return Ok::<_, ErrorObjectOwned>(RpcResult::err(
                            ErrorCode::Forbidden,
                            err.to_string(),
                        ));
                    }
                };
                let req: RoomListRequest = match params.parse() {
                    Ok(v) => v,
                    Err(_) => RoomListRequest {
                        include_inactive: None,
                    },
                };
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_room_list(ctx.as_ref().room_service.as_ref(), req)
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("room.join", |params, ctx, ext| async move {
                let gateway = RpcGateway::new();
                let _permit = match gateway.try_acquire_ctx_connection_permit(ctx.as_ref(), &ext) {
                    Ok(v) => v,
                    Err(err) => {
                        return Ok::<_, ErrorObjectOwned>(RpcResult::err(
                            ErrorCode::Forbidden,
                            err.to_string(),
                        ));
                    }
                };
                let req: RoomReadyRequest = params.parse()?;
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_room_join_with_audit(
                            ctx.as_ref().room_service.as_ref(),
                            req,
                            ctx.as_ref().request_reject_audit_sink.as_deref(),
                            ctx.as_ref().replay_nonce_store.as_deref(),
                            ctx.as_ref().request_signature_verifier.as_deref(),
                        )
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("room.ready", |params, ctx, ext| async move {
                let gateway = RpcGateway::new();
                let _permit = match gateway.try_acquire_ctx_connection_permit(ctx.as_ref(), &ext) {
                    Ok(v) => v,
                    Err(err) => {
                        return Ok::<_, ErrorObjectOwned>(RpcResult::err(
                            ErrorCode::Forbidden,
                            err.to_string(),
                        ));
                    }
                };
                let req: RoomReadyRequest = params.parse()?;
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_room_ready_with_audit(
                            ctx.as_ref().room_service.as_ref(),
                            req,
                            ctx.as_ref().request_reject_audit_sink.as_deref(),
                            ctx.as_ref().replay_nonce_store.as_deref(),
                            ctx.as_ref().request_signature_verifier.as_deref(),
                        )
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("room.bind_address", |params, ctx, ext| async move {
                let gateway = RpcGateway::new();
                let _permit = match gateway.try_acquire_ctx_connection_permit(ctx.as_ref(), &ext) {
                    Ok(v) => v,
                    Err(err) => {
                        return Ok::<_, ErrorObjectOwned>(RpcResult::err(
                            ErrorCode::Forbidden,
                            err.to_string(),
                        ));
                    }
                };
                let req: RoomBindAddressRequest = params.parse()?;
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_bind_address_with_audit(
                            ctx.as_ref().room_service.as_ref(),
                            req,
                            ctx.as_ref().request_reject_audit_sink.as_deref(),
                            ctx.as_ref().replay_nonce_store.as_deref(),
                            ctx.as_ref().request_signature_verifier.as_deref(),
                        )
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("room.bind_session_keys", |params, ctx, ext| async move {
                let gateway = RpcGateway::new();
                let _permit = match gateway.try_acquire_ctx_connection_permit(ctx.as_ref(), &ext) {
                    Ok(v) => v,
                    Err(err) => {
                        return Ok::<_, ErrorObjectOwned>(RpcResult::err(
                            ErrorCode::Forbidden,
                            err.to_string(),
                        ));
                    }
                };
                let req: RoomBindSessionKeysRequest = params.parse()?;
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_bind_session_keys_with_audit(
                            ctx.as_ref().room_service.as_ref(),
                            req,
                            ctx.as_ref().request_reject_audit_sink.as_deref(),
                            ctx.as_ref().replay_nonce_store.as_deref(),
                            ctx.as_ref().request_signature_verifier.as_deref(),
                        )
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("game.get_state", |params, ctx, ext| async move {
                let gateway = RpcGateway::new();
                let _permit = match gateway.try_acquire_ctx_connection_permit(ctx.as_ref(), &ext) {
                    Ok(v) => v,
                    Err(err) => {
                        return Ok::<_, ErrorObjectOwned>(RpcResult::err(
                            ErrorCode::Forbidden,
                            err.to_string(),
                        ));
                    }
                };
                let req: GameGetStateRequest = params.parse()?;
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_get_state(ctx.as_ref().room_service.as_ref(), req)
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("game.get_legal_actions", |params, ctx, ext| async move {
                let gateway = RpcGateway::new();
                let _permit = match gateway.try_acquire_ctx_connection_permit(ctx.as_ref(), &ext) {
                    Ok(v) => v,
                    Err(err) => {
                        return Ok::<_, ErrorObjectOwned>(RpcResult::err(
                            ErrorCode::Forbidden,
                            err.to_string(),
                        ));
                    }
                };
                let req: GameGetLegalActionsRequest = params.parse()?;
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_get_legal_actions(ctx.as_ref().room_service.as_ref(), req)
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("game.act", |params, ctx, ext| async move {
                let gateway = RpcGateway::new();
                let _permit = match gateway.try_acquire_ctx_connection_permit(ctx.as_ref(), &ext) {
                    Ok(v) => v,
                    Err(err) => {
                        return Ok::<_, ErrorObjectOwned>(RpcResult::err(
                            ErrorCode::Forbidden,
                            err.to_string(),
                        ));
                    }
                };
                let req: GameActRequest = params.parse()?;
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_game_act_with_audit(
                            ctx.as_ref().room_service.as_ref(),
                            req,
                            ctx.as_ref().request_reject_audit_sink.as_deref(),
                            ctx.as_ref().replay_nonce_store.as_deref(),
                            ctx.as_ref().request_signature_verifier.as_deref(),
                        )
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("game.get_private_payloads", |params, ctx, ext| async move {
                let gateway = RpcGateway::new();
                let _permit = match gateway.try_acquire_ctx_connection_permit(ctx.as_ref(), &ext) {
                    Ok(v) => v,
                    Err(err) => {
                        return Ok::<_, ErrorObjectOwned>(RpcResult::err(
                            ErrorCode::Forbidden,
                            err.to_string(),
                        ));
                    }
                };
                let req: GameGetPrivatePayloadsRequest = params.parse()?;
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_get_private_payloads(ctx.as_ref().room_service.as_ref(), req)
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("subscribe.events", |params, ctx, ext| async move {
                let gateway = RpcGateway::new();
                let _permit = match gateway.try_acquire_ctx_connection_permit(ctx.as_ref(), &ext) {
                    Ok(v) => v,
                    Err(err) => {
                        return Ok::<_, ErrorObjectOwned>(RpcResult::err(
                            ErrorCode::Forbidden,
                            err.to_string(),
                        ));
                    }
                };
                let req: SubscribeRequest = params.parse()?;
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_subscribe_events(ctx.as_ref().event_bus.as_ref(), req)
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        module
            .register_async_method("unsubscribe.events", |params, ctx, ext| async move {
                let gateway = RpcGateway::new();
                let _permit = match gateway.try_acquire_ctx_connection_permit(ctx.as_ref(), &ext) {
                    Ok(v) => v,
                    Err(err) => {
                        return Ok::<_, ErrorObjectOwned>(RpcResult::err(
                            ErrorCode::Forbidden,
                            err.to_string(),
                        ));
                    }
                };
                let req: UnsubscribeRequest = params.parse()?;
                Ok::<_, ErrorObjectOwned>(
                    gateway
                        .handle_unsubscribe_events(ctx.as_ref().event_bus.as_ref(), req)
                        .await,
                )
            })
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;

        Ok(module)
    }

    pub fn build_rpc_module_with_native_pubsub<S>(
        &self,
        room_service: Arc<S>,
        event_bus: Arc<InMemoryEventBus>,
    ) -> Result<RpcModule<Arc<RpcContext<S, InMemoryEventBus>>>, RpcGatewayError>
    where
        S: RoomServicePort + 'static,
    {
        self.build_rpc_module_with_native_pubsub_and_limits(
            room_service,
            event_bus,
            RpcGatewayLimits::default(),
        )
    }

    pub fn build_rpc_module_with_native_pubsub_and_limits<S>(
        &self,
        room_service: Arc<S>,
        event_bus: Arc<InMemoryEventBus>,
        limits: RpcGatewayLimits,
    ) -> Result<RpcModule<Arc<RpcContext<S, InMemoryEventBus>>>, RpcGatewayError>
    where
        S: RoomServicePort + 'static,
    {
        self.build_rpc_module_with_native_pubsub_and_limits_and_audit(
            room_service,
            event_bus,
            limits,
            None,
            None,
            None,
        )
    }

    pub fn build_rpc_module_with_native_pubsub_and_limits_and_audit<S>(
        &self,
        room_service: Arc<S>,
        event_bus: Arc<InMemoryEventBus>,
        limits: RpcGatewayLimits,
        request_reject_audit_sink: Option<Arc<dyn RequestRejectAuditSink>>,
        replay_nonce_store: Option<Arc<dyn ReplayNonceStore>>,
        request_signature_verifier: Option<Arc<dyn RequestSignatureVerifier>>,
    ) -> Result<RpcModule<Arc<RpcContext<S, InMemoryEventBus>>>, RpcGatewayError>
    where
        S: RoomServicePort + 'static,
    {
        let mut module = self.build_rpc_module_with_subscriptions_and_limits_and_audit(
            room_service,
            event_bus.clone(),
            limits,
            request_reject_audit_sink,
            replay_nonce_store,
            request_signature_verifier,
        )?;
        module
            .register_subscription(
                "subscribe.events.native",
                "events",
                "unsubscribe.events.native",
                move |params, pending, ctx, _| async move {
                    let req: SubscribeRequest = match params.parse() {
                        Ok(v) => v,
                        Err(err) => {
                            pending.reject(ErrorObjectOwned::from(err)).await;
                            return Ok(());
                        }
                    };

                    let (local_sub_id, mut rx) =
                        match ctx.as_ref().event_bus.subscribe_stream(req) {
                            Ok(v) => v,
                            Err(err) => {
                                pending
                                    .reject(ErrorObjectOwned::owned(
                                        -32050,
                                        err.to_string(),
                                        None::<()>,
                                    ))
                                    .await;
                                return Ok(());
                            }
                        };

                    let sink = pending.accept().await?;
                    loop {
                        tokio::select! {
                            _ = sink.closed() => {
                                let _ = ctx.as_ref().event_bus.unsubscribe_stream(&local_sub_id);
                                break Ok(());
                            }
                            maybe_event = rx.recv() => {
                                match maybe_event {
                                    Some(event) => {
                                        let msg = SubscriptionMessage::from_json(&event)?;
                                        if sink.send(msg).await.is_err() {
                                            let _ = ctx.as_ref().event_bus.unsubscribe_stream(&local_sub_id);
                                            break Ok(());
                                        }
                                    }
                                    None => {
                                        let _ = ctx.as_ref().event_bus.unsubscribe_stream(&local_sub_id);
                                        break Ok(());
                                    }
                                }
                            }
                        }
                    }
                },
            )
            .map_err(|e| RpcGatewayError::JsonRpcRegistration(e.to_string()))?;
        Ok(module)
    }

    pub async fn start_server<S>(
        &self,
        bind_addr: SocketAddr,
        room_service: Arc<S>,
    ) -> Result<ServerHandle, RpcGatewayError>
    where
        S: RoomServicePort + 'static,
    {
        let server = ServerBuilder::default()
            .build(bind_addr)
            .await
            .map_err(|err| RpcGatewayError::Upstream(err.to_string()))?;
        let module = self.build_rpc_module(room_service)?;
        Ok(server.start(module))
    }
}

pub fn parse_action_type(action_type: &str) -> Result<ActionType, RpcGatewayError> {
    match action_type {
        "fold" => Ok(ActionType::Fold),
        "check" => Ok(ActionType::Check),
        "call" => Ok(ActionType::Call),
        "raise_to" => Ok(ActionType::RaiseTo),
        "all_in" => Ok(ActionType::AllIn),
        _ => Err(RpcGatewayError::InvalidActionType),
    }
}

pub fn parse_amount_u128(amount: Option<&str>) -> Result<Option<Chips>, RpcGatewayError> {
    amount
        .map(|raw| {
            raw.parse::<u128>()
                .map(Chips)
                .map_err(|_| RpcGatewayError::InvalidAmount)
        })
        .transpose()
}

pub fn to_player_action(request: &GameActRequest) -> Result<PlayerAction, RpcGatewayError> {
    Ok(PlayerAction {
        room_id: request.room_id,
        hand_id: request.hand_id,
        action_seq: request.action_seq,
        seat_id: request.seat_id,
        action_type: parse_action_type(&request.action_type)?,
        amount: parse_amount_u128(request.amount.as_deref())?,
    })
}

fn validate_game_act_funding_fields(request: &GameActRequest) -> Result<(), RpcGatewayError> {
    let action_type = parse_action_type(&request.action_type)?;
    let amount = parse_amount_u128(request.amount.as_deref())?;
    let tx_hash_present = request
        .tx_hash
        .as_ref()
        .is_some_and(|v| !v.trim().is_empty());

    match action_type {
        ActionType::Call | ActionType::RaiseTo | ActionType::AllIn => {
            if amount.is_none() {
                return Err(RpcGatewayError::Upstream(
                    "amount is required for monetary action".to_string(),
                ));
            }
            if !tx_hash_present {
                return Err(RpcGatewayError::Upstream(
                    "tx_hash is required for monetary action".to_string(),
                ));
            }
            Ok(())
        }
        ActionType::Fold | ActionType::Check => {
            if amount.is_some() || tx_hash_present {
                return Err(RpcGatewayError::Upstream(
                    "amount/tx_hash must be absent for non-monetary action".to_string(),
                ));
            }
            Ok(())
        }
    }
}

fn validate_request_meta(meta: &SignedRequestMeta) -> Result<(), RpcGatewayError> {
    validate_request_window(Utc::now(), meta.request_ts, meta.request_expiry_ms, 5_000)
        .map_err(|err| RpcGatewayError::Upstream(err.to_string()))
}

pub fn build_request_context(
    session_id: Option<SessionId>,
    agent_id: Option<AgentId>,
    request_meta: Option<&SignedRequestMeta>,
) -> RequestContext {
    RequestContext {
        trace_id: TraceId::new(),
        request_id: request_meta.map(|m| m.request_id),
        session_id,
        agent_id,
    }
}

fn validate_bind_session_keys_proof(
    request: &RoomBindSessionKeysRequest,
) -> Result<(), RpcGatewayError> {
    let claim = SeatKeyBindingClaim {
        agent_id: None,
        session_id: None,
        room_id: request.room_id,
        seat_id: request.seat_id,
        seat_address: request.seat_address.clone(),
        card_encrypt_pubkey: request.card_encrypt_pubkey.clone(),
        request_verify_pubkey: request.request_verify_pubkey.clone(),
        key_algo: request.key_algo.clone(),
        issued_at: request.request_meta.request_ts,
        expires_at: None,
    };

    verify_seat_key_binding_proof_evm(&claim, &request.proof_signature)
    .map_err(|_| RpcGatewayError::InvalidKeyBindingProof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_auth::{InMemoryReplayNonceStore, build_seat_key_binding_message, evm_address_from_secp256k1_verifying_key, sign_evm_personal_message_secp256k1};
    use chrono::Duration;
    use k256::ecdsa::SigningKey;
    use poker_domain::HandId;
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;

    #[derive(Default)]
    struct DummyRoomService;

    #[async_trait]
    impl RoomServicePort for DummyRoomService {
        async fn create_room(
            &self,
            request: RoomCreateRequest,
        ) -> Result<RoomSummary, RpcGatewayError> {
            Ok(RoomSummary {
                room_id: request.room_id.unwrap_or_else(RoomId::new),
                status: "active".to_string(),
            })
        }

        async fn list_rooms(
            &self,
            _request: RoomListRequest,
        ) -> Result<Vec<RoomSummary>, RpcGatewayError> {
            Ok(Vec::new())
        }

        async fn join(&self, _request: RoomReadyRequest) -> Result<(), RpcGatewayError> {
            Ok(())
        }

        async fn ready(&self, _request: RoomReadyRequest) -> Result<(), RpcGatewayError> {
            Ok(())
        }

        async fn bind_address(
            &self,
            _request: RoomBindAddressRequest,
        ) -> Result<(), RpcGatewayError> {
            Ok(())
        }

        async fn bind_session_keys(
            &self,
            _request: RoomBindSessionKeysRequest,
        ) -> Result<(), RpcGatewayError> {
            Ok(())
        }

        async fn get_state(
            &self,
            _request: GameGetStateRequest,
        ) -> Result<Option<HandSnapshot>, RpcGatewayError> {
            Ok(None)
        }

        async fn get_legal_actions(
            &self,
            _room_id: RoomId,
            _seat_id: SeatId,
        ) -> Result<Vec<LegalAction>, RpcGatewayError> {
            Ok(Vec::new())
        }

        async fn get_private_payloads(
            &self,
            _request: GameGetPrivatePayloadsRequest,
        ) -> Result<Vec<PrivatePayloadEvent>, RpcGatewayError> {
            Ok(Vec::new())
        }

        async fn act(&self, _request: GameActRequest) -> Result<(), RpcGatewayError> {
            Ok(())
        }
    }

    fn sample_meta() -> SignedRequestMeta {
        SignedRequestMeta {
            request_id: RequestId::new(),
            request_nonce: "nonce-1".to_string(),
            request_ts: Utc::now(),
            request_expiry_ms: 30_000,
            signature_pubkey_id: "pk1".to_string(),
            signature: "sig".to_string(),
        }
    }

    #[derive(Default)]
    struct InMemoryRejectAuditSink {
        events: TokioMutex<Vec<RequestRejectAuditEvent>>,
    }

    #[async_trait]
    impl RequestRejectAuditSink for InMemoryRejectAuditSink {
        async fn on_request_rejected(&self, event: RequestRejectAuditEvent) -> Result<(), String> {
            self.events.lock().await.push(event);
            Ok(())
        }
    }

    struct AlwaysFailGameActSignatureVerifier;

    #[async_trait]
    impl RequestSignatureVerifier for AlwaysFailGameActSignatureVerifier {
        async fn verify_bind_address(&self, _request: &RoomBindAddressRequest) -> Result<(), String> {
            Err("signature verification failed".to_string())
        }

        async fn verify_bind_session_keys(
            &self,
            _request: &RoomBindSessionKeysRequest,
        ) -> Result<(), String> {
            Err("signature verification failed".to_string())
        }

        async fn verify_game_act(&self, _request: &GameActRequest) -> Result<(), String> {
            Err("signature verification failed".to_string())
        }
    }

    #[test]
    fn parse_action_type_maps_expected_values() {
        assert_eq!(parse_action_type("fold").expect("fold"), ActionType::Fold);
        assert_eq!(
            parse_action_type("check").expect("check"),
            ActionType::Check
        );
        assert!(parse_action_type("raise").is_err());
    }

    #[test]
    fn parse_amount_parses_u128_string() {
        assert_eq!(
            parse_amount_u128(Some("42")).expect("amount"),
            Some(Chips(42))
        );
        assert!(parse_amount_u128(Some("not-a-number")).is_err());
    }

    #[test]
    fn validate_game_act_funding_fields_requires_amount_and_tx_for_monetary_actions() {
        let req = GameActRequest {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            action_seq: 1,
            seat_id: 0,
            action_type: "call".to_string(),
            amount: None,
            tx_hash: None,
            request_meta: sample_meta(),
        };
        assert!(validate_game_act_funding_fields(&req).is_err());

        let ok_req = GameActRequest {
            amount: Some("10".to_string()),
            tx_hash: Some("0xabc".to_string()),
            ..req
        };
        assert!(validate_game_act_funding_fields(&ok_req).is_ok());
    }

    #[test]
    fn validate_game_act_funding_fields_rejects_amount_or_tx_on_non_monetary_actions() {
        let req = GameActRequest {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            action_seq: 1,
            seat_id: 0,
            action_type: "check".to_string(),
            amount: Some("1".to_string()),
            tx_hash: None,
            request_meta: sample_meta(),
        };
        assert!(validate_game_act_funding_fields(&req).is_err());
    }

    #[test]
    fn to_player_action_translates_request() {
        let req = GameActRequest {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            action_seq: 7,
            seat_id: 2,
            action_type: "call".to_string(),
            amount: Some("100".to_string()),
            tx_hash: Some("0xabc".to_string()),
            request_meta: sample_meta(),
        };

        let action = to_player_action(&req).expect("convert");
        assert_eq!(action.action_type, ActionType::Call);
        assert_eq!(action.amount, Some(Chips(100)));
    }

    #[test]
    fn validate_request_meta_rejects_expired_requests() {
        let mut meta = sample_meta();
        meta.request_ts = Utc::now() - Duration::minutes(10);
        meta.request_expiry_ms = 1_000;
        assert!(validate_request_meta(&meta).is_err());
    }

    #[test]
    fn rpc_result_err_builds_failure_payload() {
        let result: RpcResult<()> = RpcResult::err(ErrorCode::InternalError, "boom");
        assert!(!result.ok);
        let error = result.error.expect("error");
        assert_eq!(error.code, ErrorCode::InternalError);
        assert_eq!(error.message, "boom");
    }

    #[test]
    fn method_policy_requires_signature_for_game_act() {
        let gw = RpcGateway::new();
        let policy = gw.method_policy("game.act");
        assert!(policy.requires_signature);
        assert_eq!(policy.scope, MethodScope::Agent);
    }

    #[test]
    fn authorize_method_rejects_missing_signature_when_required() {
        let gw = RpcGateway::new();
        let result = gw.authorize_method(
            "game.act",
            RequestAuthContext {
                role: Role::Agent,
                has_valid_signature: false,
            },
        );
        assert!(matches!(result, Err(RpcGatewayError::Forbidden)));
    }

    #[tokio::test]
    async fn authorize_method_with_audit_records_missing_signature_rejection() {
        let gw = RpcGateway::new();
        let sink = InMemoryRejectAuditSink::default();
        let err = gw
            .authorize_method_with_audit(
                "game.act",
                RequestAuthContext {
                    role: Role::Agent,
                    has_valid_signature: false,
                },
                Some(&sample_meta()),
                Some(&sink),
            )
            .await
            .expect_err("forbidden");
        assert!(matches!(err, RpcGatewayError::Forbidden));
        let events = sink.events.lock().await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].reason,
            RequestRejectReason::MissingOrInvalidSignature
        );
        assert_eq!(events[0].method, "game.act");
    }

    #[tokio::test]
    async fn handle_game_act_with_audit_records_expired_request_rejection() {
        let gw = RpcGateway::new();
        let sink = InMemoryRejectAuditSink::default();
        let mut meta = sample_meta();
        meta.request_ts = Utc::now() - Duration::minutes(10);
        meta.request_expiry_ms = 1_000;
        let req = GameActRequest {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            action_seq: 1,
            seat_id: 0,
            action_type: "check".to_string(),
            amount: None,
            tx_hash: None,
            request_meta: meta.clone(),
        };
        let resp = gw
            .handle_game_act_with_audit(&DummyRoomService, req, Some(&sink), None, None)
            .await;
        assert!(!resp.ok);
        let events = sink.events.lock().await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].reason,
            RequestRejectReason::RequestExpiredOrInvalidWindow
        );
        assert_eq!(events[0].request_id, Some(meta.request_id));
        assert_eq!(events[0].seat_id, Some(0));
        assert_eq!(events[0].action_seq, Some(1));
    }

    #[tokio::test]
    async fn handle_bind_session_keys_with_audit_records_invalid_proof_rejection() {
        let gw = RpcGateway::new();
        let sink = InMemoryRejectAuditSink::default();
        let req = RoomBindSessionKeysRequest {
            room_id: RoomId::new(),
            seat_id: 1,
            seat_address: "cfx:seat1".to_string(),
            card_encrypt_pubkey: "deadbeef".to_string(),
            request_verify_pubkey: "deadbeef".to_string(),
            key_algo: "x25519+evm".to_string(),
            proof_signature: "not-a-valid-signature".to_string(),
            request_meta: sample_meta(),
        };
        let resp = gw
            .handle_bind_session_keys_with_audit(&DummyRoomService, req, Some(&sink), None, None)
            .await;
        assert!(!resp.ok);
        let events = sink.events.lock().await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].reason,
            RequestRejectReason::InvalidKeyBindingProof
        );
        assert_eq!(events[0].method, "room.bind_session_keys");
        assert_eq!(events[0].seat_id, Some(1));
    }

    #[tokio::test]
    async fn handle_bind_session_keys_with_audit_accepts_valid_evm_proof() {
        let gw = RpcGateway::new();
        let sink = InMemoryRejectAuditSink::default();
        let request_meta = sample_meta();
        let signing_key = SigningKey::from_slice(&[11_u8; 32]).expect("signing key");
        let room_id = RoomId::new();
        let seat_id = 1;
        let seat_address = evm_address_from_secp256k1_verifying_key(signing_key.verifying_key());
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
            &signing_key,
            build_seat_key_binding_message(&claim).as_bytes(),
        )
        .expect("evm proof sig");
        let req = RoomBindSessionKeysRequest {
            room_id,
            seat_id,
            seat_address,
            card_encrypt_pubkey,
            request_verify_pubkey,
            key_algo,
            proof_signature,
            request_meta,
        };

        let resp = gw
            .handle_bind_session_keys_with_audit(&DummyRoomService, req, Some(&sink), None, None)
            .await;
        assert!(resp.ok, "{resp:?}");
        let events = sink.events.lock().await;
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn handle_game_act_with_audit_records_replay_rejection() {
        let gw = RpcGateway::new();
        let sink = InMemoryRejectAuditSink::default();
        let replay_store = InMemoryReplayNonceStore::new();
        let room_id = RoomId::new();
        let hand_id = HandId::new();
        let meta = sample_meta();
        let mk_req = || GameActRequest {
            room_id,
            hand_id,
            action_seq: 1,
            seat_id: 0,
            action_type: "check".to_string(),
            amount: None,
            tx_hash: None,
            request_meta: meta.clone(),
        };

        let first = gw
            .handle_game_act_with_audit(
                &DummyRoomService,
                mk_req(),
                Some(&sink),
                Some(&replay_store),
                None,
            )
            .await;
        assert!(first.ok, "{first:?}");

        let second = gw
            .handle_game_act_with_audit(
                &DummyRoomService,
                mk_req(),
                Some(&sink),
                Some(&replay_store),
                None,
            )
            .await;
        assert!(!second.ok);

        let events = sink.events.lock().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason, RequestRejectReason::ReplayRejected);
        assert_eq!(events[0].request_id, Some(meta.request_id));
        assert_eq!(events[0].method, "game.act");
    }

    #[tokio::test]
    async fn handle_game_act_with_audit_records_signature_verifier_rejection() {
        let gw = RpcGateway::new();
        let sink = InMemoryRejectAuditSink::default();
        let req = GameActRequest {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            action_seq: 1,
            seat_id: 0,
            action_type: "check".to_string(),
            amount: None,
            tx_hash: None,
            request_meta: sample_meta(),
        };
        let resp = gw
            .handle_game_act_with_audit(
                &DummyRoomService,
                req,
                Some(&sink),
                None,
                Some(&AlwaysFailGameActSignatureVerifier),
            )
            .await;
        assert!(!resp.ok);
        let events = sink.events.lock().await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].reason,
            RequestRejectReason::MissingOrInvalidSignature
        );
        assert_eq!(events[0].method, "game.act");
    }

    #[tokio::test]
    async fn handle_bind_address_with_audit_records_signature_verifier_rejection() {
        let gw = RpcGateway::new();
        let sink = InMemoryRejectAuditSink::default();
        let req = RoomBindAddressRequest {
            room_id: RoomId::new(),
            seat_id: 0,
            seat_address: "0xabc".to_string(),
            request_meta: sample_meta(),
        };
        let resp = gw
            .handle_bind_address_with_audit(
                &DummyRoomService,
                req,
                Some(&sink),
                None,
                Some(&AlwaysFailGameActSignatureVerifier),
            )
            .await;
        assert!(!resp.ok);
        let events = sink.events.lock().await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].reason,
            RequestRejectReason::MissingOrInvalidSignature
        );
        assert_eq!(events[0].method, "room.bind_address");
    }

    #[tokio::test]
    async fn handle_bind_session_keys_with_audit_records_signature_verifier_rejection() {
        let gw = RpcGateway::new();
        let sink = InMemoryRejectAuditSink::default();
        let request_meta = sample_meta();
        let signing_key = SigningKey::from_slice(&[5_u8; 32]).expect("signing key");
        let request_verify_pubkey = String::new();
        let room_id = RoomId::new();
        let seat_id = 1;
        let seat_address = evm_address_from_secp256k1_verifying_key(signing_key.verifying_key());
        let card_encrypt_pubkey = "11".repeat(32);
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
            &signing_key,
            build_seat_key_binding_message(&claim).as_bytes(),
        )
        .expect("evm proof sig");
        let req = RoomBindSessionKeysRequest {
            room_id,
            seat_id,
            seat_address,
            card_encrypt_pubkey,
            request_verify_pubkey,
            key_algo,
            proof_signature,
            request_meta,
        };
        let resp = gw
            .handle_bind_session_keys_with_audit(
                &DummyRoomService,
                req,
                Some(&sink),
                None,
                Some(&AlwaysFailGameActSignatureVerifier),
            )
            .await;
        assert!(!resp.ok);
        let events = sink.events.lock().await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].reason,
            RequestRejectReason::MissingOrInvalidSignature
        );
        assert_eq!(events[0].method, "room.bind_session_keys");
    }

    #[test]
    fn session_id_type_is_available_for_rpc_context() {
        let ctx = build_request_context(Some(SessionId::new()), Some(AgentId::new()), None);
        assert!(ctx.session_id.is_some());
        assert!(ctx.agent_id.is_some());
    }

    #[tokio::test]
    async fn in_memory_event_bus_subscribe_publish_unsubscribe() {
        let bus = InMemoryEventBus::new();
        let room_id = RoomId::new();
        let hand_id = HandId::new();

        let sub_id = bus
            .subscribe(SubscribeRequest {
                topic: EventTopic::HandEvents,
                room_id,
                hand_id: Some(hand_id),
                seat_id: None,
            })
            .await
            .expect("subscribe");

        let matched = bus
            .publish(EventEnvelope {
                topic: EventTopic::HandEvents,
                room_id,
                hand_id: Some(hand_id),
                seat_id: None,
                event_name: "turn_started".to_string(),
                payload: serde_json::json!({}),
            })
            .await
            .expect("publish");
        assert_eq!(matched, 1);
        assert_eq!(bus.subscription_count(), 1);
        assert_eq!(bus.published_count(), 1);

        bus.unsubscribe(&sub_id).await.expect("unsubscribe");
        assert_eq!(bus.subscription_count(), 0);
    }

    #[tokio::test]
    async fn gateway_handles_subscribe_and_unsubscribe() {
        let gw = RpcGateway::new();
        let bus = InMemoryEventBus::new();
        let room_id = RoomId::new();

        let subscribed = gw
            .handle_subscribe_events(
                &bus,
                SubscribeRequest {
                    topic: EventTopic::PublicRoomEvents,
                    room_id,
                    hand_id: None,
                    seat_id: None,
                },
            )
            .await;
        assert!(subscribed.ok);
        let sub_id = subscribed.data.expect("sub id");

        let unsubscribed = gw
            .handle_unsubscribe_events(
                &bus,
                UnsubscribeRequest {
                    subscription_id: sub_id,
                },
            )
            .await;
        assert!(unsubscribed.ok);
    }

    #[tokio::test]
    async fn in_memory_event_bus_stream_subscription_receives_matching_event() {
        let bus = InMemoryEventBus::new();
        let room_id = RoomId::new();
        let hand_id = HandId::new();
        let (sub_id, mut rx) = bus
            .subscribe_stream(SubscribeRequest {
                topic: EventTopic::HandEvents,
                room_id,
                hand_id: Some(hand_id),
                seat_id: None,
            })
            .expect("stream subscribe");

        let matched = bus
            .publish(EventEnvelope {
                topic: EventTopic::HandEvents,
                room_id,
                hand_id: Some(hand_id),
                seat_id: None,
                event_name: "hand_started".to_string(),
                payload: serde_json::json!({"seq": 1}),
            })
            .await
            .expect("publish");
        assert_eq!(matched, 1);

        let ev = rx.recv().await.expect("event");
        assert_eq!(ev.event_name, "hand_started");

        bus.unsubscribe_stream(&sub_id).expect("stream unsubscribe");
        assert_eq!(bus.live_subscription_count(), 0);
    }

    #[tokio::test]
    async fn in_memory_event_bus_disconnects_slow_stream_subscriber() {
        let bus = InMemoryEventBus::new().with_stream_buffer_capacity(1);
        let room_id = RoomId::new();
        let hand_id = HandId::new();
        let (_sub_id, _rx) = bus
            .subscribe_stream(SubscribeRequest {
                topic: EventTopic::HandEvents,
                room_id,
                hand_id: Some(hand_id),
                seat_id: None,
            })
            .expect("stream subscribe");

        let mk_event = |n| EventEnvelope {
            topic: EventTopic::HandEvents,
            room_id,
            hand_id: Some(hand_id),
            seat_id: None,
            event_name: format!("e{n}"),
            payload: serde_json::json!({"n": n}),
        };

        let matched1 = bus.publish(mk_event(1)).await.expect("publish1");
        assert_eq!(matched1, 1);
        let matched2 = bus.publish(mk_event(2)).await.expect("publish2");
        assert_eq!(matched2, 0);
        assert_eq!(bus.live_subscription_count(), 0);
        assert_eq!(bus.slow_disconnect_count(), 1);
    }

    #[test]
    fn build_rpc_module_with_subscriptions_registers_subscribe_methods() {
        let gw = RpcGateway::new();
        let module = gw
            .build_rpc_module_with_subscriptions(
                Arc::new(DummyRoomService),
                Arc::new(InMemoryEventBus::new()),
            )
            .expect("module");

        let methods: Vec<_> = module.method_names().collect();
        assert!(methods.contains(&"subscribe.events"));
        assert!(methods.contains(&"unsubscribe.events"));
    }

    #[test]
    fn build_rpc_module_with_native_pubsub_registers_native_subscription_methods() {
        let gw = RpcGateway::new();
        let module = gw
            .build_rpc_module_with_native_pubsub(
                Arc::new(DummyRoomService),
                Arc::new(InMemoryEventBus::new()),
            )
            .expect("module");

        let methods: Vec<_> = module.method_names().collect();
        assert!(methods.contains(&"subscribe.events.native"));
        assert!(methods.contains(&"unsubscribe.events.native"));
        assert!(methods.contains(&"subscribe.events"));
        assert!(methods.contains(&"unsubscribe.events"));
    }

    #[test]
    fn connection_concurrency_limiter_rejects_when_limit_reached() {
        let limiter = ConnectionConcurrencyLimiter::new(1);
        let permit = limiter.try_acquire("conn-1").expect("first acquire");
        assert_eq!(limiter.active_for("conn-1"), 1);

        let err = limiter
            .try_acquire("conn-1")
            .expect_err("should reject second acquire");
        assert!(matches!(err, RpcGatewayError::TooManyConcurrentRequests));

        drop(permit);
        assert_eq!(limiter.active_for("conn-1"), 0);
        let _permit2 = limiter
            .try_acquire("conn-1")
            .expect("acquire after release");
    }

    #[test]
    fn connection_rate_limiter_rejects_when_limit_reached() {
        let limiter = ConnectionRateLimiter::new(1, StdDuration::from_secs(60));
        limiter.check_and_record("conn-1").expect("first");
        assert_eq!(limiter.count_for("conn-1"), 1);
        let err = limiter
            .check_and_record("conn-1")
            .expect_err("second should be rejected");
        assert!(matches!(err, RpcGatewayError::TooManyRequests));
    }

    #[tokio::test]
    async fn run_with_connection_limit_wraps_handler_execution() {
        let gw = RpcGateway::new();
        let limiter = ConnectionConcurrencyLimiter::new(1);
        let result = gw
            .run_with_connection_limit(&limiter, "conn-1", async {
                Ok::<_, RpcGatewayError>(123_u32)
            })
            .await
            .expect("handler result");
        assert_eq!(result, 123);
        assert_eq!(limiter.active_for("conn-1"), 0);
    }

    #[tokio::test]
    async fn handle_room_create_returns_created_room() {
        let gw = RpcGateway::new();
        let room_id = RoomId::new();
        let resp = gw
            .handle_room_create(&DummyRoomService, RoomCreateRequest { room_id: Some(room_id) })
            .await;
        assert!(resp.ok, "{resp:?}");
        let data = resp.data.expect("room");
        assert_eq!(data.room_id, room_id);
        assert_eq!(data.status, "active");
    }
}
