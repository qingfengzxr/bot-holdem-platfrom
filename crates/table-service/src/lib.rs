use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use poker_domain::{
    ActionType, AuditBehaviorEventKind, HandEvent, HandEventKind, HandId, HandSnapshot, HandStatus,
    LegalAction, PlayerAction, RoomId, SeatId, TraceId,
};
use poker_engine::{EngineActionResult, EngineState, PokerEngine};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum RoomCommand {
    Join {
        seat_id: SeatId,
    },
    Leave {
        seat_id: SeatId,
    },
    BindAddress {
        seat_id: SeatId,
        seat_address: String,
    },
    BindSessionKeys {
        seat_id: SeatId,
        card_encrypt_pubkey: String,
        request_verify_pubkey: String,
        key_algo: String,
        proof_signature: String,
    },
    Ready {
        seat_id: SeatId,
    },
    GetState {
        reply: oneshot::Sender<Option<HandSnapshot>>,
    },
    GetEngineState {
        reply: oneshot::Sender<Option<EngineState>>,
    },
    GetLegalActions {
        seat_id: SeatId,
        reply: oneshot::Sender<Vec<LegalAction>>,
    },
    Act {
        action: PlayerAction,
        reply: oneshot::Sender<Result<EngineActionResult, String>>,
    },
    QueuePendingChainAction {
        tx_hash: String,
        action: PlayerAction,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ChainTxVerified {
        callback: ChainTxVerifiedCallback,
    },
    CompleteSettlement {
        hand_id: HandId,
    },
    TurnTimeoutElapsed {
        expected_generation: u64,
    },
}

#[derive(Debug, Clone)]
pub struct RoomHandle {
    sender: mpsc::Sender<RoomCommand>,
}

impl RoomHandle {
    #[must_use]
    pub fn new(sender: mpsc::Sender<RoomCommand>) -> Self {
        Self { sender }
    }

    pub fn sender(&self) -> mpsc::Sender<RoomCommand> {
        self.sender.clone()
    }

    pub async fn join(&self, seat_id: SeatId) -> Result<(), String> {
        self.sender
            .send(RoomCommand::Join { seat_id })
            .await
            .map_err(|_| "room actor unavailable".to_string())
    }

    pub async fn leave(&self, seat_id: SeatId) -> Result<(), String> {
        self.sender
            .send(RoomCommand::Leave { seat_id })
            .await
            .map_err(|_| "room actor unavailable".to_string())
    }

    pub async fn bind_address(&self, seat_id: SeatId, seat_address: String) -> Result<(), String> {
        self.sender
            .send(RoomCommand::BindAddress {
                seat_id,
                seat_address,
            })
            .await
            .map_err(|_| "room actor unavailable".to_string())
    }

    pub async fn ready(&self, seat_id: SeatId) -> Result<(), String> {
        self.sender
            .send(RoomCommand::Ready { seat_id })
            .await
            .map_err(|_| "room actor unavailable".to_string())
    }

    pub async fn bind_session_keys(
        &self,
        seat_id: SeatId,
        card_encrypt_pubkey: String,
        request_verify_pubkey: String,
        key_algo: String,
        proof_signature: String,
    ) -> Result<(), String> {
        self.sender
            .send(RoomCommand::BindSessionKeys {
                seat_id,
                card_encrypt_pubkey,
                request_verify_pubkey,
                key_algo,
                proof_signature,
            })
            .await
            .map_err(|_| "room actor unavailable".to_string())
    }

    pub async fn get_state(&self) -> Result<Option<HandSnapshot>, String> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RoomCommand::GetState { reply: tx })
            .await
            .map_err(|_| "room actor unavailable".to_string())?;
        rx.await.map_err(|_| "room actor dropped reply".to_string())
    }

    pub async fn get_engine_state(&self) -> Result<Option<EngineState>, String> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RoomCommand::GetEngineState { reply: tx })
            .await
            .map_err(|_| "room actor unavailable".to_string())?;
        rx.await.map_err(|_| "room actor dropped reply".to_string())
    }

    pub async fn get_legal_actions(&self, seat_id: SeatId) -> Result<Vec<LegalAction>, String> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RoomCommand::GetLegalActions { seat_id, reply: tx })
            .await
            .map_err(|_| "room actor unavailable".to_string())?;
        rx.await.map_err(|_| "room actor dropped reply".to_string())
    }

    pub async fn act(&self, action: PlayerAction) -> Result<EngineActionResult, String> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RoomCommand::Act { action, reply: tx })
            .await
            .map_err(|_| "room actor unavailable".to_string())?;
        rx.await
            .map_err(|_| "room actor dropped reply".to_string())?
    }

    pub async fn chain_tx_verified(&self, callback: ChainTxVerifiedCallback) -> Result<(), String> {
        self.sender
            .send(RoomCommand::ChainTxVerified { callback })
            .await
            .map_err(|_| "room actor unavailable".to_string())
    }

    pub async fn complete_settlement(&self, hand_id: HandId) -> Result<(), String> {
        self.sender
            .send(RoomCommand::CompleteSettlement { hand_id })
            .await
            .map_err(|_| "room actor unavailable".to_string())
    }

    pub async fn queue_pending_chain_action(
        &self,
        tx_hash: String,
        action: PlayerAction,
    ) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RoomCommand::QueuePendingChainAction {
                tx_hash,
                action,
                reply: tx,
            })
            .await
            .map_err(|_| "room actor unavailable".to_string())?;
        rx.await
            .map_err(|_| "room actor dropped reply".to_string())?
    }
}

#[derive(Debug, Clone)]
pub struct ChainTxVerifiedCallback {
    pub tx_hash: String,
    pub status: String,
    pub confirmations: u64,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
    pub action_seq: Option<u32>,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingChainAction {
    action: PlayerAction,
}

#[derive(Debug, Clone)]
pub struct RoomBehaviorEvent {
    pub room_id: RoomId,
    pub occurred_at_unix_ms: i64,
    pub trace_id: TraceId,
    pub kind: AuditBehaviorEventKind,
}

pub trait RoomEventSink: Send + Sync {
    fn append_hand_events(&self, _events: &[HandEvent]) -> Result<(), String> {
        Ok(())
    }

    fn record_behavior_event(&self, _event: &RoomBehaviorEvent) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct NoopRoomEventSink;

impl RoomEventSink for NoopRoomEventSink {}

#[derive(Debug, Default)]
pub struct InMemoryRoomEventSink {
    hand_events: std::sync::Mutex<Vec<HandEvent>>,
    behavior_events: std::sync::Mutex<Vec<RoomBehaviorEvent>>,
}

impl InMemoryRoomEventSink {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn hand_events_len(&self) -> usize {
        self.hand_events.lock().map_or(0, |events| events.len())
    }

    pub fn behavior_events_len(&self) -> usize {
        self.behavior_events.lock().map_or(0, |events| events.len())
    }

    pub fn hand_events_snapshot(&self) -> Vec<HandEvent> {
        self.hand_events
            .lock()
            .map_or_else(|_| Vec::new(), |events| events.clone())
    }

    pub fn behavior_events_snapshot(&self) -> Vec<RoomBehaviorEvent> {
        self.behavior_events
            .lock()
            .map_or_else(|_| Vec::new(), |events| events.clone())
    }
}

impl RoomEventSink for InMemoryRoomEventSink {
    fn append_hand_events(&self, events: &[HandEvent]) -> Result<(), String> {
        let mut guard = self
            .hand_events
            .lock()
            .map_err(|_| "hand_events mutex poisoned".to_string())?;
        guard.extend_from_slice(events);
        Ok(())
    }

    fn record_behavior_event(&self, event: &RoomBehaviorEvent) -> Result<(), String> {
        let mut guard = self
            .behavior_events
            .lock()
            .map_err(|_| "behavior_events mutex poisoned".to_string())?;
        guard.push(event.clone());
        Ok(())
    }
}

#[derive(Default)]
pub struct RoomRegistry {
    rooms: HashMap<RoomId, RoomHandle>,
}

impl RoomRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, room_id: RoomId, handle: RoomHandle) {
        self.rooms.insert(room_id, handle);
    }

    pub fn get(&self, room_id: &RoomId) -> Option<RoomHandle> {
        self.rooms.get(room_id).cloned()
    }
}

pub fn spawn_room_actor(room_id: RoomId, queue_capacity: usize) -> RoomHandle {
    spawn_room_actor_with_sink(room_id, queue_capacity, Arc::new(NoopRoomEventSink))
}

pub fn spawn_room_actor_with_sink(
    room_id: RoomId,
    queue_capacity: usize,
    event_sink: Arc<dyn RoomEventSink>,
) -> RoomHandle {
    let (tx, mut rx) = mpsc::channel(queue_capacity);
    let handle_tx = tx.clone();

    tokio::spawn(async move {
        let actor_sender = tx.clone();
        let engine = PokerEngine::new();
        let mut state = EngineState::new(room_id, 1);
        let mut seated: HashSet<SeatId> = HashSet::new();
        let mut ready: HashSet<SeatId> = HashSet::new();
        let mut bound_addresses: HashMap<SeatId, String> = HashMap::new();
        let mut request_pubkeys: HashMap<SeatId, String> = HashMap::new();
        let mut card_pubkeys: HashMap<SeatId, String> = HashMap::new();
        let mut pending_chain_actions: HashMap<String, PendingChainAction> = HashMap::new();
        let mut timeout_generation: u64 = 0;
        let mut next_hand_event_seq: u32 = 1;

        while let Some(cmd) = rx.recv().await {
            match cmd {
                RoomCommand::Join { seat_id } => {
                    seated.insert(seat_id);
                    state.seat_player(seat_id);
                    ready.remove(&seat_id);
                    let _ = event_sink.record_behavior_event(&new_behavior_event(
                        room_id,
                        AuditBehaviorEventKind::SeatJoined { seat_id },
                    ));
                    debug!(?room_id, seat_id, "seat joined");
                }
                RoomCommand::Leave { seat_id } => {
                    seated.remove(&seat_id);
                    ready.remove(&seat_id);
                    bound_addresses.remove(&seat_id);
                    let _ = event_sink.record_behavior_event(&new_behavior_event(
                        room_id,
                        AuditBehaviorEventKind::SeatLeft { seat_id },
                    ));
                    debug!(?room_id, seat_id, "seat left");
                }
                RoomCommand::BindAddress {
                    seat_id,
                    seat_address,
                } => {
                    if seated.contains(&seat_id) {
                        bound_addresses.insert(seat_id, seat_address);
                        let _ = event_sink.record_behavior_event(&new_behavior_event(
                            room_id,
                            AuditBehaviorEventKind::SeatAddressBound { seat_id },
                        ));
                    }
                }
                RoomCommand::BindSessionKeys {
                    seat_id,
                    card_encrypt_pubkey,
                    request_verify_pubkey,
                    key_algo,
                    proof_signature,
                } => {
                    if seated.contains(&seat_id) {
                        card_pubkeys.insert(seat_id, card_encrypt_pubkey);
                        request_pubkeys.insert(seat_id, request_verify_pubkey);
                        let _ = event_sink.record_behavior_event(&new_behavior_event(
                            room_id,
                            AuditBehaviorEventKind::SeatSessionKeysBound {
                                seat_id,
                                key_algo: key_algo.clone(),
                            },
                        ));
                        debug!(
                            ?room_id,
                            seat_id,
                            key_algo,
                            proof_sig_len = proof_signature.len(),
                            "seat session keys bound"
                        );
                    }
                }
                RoomCommand::Ready { seat_id } => {
                    if seated.contains(&seat_id) {
                        ready.insert(seat_id);
                        let has_addr = bound_addresses.contains_key(&seat_id);
                        let has_card_key = card_pubkeys.contains_key(&seat_id);
                        let has_req_key = request_pubkeys.contains_key(&seat_id);
                        if state.snapshot.acting_seat_id.is_none()
                            && has_addr
                            && has_card_key
                            && has_req_key
                        {
                            state.start(seat_id);
                            let hand_events = vec![
                                new_hand_event(
                                    room_id,
                                    state.snapshot.hand_id,
                                    &mut next_hand_event_seq,
                                    HandEventKind::HandStarted,
                                ),
                                new_hand_event(
                                    room_id,
                                    state.snapshot.hand_id,
                                    &mut next_hand_event_seq,
                                    HandEventKind::TurnStarted { seat_id },
                                ),
                            ];
                            let _ = event_sink.append_hand_events(&hand_events);
                            timeout_generation = timeout_generation.saturating_add(1);
                            spawn_turn_timeout(actor_sender.clone(), timeout_generation);
                        }
                    }
                }
                RoomCommand::GetState { reply } => {
                    let _ = reply.send(Some(state.snapshot.clone()));
                }
                RoomCommand::GetEngineState { reply } => {
                    let _ = reply.send(Some(state.clone()));
                }
                RoomCommand::GetLegalActions { seat_id, reply } => {
                    let legal_actions = engine.legal_actions(&state, seat_id);
                    let _ = reply.send(legal_actions);
                }
                RoomCommand::Act { action, reply } => {
                    let submitted_action = action.clone();
                    let result = engine
                        .apply_action(&mut state, action)
                        .map_err(|err| err.to_string());
                    if let Ok(engine_result) = result.as_ref() {
                        emit_post_action_hand_events(
                            &*event_sink,
                            room_id,
                            &state,
                            submitted_action,
                            *engine_result,
                            &mut next_hand_event_seq,
                            &mut timeout_generation,
                            actor_sender.clone(),
                        );
                    }
                    let _ = reply.send(result);
                }
                RoomCommand::QueuePendingChainAction {
                    tx_hash,
                    action,
                    reply,
                } => {
                    let _ = pending_chain_actions
                        .insert(tx_hash.clone(), PendingChainAction { action });
                    let _ = event_sink.record_behavior_event(&new_behavior_event(
                        room_id,
                        AuditBehaviorEventKind::ChainTxVerificationCallback {
                            tx_hash,
                            status: "pending_action_queued".to_string(),
                            confirmations: 0,
                            seat_id: None,
                            action_seq: None,
                            failure_reason: None,
                        },
                    ));
                    let _ = reply.send(Ok(()));
                }
                RoomCommand::ChainTxVerified { callback } => {
                    if callback.status == "matched"
                        && callback.hand_id == Some(state.snapshot.hand_id)
                        && let Some(pending) = pending_chain_actions.remove(&callback.tx_hash)
                    {
                        let submitted_action = pending.action.clone();
                        let result = engine
                            .apply_action(&mut state, pending.action)
                            .map_err(|err| err.to_string());
                        if let Ok(engine_result) = result.as_ref() {
                            emit_post_action_hand_events(
                                &*event_sink,
                                room_id,
                                &state,
                                submitted_action,
                                *engine_result,
                                &mut next_hand_event_seq,
                                &mut timeout_generation,
                                actor_sender.clone(),
                            );
                        }
                    }

                    if matches!(callback.status.as_str(), "failed" | "unmatched")
                        && callback.hand_id == Some(state.snapshot.hand_id)
                        && matches!(
                            state.snapshot.status,
                            HandStatus::Running | HandStatus::Showdown | HandStatus::Settling
                        )
                    {
                        state.snapshot.status = HandStatus::Aborted;
                        state.snapshot.acting_seat_id = None;
                        state.hand.acting_seat_id = None;
                        state.betting_round.acting_seat_id = None;
                        let _ = event_sink.append_hand_events(&[new_hand_event(
                            room_id,
                            state.snapshot.hand_id,
                            &mut next_hand_event_seq,
                            HandEventKind::HandClosed,
                        )]);
                        timeout_generation = timeout_generation.saturating_add(1);
                    }

                    let _ = event_sink.record_behavior_event(&new_behavior_event(
                        room_id,
                        AuditBehaviorEventKind::ChainTxVerificationCallback {
                            tx_hash: callback.tx_hash.clone(),
                            status: callback.status.clone(),
                            confirmations: callback.confirmations,
                            seat_id: callback.seat_id,
                            action_seq: callback.action_seq,
                            failure_reason: callback.failure_reason.clone(),
                        },
                    ));
                    debug!(
                        ?room_id,
                        tx_hash = %callback.tx_hash,
                        status = %callback.status,
                        confirmations = callback.confirmations,
                        "chain tx verification callback received"
                    );
                }
                RoomCommand::CompleteSettlement { hand_id } => {
                    if hand_id == state.snapshot.hand_id
                        && matches!(
                            state.snapshot.status,
                            HandStatus::Showdown | HandStatus::Settling
                        )
                    {
                        state.snapshot.status = HandStatus::Settled;
                        state.snapshot.acting_seat_id = None;
                        state.hand.acting_seat_id = None;
                        state.betting_round.acting_seat_id = None;
                        let _ = event_sink.append_hand_events(&[new_hand_event(
                            room_id,
                            state.snapshot.hand_id,
                            &mut next_hand_event_seq,
                            HandEventKind::HandClosed,
                        )]);
                        timeout_generation = timeout_generation.saturating_add(1);
                    }
                }
                RoomCommand::TurnTimeoutElapsed {
                    expected_generation,
                } => {
                    if expected_generation != timeout_generation {
                        continue;
                    }

                    let Some(acting_seat_id) = state.snapshot.acting_seat_id else {
                        continue;
                    };

                    warn!(
                        ?room_id,
                        seat_id = acting_seat_id,
                        action_seq = state.snapshot.next_action_seq,
                        "turn timeout elapsed, applying auto-fold"
                    );

                    let auto_fold = PlayerAction {
                        room_id,
                        hand_id: state.snapshot.hand_id,
                        action_seq: state.snapshot.next_action_seq,
                        seat_id: acting_seat_id,
                        action_type: ActionType::Fold,
                        amount: None,
                    };
                    let submitted_action = auto_fold.clone();
                    let result = engine
                        .apply_action(&mut state, auto_fold)
                        .map_err(|err| err.to_string());
                    if let Ok(engine_result) = result.as_ref() {
                        emit_post_action_hand_events(
                            &*event_sink,
                            room_id,
                            &state,
                            submitted_action,
                            *engine_result,
                            &mut next_hand_event_seq,
                            &mut timeout_generation,
                            actor_sender.clone(),
                        );
                    } else {
                        timeout_generation = timeout_generation.saturating_add(1);
                    }
                    let _ = event_sink.record_behavior_event(&new_behavior_event(
                        room_id,
                        AuditBehaviorEventKind::TurnTimeoutAutoFold {
                            seat_id: acting_seat_id,
                            action_seq: state.snapshot.next_action_seq.saturating_sub(1),
                        },
                    ));
                }
            }
        }
    });

    RoomHandle::new(handle_tx)
}

fn new_hand_event(
    room_id: RoomId,
    hand_id: poker_domain::HandId,
    next_event_seq: &mut u32,
    kind: HandEventKind,
) -> HandEvent {
    let event = HandEvent {
        room_id,
        hand_id,
        event_seq: *next_event_seq,
        trace_id: TraceId::new(),
        occurred_at: Utc::now(),
        kind,
    };
    *next_event_seq = next_event_seq.saturating_add(1);
    event
}

fn new_behavior_event(room_id: RoomId, kind: AuditBehaviorEventKind) -> RoomBehaviorEvent {
    RoomBehaviorEvent {
        room_id,
        occurred_at_unix_ms: Utc::now().timestamp_millis(),
        trace_id: TraceId::new(),
        kind,
    }
}

fn emit_post_action_hand_events(
    event_sink: &dyn RoomEventSink,
    room_id: RoomId,
    state: &EngineState,
    submitted_action: PlayerAction,
    engine_result: EngineActionResult,
    next_hand_event_seq: &mut u32,
    timeout_generation: &mut u64,
    actor_sender: mpsc::Sender<RoomCommand>,
) {
    let mut hand_events = vec![
        new_hand_event(
            room_id,
            state.snapshot.hand_id,
            next_hand_event_seq,
            HandEventKind::ActionAccepted(submitted_action),
        ),
        new_hand_event(
            room_id,
            state.snapshot.hand_id,
            next_hand_event_seq,
            HandEventKind::PotUpdated {
                pot_total: state.snapshot.pot_total,
            },
        ),
    ];

    let EngineActionResult::Accepted { street_changed } = engine_result;
    if let Some(street) = street_changed {
        hand_events.push(new_hand_event(
            room_id,
            state.snapshot.hand_id,
            next_hand_event_seq,
            HandEventKind::StreetChanged { street },
        ));
    }

    if let Some(next_seat_id) = state.snapshot.acting_seat_id {
        hand_events.push(new_hand_event(
            room_id,
            state.snapshot.hand_id,
            next_hand_event_seq,
            HandEventKind::TurnStarted {
                seat_id: next_seat_id,
            },
        ));
        *timeout_generation = timeout_generation.saturating_add(1);
        spawn_turn_timeout(actor_sender, *timeout_generation);
    } else if matches!(
        state.snapshot.status,
        HandStatus::Settled | HandStatus::Aborted
    ) {
        hand_events.push(new_hand_event(
            room_id,
            state.snapshot.hand_id,
            next_hand_event_seq,
            HandEventKind::HandClosed,
        ));
        *timeout_generation = timeout_generation.saturating_add(1);
    }

    let _ = event_sink.append_hand_events(&hand_events);
}

fn spawn_turn_timeout(sender: mpsc::Sender<RoomCommand>, generation: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(DEFAULT_STEP_TIMEOUT).await;
        let _ = sender
            .send(RoomCommand::TurnTimeoutElapsed {
                expected_generation: generation,
            })
            .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn room_actor_records_chain_callback_behavior_event() {
        let room_id = RoomId::new();
        let sink = Arc::new(InMemoryRoomEventSink::new());
        let room = spawn_room_actor_with_sink(room_id, 16, sink.clone());

        room.join(0).await.expect("join");
        room.chain_tx_verified(ChainTxVerifiedCallback {
            tx_hash: "0xabc".to_string(),
            status: "matched".to_string(),
            confirmations: 5,
            hand_id: None,
            seat_id: Some(0),
            action_seq: Some(1),
            failure_reason: None,
        })
        .await
        .expect("callback");

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(sink.behavior_events_len() >= 2);
    }

    #[tokio::test]
    async fn room_ready_emits_hand_started_events_when_bindings_present() {
        let room_id = RoomId::new();
        let sink = Arc::new(InMemoryRoomEventSink::new());
        let room = spawn_room_actor_with_sink(room_id, 16, sink.clone());

        room.join(0).await.expect("join");
        room.bind_address(0, "cfx:seat".to_string())
            .await
            .expect("bind addr");
        room.bind_session_keys(
            0,
            "encpk".to_string(),
            "sigpk".to_string(),
            "x25519+ed25519".to_string(),
            "proof".to_string(),
        )
        .await
        .expect("bind keys");
        room.ready(0).await.expect("ready");

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(sink.hand_events_len() >= 2);
    }

    #[tokio::test]
    async fn failed_chain_callback_aborts_current_hand() {
        let room_id = RoomId::new();
        let sink = Arc::new(InMemoryRoomEventSink::new());
        let room = spawn_room_actor_with_sink(room_id, 16, sink.clone());

        room.join(0).await.expect("join");
        room.bind_address(0, "cfx:seat".to_string())
            .await
            .expect("bind addr");
        room.bind_session_keys(
            0,
            "encpk".to_string(),
            "sigpk".to_string(),
            "x25519+ed25519".to_string(),
            "proof".to_string(),
        )
        .await
        .expect("bind keys");
        room.ready(0).await.expect("ready");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let snapshot = room.get_state().await.expect("state").expect("snapshot");
        room.chain_tx_verified(ChainTxVerifiedCallback {
            tx_hash: "0xdead".to_string(),
            status: "failed".to_string(),
            confirmations: 1,
            hand_id: Some(snapshot.hand_id),
            seat_id: Some(0),
            action_seq: Some(snapshot.next_action_seq),
            failure_reason: Some("tx_failed".to_string()),
        })
        .await
        .expect("callback");

        tokio::time::sleep(Duration::from_millis(20)).await;
        let after = room.get_state().await.expect("state").expect("snapshot");
        assert_eq!(after.status, HandStatus::Aborted);
        assert!(sink.hand_events_len() >= 3);
    }

    #[tokio::test]
    async fn pending_chain_action_applies_only_after_matched_callback() {
        let room_id = RoomId::new();
        let sink = Arc::new(InMemoryRoomEventSink::new());
        let room = spawn_room_actor_with_sink(room_id, 32, sink.clone());

        room.join(0).await.expect("join");
        room.join(1).await.expect("join");
        room.bind_address(0, "cfx:seat0".to_string())
            .await
            .expect("bind addr");
        room.bind_session_keys(
            0,
            "encpk0".to_string(),
            "sigpk0".to_string(),
            "x25519+ed25519".to_string(),
            "proof0".to_string(),
        )
        .await
        .expect("bind keys");
        room.ready(0).await.expect("ready");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let before = room.get_state().await.expect("state").expect("snapshot");
        let pending_action = PlayerAction {
            room_id,
            hand_id: before.hand_id,
            action_seq: before.next_action_seq,
            seat_id: before.acting_seat_id.expect("acting seat"),
            action_type: ActionType::Check,
            amount: None,
        };

        room.queue_pending_chain_action("0xtx1".to_string(), pending_action.clone())
            .await
            .expect("queue pending");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let still_before = room.get_state().await.expect("state").expect("snapshot");
        assert_eq!(still_before.next_action_seq, before.next_action_seq);

        room.chain_tx_verified(ChainTxVerifiedCallback {
            tx_hash: "0xtx1".to_string(),
            status: "matched".to_string(),
            confirmations: 3,
            hand_id: Some(before.hand_id),
            seat_id: Some(pending_action.seat_id),
            action_seq: Some(pending_action.action_seq),
            failure_reason: None,
        })
        .await
        .expect("matched callback");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let after = room.get_state().await.expect("state").expect("snapshot");
        assert_eq!(after.next_action_seq, before.next_action_seq + 1);
        assert!(sink.hand_events_len() >= 4);
    }

    #[tokio::test]
    async fn two_checks_emit_street_changed_before_next_turn() {
        let room_id = RoomId::new();
        let sink = Arc::new(InMemoryRoomEventSink::new());
        let room = spawn_room_actor_with_sink(room_id, 32, sink.clone());

        for seat_id in [0, 1] {
            room.join(seat_id).await.expect("join");
            room.bind_address(seat_id, format!("cfx:seat{seat_id}"))
                .await
                .expect("bind addr");
            room.bind_session_keys(
                seat_id,
                format!("encpk{seat_id}"),
                format!("sigpk{seat_id}"),
                "x25519+ed25519".to_string(),
                format!("proof{seat_id}"),
            )
            .await
            .expect("bind keys");
        }
        room.ready(0).await.expect("ready");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let snapshot = room.get_state().await.expect("state").expect("snapshot");
        let hand_id = snapshot.hand_id;
        room.act(PlayerAction {
            room_id,
            hand_id,
            action_seq: snapshot.next_action_seq,
            seat_id: snapshot.acting_seat_id.expect("acting seat"),
            action_type: ActionType::Check,
            amount: None,
        })
        .await
        .expect("act 1");

        let snapshot = room.get_state().await.expect("state").expect("snapshot");
        room.act(PlayerAction {
            room_id,
            hand_id,
            action_seq: snapshot.next_action_seq,
            seat_id: snapshot.acting_seat_id.expect("acting seat"),
            action_type: ActionType::Check,
            amount: None,
        })
        .await
        .expect("act 2");

        tokio::time::sleep(Duration::from_millis(20)).await;
        let events = sink.hand_events_snapshot();
        let street_idx = events
            .iter()
            .position(|e| {
                matches!(
                    e.kind,
                    HandEventKind::StreetChanged {
                        street: poker_domain::Street::Flop
                    }
                )
            })
            .expect("street changed event");
        let turn_idx = events
            .iter()
            .position(|e| {
                matches!(e.kind, HandEventKind::TurnStarted { .. })
                    && e.event_seq > events[street_idx].event_seq
            })
            .expect("next turn after street change");
        assert!(street_idx < turn_idx);
    }

    #[tokio::test]
    async fn river_completion_emits_showdown_street_without_hand_closed() {
        let room_id = RoomId::new();
        let sink = Arc::new(InMemoryRoomEventSink::new());
        let room = spawn_room_actor_with_sink(room_id, 32, sink.clone());

        for seat_id in [0, 1] {
            room.join(seat_id).await.expect("join");
            room.bind_address(seat_id, format!("cfx:seat{seat_id}"))
                .await
                .expect("bind addr");
            room.bind_session_keys(
                seat_id,
                format!("encpk{seat_id}"),
                format!("sigpk{seat_id}"),
                "x25519+ed25519".to_string(),
                format!("proof{seat_id}"),
            )
            .await
            .expect("bind keys");
        }
        room.ready(0).await.expect("ready");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut snapshot = room.get_state().await.expect("state").expect("snapshot");
        // Walk to river using two checks per street.
        for _ in 0..8 {
            let action = PlayerAction {
                room_id,
                hand_id: snapshot.hand_id,
                action_seq: snapshot.next_action_seq,
                seat_id: snapshot.acting_seat_id.expect("acting seat"),
                action_type: ActionType::Check,
                amount: None,
            };
            room.act(action).await.expect("check");
            snapshot = room.get_state().await.expect("state").expect("snapshot");
            if snapshot.status == HandStatus::Showdown {
                break;
            }
        }

        assert_eq!(snapshot.status, HandStatus::Showdown);
        assert_eq!(snapshot.street, poker_domain::Street::Showdown);
        assert_eq!(snapshot.acting_seat_id, None);

        let events = sink.hand_events_snapshot();
        assert!(events.iter().any(|e| {
            matches!(
                e.kind,
                HandEventKind::StreetChanged {
                    street: poker_domain::Street::Showdown
                }
            )
        }));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.kind, HandEventKind::HandClosed)),
            "showdown transition should not close hand before settlement"
        );
    }

    #[tokio::test]
    async fn complete_settlement_closes_hand_after_showdown() {
        let room_id = RoomId::new();
        let sink = Arc::new(InMemoryRoomEventSink::new());
        let room = spawn_room_actor_with_sink(room_id, 32, sink.clone());

        for seat_id in [0, 1] {
            room.join(seat_id).await.expect("join");
            room.bind_address(seat_id, format!("cfx:seat{seat_id}"))
                .await
                .expect("bind addr");
            room.bind_session_keys(
                seat_id,
                format!("encpk{seat_id}"),
                format!("sigpk{seat_id}"),
                "x25519+ed25519".to_string(),
                format!("proof{seat_id}"),
            )
            .await
            .expect("bind keys");
        }
        room.ready(0).await.expect("ready");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut snapshot = room.get_state().await.expect("state").expect("snapshot");
        for _ in 0..8 {
            room.act(PlayerAction {
                room_id,
                hand_id: snapshot.hand_id,
                action_seq: snapshot.next_action_seq,
                seat_id: snapshot.acting_seat_id.expect("acting seat"),
                action_type: ActionType::Check,
                amount: None,
            })
            .await
            .expect("check");
            snapshot = room.get_state().await.expect("state").expect("snapshot");
            if snapshot.status == HandStatus::Showdown {
                break;
            }
        }
        assert_eq!(snapshot.status, HandStatus::Showdown);
        let hand_id = snapshot.hand_id;

        room.complete_settlement(hand_id)
            .await
            .expect("complete settlement");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let after = room.get_state().await.expect("state").expect("snapshot");
        assert_eq!(after.status, HandStatus::Settled);
        let events = sink.hand_events_snapshot();
        assert!(
            events
                .iter()
                .any(|e| matches!(e.kind, HandEventKind::HandClosed))
        );
    }

    #[tokio::test]
    async fn turn_timeout_auto_folds_and_settles_single_player_hand() {
        let room_id = RoomId::new();
        let sink = Arc::new(InMemoryRoomEventSink::new());
        let room = spawn_room_actor_with_sink(room_id, 16, sink.clone());

        room.join(0).await.expect("join");
        room.bind_address(0, "cfx:seat".to_string())
            .await
            .expect("bind addr");
        room.bind_session_keys(
            0,
            "encpk".to_string(),
            "sigpk".to_string(),
            "x25519+ed25519".to_string(),
            "proof".to_string(),
        )
        .await
        .expect("bind keys");
        room.ready(0).await.expect("ready");

        room.sender()
            .send(RoomCommand::TurnTimeoutElapsed {
                expected_generation: 1,
            })
            .await
            .expect("send timeout");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let snapshot = room.get_state().await.expect("state").expect("snapshot");
        assert_eq!(snapshot.status, HandStatus::Settled);
        assert_eq!(snapshot.acting_seat_id, None);
        assert_eq!(snapshot.next_action_seq, 2);
        assert!(sink.behavior_events_len() >= 3);

        let hand_events = sink.hand_events_snapshot();
        assert!(hand_events.iter().any(|e| matches!(
            e.kind,
            HandEventKind::ActionAccepted(PlayerAction {
                action_type: ActionType::Fold,
                ..
            })
        )));
        assert!(
            hand_events
                .iter()
                .any(|e| matches!(e.kind, HandEventKind::HandClosed))
        );

        let behavior_events = sink.behavior_events_snapshot();
        assert!(behavior_events.iter().any(|e| matches!(
            e.kind,
            AuditBehaviorEventKind::TurnTimeoutAutoFold {
                seat_id: 0,
                action_seq: 1
            }
        )));
    }

    #[tokio::test]
    async fn stale_turn_timeout_generation_is_ignored() {
        let room_id = RoomId::new();
        let sink = Arc::new(InMemoryRoomEventSink::new());
        let room = spawn_room_actor_with_sink(room_id, 16, sink.clone());

        room.join(0).await.expect("join");
        room.bind_address(0, "cfx:seat".to_string())
            .await
            .expect("bind addr");
        room.bind_session_keys(
            0,
            "encpk".to_string(),
            "sigpk".to_string(),
            "x25519+ed25519".to_string(),
            "proof".to_string(),
        )
        .await
        .expect("bind keys");
        room.ready(0).await.expect("ready");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let before = room.get_state().await.expect("state").expect("snapshot");
        assert_eq!(before.status, HandStatus::Running);
        assert_eq!(before.next_action_seq, 1);

        room.sender()
            .send(RoomCommand::TurnTimeoutElapsed {
                expected_generation: 0,
            })
            .await
            .expect("send stale timeout");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let after = room.get_state().await.expect("state").expect("snapshot");
        assert_eq!(after.status, HandStatus::Running);
        assert_eq!(after.next_action_seq, 1);
        assert_eq!(after.acting_seat_id, before.acting_seat_id);
    }

    #[tokio::test]
    async fn get_engine_state_returns_full_state_snapshot() {
        let room = spawn_room_actor(RoomId::new(), 16);
        room.join(0).await.expect("join");
        room.bind_address(0, "cfx:seat".to_string())
            .await
            .expect("bind addr");
        room.bind_session_keys(
            0,
            "encpk".to_string(),
            "sigpk".to_string(),
            "x25519+ed25519".to_string(),
            "proof".to_string(),
        )
        .await
        .expect("bind keys");
        room.ready(0).await.expect("ready");

        let state = room
            .get_engine_state()
            .await
            .expect("state")
            .expect("engine state");
        let snapshot = room.get_state().await.expect("state").expect("snapshot");
        assert_eq!(state.snapshot.hand_id, snapshot.hand_id);
        assert_eq!(state.snapshot.room_id, snapshot.room_id);
        assert_eq!(state.snapshot.status, snapshot.status);
        assert_eq!(state.pot.main_pot, snapshot.pot_total);
    }
}
