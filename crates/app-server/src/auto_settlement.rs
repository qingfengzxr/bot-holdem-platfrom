use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use ledger_store::{SettlementPersistenceReadRepository, SettlementPersistenceRepository};
use poker_domain::{HandId, HandStatus, RoomId, SeatId, TraceId};
use poker_engine::{PokerEngine, ShowdownInput};
use rs_poker::core::{Card, FlatHand};
use settlement::{BatchSettlementWalletAdapter, RakePolicy, SettlementService, SettlementWalletAdapter};
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::room_service_port::AppRoomService;
use crate::settlement_orchestrator::{
    BatchPayoutSettlementInput, DirectPayoutSettlementInput, SettlementAlertSink,
    SettlementAuditSink, SettlementLifecyclePort, run_showdown_settlement_with_batch_payouts_and_alerts,
    run_showdown_settlement_with_direct_payouts_and_alerts,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoSettlementOutcome {
    NoActiveHand,
    NotShowdown,
    WaitingForShowdownInput,
    WaitingForSeatAddresses,
    Settled,
}

#[async_trait]
pub trait AutoSettlementRoomPort: Send + Sync {
    async fn get_engine_state(
        &self,
        room_id: RoomId,
    ) -> Result<Option<poker_engine::EngineState>, String>;

    async fn complete_settlement(&self, room_id: RoomId, hand_id: HandId) -> Result<(), String>;
}

#[async_trait]
pub trait ShowdownInputProvider: Send + Sync {
    async fn get_showdown_input(
        &self,
        room_id: RoomId,
        hand_id: HandId,
    ) -> Result<Option<ShowdownInput>, String>;

    async fn get_settlement_seat_addresses(
        &self,
        room_id: RoomId,
        hand_id: HandId,
    ) -> Result<HashMap<SeatId, String>, String>;
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryShowdownInputProvider {
    showdown_inputs: Arc<Mutex<HashMap<(RoomId, HandId), ShowdownInput>>>,
    seat_addresses: Arc<Mutex<HashMap<(RoomId, HandId), HashMap<SeatId, String>>>>,
}

impl InMemoryShowdownInputProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn upsert_showdown_input(
        &self,
        room_id: RoomId,
        hand_id: HandId,
        input: ShowdownInput,
    ) -> Result<(), String> {
        self.showdown_inputs
            .lock()
            .map_err(|_| "showdown_inputs lock poisoned".to_string())?
            .insert((room_id, hand_id), input);
        Ok(())
    }

    pub fn upsert_seat_addresses(
        &self,
        room_id: RoomId,
        hand_id: HandId,
        addresses: HashMap<SeatId, String>,
    ) -> Result<(), String> {
        self.seat_addresses
            .lock()
            .map_err(|_| "seat_addresses lock poisoned".to_string())?
            .insert((room_id, hand_id), addresses);
        Ok(())
    }
}

#[async_trait]
impl ShowdownInputProvider for InMemoryShowdownInputProvider {
    async fn get_showdown_input(
        &self,
        room_id: RoomId,
        hand_id: HandId,
    ) -> Result<Option<ShowdownInput>, String> {
        Ok(self
            .showdown_inputs
            .lock()
            .map_err(|_| "showdown_inputs lock poisoned".to_string())?
            .get(&(room_id, hand_id))
            .cloned())
    }

    async fn get_settlement_seat_addresses(
        &self,
        room_id: RoomId,
        hand_id: HandId,
    ) -> Result<HashMap<SeatId, String>, String> {
        Ok(self
            .seat_addresses
            .lock()
            .map_err(|_| "seat_addresses lock poisoned".to_string())?
            .get(&(room_id, hand_id))
            .cloned()
            .unwrap_or_default())
    }
}

#[derive(Clone)]
pub struct AppRoomShowdownInputProvider {
    room_service: AppRoomService,
    inner: InMemoryShowdownInputProvider,
    showdown_read_repo: Option<Arc<dyn SettlementPersistenceReadRepository>>,
    showdown_write_repo: Option<Arc<dyn SettlementPersistenceRepository>>,
}

impl AppRoomShowdownInputProvider {
    #[must_use]
    pub fn new(room_service: AppRoomService) -> Self {
        Self {
            room_service,
            inner: InMemoryShowdownInputProvider::new(),
            showdown_read_repo: None,
            showdown_write_repo: None,
        }
    }

    #[must_use]
    pub fn with_persistence(
        mut self,
        read_repo: Arc<dyn SettlementPersistenceReadRepository>,
        write_repo: Arc<dyn SettlementPersistenceRepository>,
    ) -> Self {
        self.showdown_read_repo = Some(read_repo);
        self.showdown_write_repo = Some(write_repo);
        self
    }

    pub async fn upsert_showdown_input(
        &self,
        room_id: RoomId,
        hand_id: HandId,
        input: ShowdownInput,
    ) -> Result<(), String> {
        self.inner
            .upsert_showdown_input(room_id, hand_id, input.clone())
            .await?;
        if let Some(repo) = self.showdown_write_repo.as_ref() {
            let record = showdown_input_to_record(room_id, hand_id, &input)?;
            repo.upsert_settlement_showdown_input(&record)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[async_trait]
impl ShowdownInputProvider for AppRoomShowdownInputProvider {
    async fn get_showdown_input(
        &self,
        room_id: RoomId,
        hand_id: HandId,
    ) -> Result<Option<ShowdownInput>, String> {
        if let Some(input) = self.inner.get_showdown_input(room_id, hand_id).await? {
            return Ok(Some(input));
        }
        let Some(repo) = self.showdown_read_repo.as_ref() else {
            return Ok(None);
        };
        let Some(record) = repo
            .get_settlement_showdown_input(hand_id)
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        if record.room_id != room_id {
            return Ok(None);
        }
        let input = showdown_input_from_record(&record)?;
        self.inner
            .upsert_showdown_input(room_id, hand_id, input.clone())
            .await
            .map_err(|e| format!("cache showdown input failed: {e}"))?;
        Ok(Some(input))
    }

    async fn get_settlement_seat_addresses(
        &self,
        room_id: RoomId,
        hand_id: HandId,
    ) -> Result<HashMap<SeatId, String>, String> {
        let mut addresses = self
            .inner
            .get_settlement_seat_addresses(room_id, hand_id)
            .await?;
        if addresses.is_empty() {
            addresses = self
                .room_service
                .bound_seat_addresses(room_id)
                .map_err(|e| e.to_string())?;
        }
        Ok(addresses)
    }
}

fn cards_to_compact_string(cards: &[Card]) -> String {
    cards.iter().map(ToString::to_string).collect::<Vec<_>>().join("")
}

fn showdown_input_to_record(
    room_id: RoomId,
    hand_id: HandId,
    input: &ShowdownInput,
) -> Result<ledger_store::SettlementShowdownInputRecord, String> {
    Ok(ledger_store::SettlementShowdownInputRecord {
        room_id,
        hand_id,
        board_cards: cards_to_compact_string(&input.board),
        revealed_hole_cards: input
            .revealed_hole_cards
            .iter()
            .map(|(seat_id, cards)| ledger_store::SettlementShowdownSeatCardsRecord {
                seat_id: *seat_id,
                cards: cards_to_compact_string(cards),
            })
            .collect(),
        source: "admin_ops_http".to_string(),
        updated_at: Utc::now(),
        trace_id: TraceId::new(),
    })
}

fn showdown_input_from_record(
    record: &ledger_store::SettlementShowdownInputRecord,
) -> Result<ShowdownInput, String> {
    let board = FlatHand::new_from_str(&record.board_cards)
        .map_err(|e| format!("invalid persisted board cards: {e}"))?
        .iter()
        .copied()
        .collect::<Vec<_>>();

    let mut revealed_hole_cards = Vec::with_capacity(record.revealed_hole_cards.len());
    for seat in &record.revealed_hole_cards {
        let cards = FlatHand::new_from_str(&seat.cards)
            .map_err(|e| format!("invalid persisted seat cards for seat {}: {e}", seat.seat_id))?;
        if cards.len() != 2 {
            return Err(format!(
                "invalid persisted seat cards length for seat {}",
                seat.seat_id
            ));
        }
        revealed_hole_cards.push((seat.seat_id, [cards[0], cards[1]]));
    }

    Ok(ShowdownInput {
        board,
        revealed_hole_cards,
    })
}

pub struct AutoSettlementLoopConfig {
    pub poll_interval: Duration,
    pub min_confirmations: u64,
    pub rake_policy: RakePolicy,
}

impl Default for AutoSettlementLoopConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
            min_confirmations: 1,
            rake_policy: RakePolicy::zero(),
        }
    }
}

#[async_trait]
impl AutoSettlementRoomPort for AppRoomService {
    async fn get_engine_state(
        &self,
        room_id: RoomId,
    ) -> Result<Option<poker_engine::EngineState>, String> {
        AppRoomService::get_engine_state(self, room_id)
            .await
            .map_err(|e| e.to_string())
    }

    async fn complete_settlement(&self, room_id: RoomId, hand_id: HandId) -> Result<(), String> {
        let room = self.ensure_room(room_id).map_err(|e| e.to_string())?;
        room.complete_settlement(hand_id).await
    }
}

pub async fn try_auto_settle_showdown_room<L, RP, SP, SR, W>(
    room_id: RoomId,
    engine: &PokerEngine,
    settlement_service: &SettlementService<L>,
    settlement_repo: &SR,
    wallet: &W,
    room_port: &RP,
    showdown_provider: &SP,
    rake_policy: RakePolicy,
    min_confirmations: u64,
    trace_id: TraceId,
    alert_sink: Option<&dyn SettlementAlertSink>,
    audit_sink: Option<&dyn SettlementAuditSink>,
) -> Result<AutoSettlementOutcome, String>
where
    L: ledger_store::LedgerRepository,
    RP: AutoSettlementRoomPort,
    SR: ledger_store::SettlementPersistenceRepository,
    W: SettlementWalletAdapter,
    SP: ShowdownInputProvider,
{
    let Some(state) = room_port.get_engine_state(room_id).await? else {
        return Ok(AutoSettlementOutcome::NoActiveHand);
    };
    if state.snapshot.status != HandStatus::Showdown {
        return Ok(AutoSettlementOutcome::NotShowdown);
    }

    let Some(showdown_input) = showdown_provider
        .get_showdown_input(room_id, state.snapshot.hand_id)
        .await?
    else {
        return Ok(AutoSettlementOutcome::WaitingForShowdownInput);
    };
    let seat_addresses = showdown_provider
        .get_settlement_seat_addresses(room_id, state.snapshot.hand_id)
        .await?;
    if seat_addresses.is_empty() {
        return Ok(AutoSettlementOutcome::WaitingForSeatAddresses);
    }

    struct RoomLifecycleAdapter<'a, RP> {
        inner: &'a RP,
        room_id: RoomId,
    }

    #[async_trait]
    impl<RP: AutoSettlementRoomPort> SettlementLifecyclePort for RoomLifecycleAdapter<'_, RP> {
        async fn complete_settlement(&self, hand_id: HandId) -> Result<(), String> {
            self.inner.complete_settlement(self.room_id, hand_id).await
        }
    }

    let lifecycle = RoomLifecycleAdapter {
        inner: room_port,
        room_id,
    };

    let _ = run_showdown_settlement_with_direct_payouts_and_alerts(
        engine,
        settlement_service,
        settlement_repo,
        wallet,
        &lifecycle,
        &state,
        &showdown_input,
        rake_policy,
        DirectPayoutSettlementInput {
            seat_addresses: &seat_addresses,
            min_confirmations,
        },
        trace_id,
        alert_sink,
        audit_sink,
    )
    .await?;

    Ok(AutoSettlementOutcome::Settled)
}

pub async fn try_auto_settle_showdown_room_with_batch<L, RP, SP, SR, W>(
    room_id: RoomId,
    engine: &PokerEngine,
    settlement_service: &SettlementService<L>,
    settlement_repo: &SR,
    wallet: &W,
    room_port: &RP,
    showdown_provider: &SP,
    rake_policy: RakePolicy,
    min_confirmations: u64,
    trace_id: TraceId,
    alert_sink: Option<&dyn SettlementAlertSink>,
    audit_sink: Option<&dyn SettlementAuditSink>,
) -> Result<AutoSettlementOutcome, String>
where
    L: ledger_store::LedgerRepository,
    RP: AutoSettlementRoomPort,
    SR: ledger_store::SettlementPersistenceRepository,
    W: BatchSettlementWalletAdapter,
    SP: ShowdownInputProvider,
{
    let Some(state) = room_port.get_engine_state(room_id).await? else {
        return Ok(AutoSettlementOutcome::NoActiveHand);
    };
    if state.snapshot.status != HandStatus::Showdown {
        return Ok(AutoSettlementOutcome::NotShowdown);
    }

    let Some(showdown_input) = showdown_provider
        .get_showdown_input(room_id, state.snapshot.hand_id)
        .await?
    else {
        return Ok(AutoSettlementOutcome::WaitingForShowdownInput);
    };
    let seat_addresses = showdown_provider
        .get_settlement_seat_addresses(room_id, state.snapshot.hand_id)
        .await?;
    if seat_addresses.is_empty() {
        return Ok(AutoSettlementOutcome::WaitingForSeatAddresses);
    }

    struct RoomLifecycleAdapter<'a, RP> {
        inner: &'a RP,
        room_id: RoomId,
    }

    #[async_trait]
    impl<RP: AutoSettlementRoomPort> SettlementLifecyclePort for RoomLifecycleAdapter<'_, RP> {
        async fn complete_settlement(&self, hand_id: HandId) -> Result<(), String> {
            self.inner.complete_settlement(self.room_id, hand_id).await
        }
    }

    let lifecycle = RoomLifecycleAdapter {
        inner: room_port,
        room_id,
    };

    let _ = run_showdown_settlement_with_batch_payouts_and_alerts(
        engine,
        settlement_service,
        settlement_repo,
        wallet,
        &lifecycle,
        &state,
        &showdown_input,
        rake_policy,
        BatchPayoutSettlementInput {
            seat_addresses: &seat_addresses,
            min_confirmations,
        },
        trace_id,
        alert_sink,
        audit_sink,
    )
    .await?;

    Ok(AutoSettlementOutcome::Settled)
}

pub fn spawn_auto_settlement_loop<L, SP, SR, W>(
    room_service: AppRoomService,
    room_ids_provider: Arc<dyn Fn() -> Result<Vec<RoomId>, String> + Send + Sync>,
    engine: Arc<PokerEngine>,
    settlement_service: Arc<SettlementService<L>>,
    settlement_repo: Arc<SR>,
    wallet: Arc<W>,
    showdown_provider: Arc<SP>,
    cfg: AutoSettlementLoopConfig,
    mut shutdown_rx: oneshot::Receiver<()>,
    alert_sink: Option<Arc<dyn SettlementAlertSink>>,
    audit_sink: Option<Arc<dyn SettlementAuditSink>>,
) -> tokio::task::JoinHandle<()>
where
    L: ledger_store::LedgerRepository + Send + Sync + 'static,
    SP: ShowdownInputProvider + 'static,
    SR: ledger_store::SettlementPersistenceRepository + Send + Sync + 'static,
    W: SettlementWalletAdapter + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(cfg.poll_interval);
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    info!("auto settlement loop shutdown");
                    break;
                }
                _ = ticker.tick() => {
                    let room_ids = match room_ids_provider() {
                        Ok(ids) => ids,
                        Err(err) => {
                            warn!(error = %err, "auto settlement room id provider failed");
                            continue;
                        }
                    };
                    for room_id in room_ids {
                        if let Err(err) = try_auto_settle_showdown_room(
                            room_id,
                            &engine,
                            &settlement_service,
                            &*settlement_repo,
                            &*wallet,
                            &room_service,
                            &*showdown_provider,
                            cfg.rake_policy,
                            cfg.min_confirmations,
                            TraceId::new(),
                            alert_sink.as_deref(),
                            audit_sink.as_deref(),
                        ).await {
                            warn!(room_id = %room_id.0, error = %err, "auto settlement attempt failed");
                        }
                    }
                }
            }
        }
    })
}

pub fn spawn_auto_settlement_loop_with_batch<L, SP, SR, W>(
    room_service: AppRoomService,
    room_ids_provider: Arc<dyn Fn() -> Result<Vec<RoomId>, String> + Send + Sync>,
    engine: Arc<PokerEngine>,
    settlement_service: Arc<SettlementService<L>>,
    settlement_repo: Arc<SR>,
    wallet: Arc<W>,
    showdown_provider: Arc<SP>,
    cfg: AutoSettlementLoopConfig,
    mut shutdown_rx: oneshot::Receiver<()>,
    alert_sink: Option<Arc<dyn SettlementAlertSink>>,
    audit_sink: Option<Arc<dyn SettlementAuditSink>>,
) -> tokio::task::JoinHandle<()>
where
    L: ledger_store::LedgerRepository + Send + Sync + 'static,
    SP: ShowdownInputProvider + 'static,
    SR: ledger_store::SettlementPersistenceRepository + Send + Sync + 'static,
    W: BatchSettlementWalletAdapter + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(cfg.poll_interval);
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    info!("auto settlement loop shutdown");
                    break;
                }
                _ = ticker.tick() => {
                    let room_ids = match room_ids_provider() {
                        Ok(ids) => ids,
                        Err(err) => {
                            warn!(error = %err, "auto settlement room id provider failed");
                            continue;
                        }
                    };
                    for room_id in room_ids {
                        if let Err(err) = try_auto_settle_showdown_room_with_batch(
                            room_id,
                            &engine,
                            &settlement_service,
                            &*settlement_repo,
                            &*wallet,
                            &room_service,
                            &*showdown_provider,
                            cfg.rake_policy,
                            cfg.min_confirmations,
                            TraceId::new(),
                            alert_sink.as_deref(),
                            audit_sink.as_deref(),
                        ).await {
                            warn!(room_id = %room_id.0, error = %err, "auto settlement attempt failed");
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use crate::showdown_admin_repo::AppShowdownAdminRepository;
    use agent_auth::SignedRequestMeta;
    use ledger_store::{InMemoryLedgerRepository, InMemorySettlementPersistenceRepository};
    use ops_http::ShowdownAdminRepository;
    use poker_domain::{Chips, HandId};
    use poker_engine::EngineState;
    use rpc_gateway::{RoomBindAddressRequest, RoomServicePort};
    use rs_poker::core::FlatHand;
    use settlement::SettlementTransferReceipt;

    #[derive(Debug, Default, Clone)]
    struct FakeRoomPort {
        state: Arc<Mutex<Option<EngineState>>>,
        completed: Arc<Mutex<Vec<(RoomId, HandId)>>>,
    }

    #[async_trait]
    impl AutoSettlementRoomPort for FakeRoomPort {
        async fn get_engine_state(
            &self,
            _room_id: RoomId,
        ) -> Result<Option<poker_engine::EngineState>, String> {
            Ok(self.state.lock().map_err(|_| "lock".to_string())?.clone())
        }

        async fn complete_settlement(
            &self,
            room_id: RoomId,
            hand_id: HandId,
        ) -> Result<(), String> {
            self.completed
                .lock()
                .map_err(|_| "lock".to_string())?
                .push((room_id, hand_id));
            Ok(())
        }
    }

    #[derive(Debug, Default, Clone)]
    struct FakeProvider {
        showdown: Arc<Mutex<Option<ShowdownInput>>>,
        seat_addresses: Arc<Mutex<HashMap<SeatId, String>>>,
    }

    #[derive(Default)]
    struct NoopLoopProvider;

    #[async_trait]
    impl ShowdownInputProvider for NoopLoopProvider {
        async fn get_showdown_input(
            &self,
            _room_id: RoomId,
            _hand_id: HandId,
        ) -> Result<Option<ShowdownInput>, String> {
            Ok(None)
        }

        async fn get_settlement_seat_addresses(
            &self,
            _room_id: RoomId,
            _hand_id: HandId,
        ) -> Result<HashMap<SeatId, String>, String> {
            Ok(HashMap::new())
        }
    }

    #[async_trait]
    impl ShowdownInputProvider for FakeProvider {
        async fn get_showdown_input(
            &self,
            _room_id: RoomId,
            _hand_id: HandId,
        ) -> Result<Option<ShowdownInput>, String> {
            Ok(self
                .showdown
                .lock()
                .map_err(|_| "lock".to_string())?
                .clone())
        }

        async fn get_settlement_seat_addresses(
            &self,
            _room_id: RoomId,
            _hand_id: HandId,
        ) -> Result<HashMap<SeatId, String>, String> {
            Ok(self
                .seat_addresses
                .lock()
                .map_err(|_| "lock".to_string())?
                .clone())
        }
    }

    #[derive(Debug, Default, Clone)]
    struct FakeWallet;

    #[async_trait]
    impl SettlementWalletAdapter for FakeWallet {
        async fn submit_transfer(
            &self,
            request: &settlement::SettlementTransferRequest,
        ) -> Result<String, String> {
            Ok(format!("0x{:02x}", request.to_seat_id))
        }

        async fn get_transfer_receipt(
            &self,
            tx_hash: &str,
        ) -> Result<SettlementTransferReceipt, String> {
            Ok(SettlementTransferReceipt {
                tx_hash: tx_hash.to_string(),
                status: settlement::SettlementTxStatus::Confirmed,
                confirmations: 10,
            })
        }
    }

    #[tokio::test]
    async fn returns_waiting_when_not_showdown_or_missing_inputs() {
        let room_id = RoomId::new();
        let engine = PokerEngine::new();
        let settlement_service = SettlementService::new(InMemoryLedgerRepository::new());
        let settlement_repo = InMemorySettlementPersistenceRepository::new();
        let room_port = FakeRoomPort::default();
        let provider = FakeProvider::default();
        let wallet = FakeWallet;

        let outcome = try_auto_settle_showdown_room(
            room_id,
            &engine,
            &settlement_service,
            &settlement_repo,
            &wallet,
            &room_port,
            &provider,
            RakePolicy::zero(),
            1,
            TraceId::new(),
            None,
            None,
        )
        .await
        .expect("ok");
        assert_eq!(outcome, AutoSettlementOutcome::NoActiveHand);

        let mut state = EngineState::new(room_id, 1);
        state.snapshot.status = HandStatus::Running;
        *room_port.state.lock().expect("lock") = Some(state);
        let outcome = try_auto_settle_showdown_room(
            room_id,
            &engine,
            &settlement_service,
            &settlement_repo,
            &wallet,
            &room_port,
            &provider,
            RakePolicy::zero(),
            1,
            TraceId::new(),
            None,
            None,
        )
        .await
        .expect("ok");
        assert_eq!(outcome, AutoSettlementOutcome::NotShowdown);
    }

    #[tokio::test]
    async fn settles_showdown_when_provider_has_inputs() {
        let room_id = RoomId::new();
        let engine = PokerEngine::new();
        let settlement_service = SettlementService::new(InMemoryLedgerRepository::new());
        let settlement_repo = InMemorySettlementPersistenceRepository::new();
        let room_port = FakeRoomPort::default();
        let provider = FakeProvider::default();
        let wallet = FakeWallet;

        let mut state = EngineState::new(room_id, 1);
        state.snapshot.status = HandStatus::Showdown;
        state.snapshot.pot_total = Chips(20);
        state.pot.main_pot = Chips(20);
        state.pot.player_contributions.insert(0, Chips(10));
        state.pot.player_contributions.insert(1, Chips(10));
        *room_port.state.lock().expect("lock") = Some(state.clone());

        let board = FlatHand::new_from_str("AhKhQhJhTh")
            .expect("board")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let s0 = FlatHand::new_from_str("2c3d").expect("s0");
        let s1 = FlatHand::new_from_str("4s5c").expect("s1");
        *provider.showdown.lock().expect("lock") = Some(ShowdownInput {
            board,
            revealed_hole_cards: vec![(0, [s0[0], s0[1]]), (1, [s1[0], s1[1]])],
        });
        provider
            .seat_addresses
            .lock()
            .expect("lock")
            .extend([(0, "0xseat0".to_string()), (1, "0xseat1".to_string())]);

        let outcome = try_auto_settle_showdown_room(
            room_id,
            &engine,
            &settlement_service,
            &settlement_repo,
            &wallet,
            &room_port,
            &provider,
            RakePolicy::zero(),
            1,
            TraceId::new(),
            None,
            None,
        )
        .await
        .expect("ok");
        assert_eq!(outcome, AutoSettlementOutcome::Settled);
        assert_eq!(settlement_repo.settlement_plans_len(), 1);
        assert_eq!(
            room_port.completed.lock().expect("lock").as_slice(),
            &[(room_id, state.snapshot.hand_id)]
        );
    }

    #[tokio::test]
    async fn auto_settlement_loop_ticks_and_shuts_down() {
        let room_service = AppRoomService::new();
        let engine = Arc::new(PokerEngine::new());
        let settlement_service = Arc::new(SettlementService::new(InMemoryLedgerRepository::new()));
        let settlement_repo = Arc::new(InMemorySettlementPersistenceRepository::new());
        let wallet = Arc::new(FakeWallet);
        let provider = Arc::new(NoopLoopProvider);
        let (tx, rx) = oneshot::channel();
        let room_ids_provider: Arc<dyn Fn() -> Result<Vec<RoomId>, String> + Send + Sync> =
            Arc::new(|| Ok(Vec::new()));

        let handle = spawn_auto_settlement_loop(
            room_service,
            room_ids_provider,
            engine,
            settlement_service,
            settlement_repo,
            wallet,
            provider,
            AutoSettlementLoopConfig {
                poll_interval: Duration::from_millis(10),
                ..AutoSettlementLoopConfig::default()
            },
            rx,
            None,
            None,
        );

        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = tx.send(());
        handle.await.expect("join");
    }

    #[tokio::test]
    async fn app_room_provider_falls_back_to_room_service_bound_addresses() {
        let room_service = AppRoomService::new();
        let provider = AppRoomShowdownInputProvider::new(room_service.clone());
        let room_id = RoomId::new();
        let hand_id = HandId::new();
        let room = room_service.ensure_room(room_id).expect("room");
        room.join(0).await.expect("join");
        room_service
            .bind_address(RoomBindAddressRequest {
                room_id,
                seat_id: 0,
                seat_address: "cfx:seat0".to_string(),
                request_meta: SignedRequestMeta {
                    request_id: poker_domain::RequestId::new(),
                    request_nonce: "n1".to_string(),
                    request_ts: chrono::Utc::now(),
                    request_expiry_ms: 30_000,
                    signature_pubkey_id: "k".to_string(),
                    signature: "s".to_string(),
                },
            })
            .await
            .expect("bind");

        let addresses = provider
            .get_settlement_seat_addresses(room_id, hand_id)
            .await
            .expect("addresses");
        assert_eq!(addresses.get(&0).map(String::as_str), Some("cfx:seat0"));
    }

    #[tokio::test]
    async fn auto_settlement_accepts_showdown_input_from_admin_repo_and_uses_bound_addresses() {
        let room_id = RoomId::new();
        let engine = PokerEngine::new();
        let settlement_service = SettlementService::new(InMemoryLedgerRepository::new());
        let settlement_repo = InMemorySettlementPersistenceRepository::new();
        let room_port = FakeRoomPort::default();
        let wallet = FakeWallet;

        let mut state = EngineState::new(room_id, 1);
        state.snapshot.status = HandStatus::Showdown;
        state.snapshot.pot_total = Chips(20);
        state.pot.main_pot = Chips(20);
        state.pot.player_contributions.insert(0, Chips(10));
        state.pot.player_contributions.insert(1, Chips(10));
        *room_port.state.lock().expect("lock") = Some(state.clone());

        let app_room_service = AppRoomService::new();
        let provider = AppRoomShowdownInputProvider::new(app_room_service.clone());
        let admin_repo = AppShowdownAdminRepository::new(provider.clone());

        let room = app_room_service.ensure_room(room_id).expect("room");
        room.join(0).await.expect("join seat0");
        room.join(1).await.expect("join seat1");
        for (seat_id, seat_address) in [(0, "cfx:seat0"), (1, "cfx:seat1")] {
            app_room_service
                .bind_address(RoomBindAddressRequest {
                    room_id,
                    seat_id,
                    seat_address: seat_address.to_string(),
                    request_meta: SignedRequestMeta {
                        request_id: poker_domain::RequestId::new(),
                        request_nonce: format!("nonce-{seat_id}"),
                        request_ts: chrono::Utc::now(),
                        request_expiry_ms: 30_000,
                        signature_pubkey_id: "demo".to_string(),
                        signature: "sig".to_string(),
                    },
                })
                .await
                .expect("bind address");
        }

        admin_repo
            .upsert_showdown_input(
                state.snapshot.hand_id,
                ops_http::AdminUpsertShowdownInputRequest {
                    room_id: room_id.0.to_string(),
                    board: "AhKhQhJhTh".to_string(),
                    revealed_hole_cards: vec![
                        ops_http::ShowdownSeatCardsInput {
                            seat_id: 0,
                            cards: "2c3d".to_string(),
                        },
                        ops_http::ShowdownSeatCardsInput {
                            seat_id: 1,
                            cards: "4s5c".to_string(),
                        },
                    ],
                },
            )
            .await
            .expect("admin showdown upsert");

        let outcome = try_auto_settle_showdown_room(
            room_id,
            &engine,
            &settlement_service,
            &settlement_repo,
            &wallet,
            &room_port,
            &provider,
            RakePolicy::zero(),
            1,
            TraceId::new(),
            None,
            None,
        )
        .await
        .expect("auto settle");

        assert_eq!(outcome, AutoSettlementOutcome::Settled);
        assert_eq!(settlement_repo.settlement_plans_len(), 1);
    }
}
