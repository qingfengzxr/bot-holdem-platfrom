use async_trait::async_trait;
use audit_store::BehaviorEventRecord;
use chrono::Utc;
use ledger_store::{
    SettlementPersistenceRepository, SettlementPlanRecordInsert, SettlementRecordInsert,
    SettlementRecordStatusUpdate,
};
use poker_engine::{EngineState, PokerEngine, ShowdownInput};
use settlement::{
    RakePolicy, SettlementService, SettlementTransferSubmission, SettlementTxStatus,
    SettlementWalletAdapter, mark_failed_receipts_for_manual_review,
};
use table_service::RoomHandle;
use tracing::info;
use uuid::Uuid;

use crate::showdown_settlement::build_settlement_plan_from_showdown;

#[async_trait]
pub trait SettlementLifecyclePort: Send + Sync {
    async fn complete_settlement(&self, hand_id: poker_domain::HandId) -> Result<(), String>;
}

#[derive(Debug, Clone)]
pub struct SettlementAlert {
    pub room_id: poker_domain::RoomId,
    pub hand_id: poker_domain::HandId,
    pub tx_hash: String,
    pub kind: &'static str,
    pub message: String,
}

#[async_trait]
pub trait SettlementAlertSink: Send + Sync {
    async fn emit(&self, alert: SettlementAlert) -> Result<(), String>;
}

#[async_trait]
pub trait SettlementAuditSink: Send + Sync {
    async fn emit(&self, event: BehaviorEventRecord) -> Result<(), String>;
}

#[async_trait]
impl SettlementLifecyclePort for RoomHandle {
    async fn complete_settlement(&self, hand_id: poker_domain::HandId) -> Result<(), String> {
        RoomHandle::complete_settlement(self, hand_id).await
    }
}

pub async fn run_showdown_settlement<L, P>(
    engine: &PokerEngine,
    settlement_service: &SettlementService<L>,
    room_port: &P,
    state: &EngineState,
    showdown_input: &ShowdownInput,
    rake_policy: RakePolicy,
    trace_id: poker_domain::TraceId,
) -> Result<settlement::SettlementPlan, String>
where
    L: ledger_store::LedgerRepository,
    P: SettlementLifecyclePort,
{
    let plan = build_settlement_plan_from_showdown(
        engine,
        settlement_service,
        state,
        showdown_input,
        rake_policy,
    )?;

    settlement_service
        .record_plan(&plan, trace_id)
        .await
        .map_err(|e| e.to_string())?;
    room_port
        .complete_settlement(state.snapshot.hand_id)
        .await
        .map_err(|e| format!("complete settlement failed: {e}"))?;

    info!(
        room_id = %state.snapshot.room_id.0,
        hand_id = %state.snapshot.hand_id.0,
        payout_count = plan.payouts.len(),
        "showdown settlement orchestrated"
    );

    Ok(plan)
}

pub async fn run_showdown_settlement_with_persistence<L, P, S>(
    engine: &PokerEngine,
    settlement_service: &SettlementService<L>,
    settlement_repo: &S,
    room_port: &P,
    state: &EngineState,
    showdown_input: &ShowdownInput,
    rake_policy: RakePolicy,
    trace_id: poker_domain::TraceId,
) -> Result<settlement::SettlementPlan, String>
where
    L: ledger_store::LedgerRepository,
    P: SettlementLifecyclePort,
    S: SettlementPersistenceRepository,
{
    let plan = build_settlement_plan_from_showdown(
        engine,
        settlement_service,
        state,
        showdown_input,
        rake_policy,
    )?;

    let settlement_plan_id = Uuid::now_v7();
    settlement_repo
        .insert_settlement_plan(&SettlementPlanRecordInsert {
            settlement_plan_id,
            room_id: state.snapshot.room_id,
            hand_id: state.snapshot.hand_id,
            status: "created".to_string(),
            rake_amount: plan.rake_amount,
            payout_total: plan
                .payout_total()
                .map_err(|e| format!("settlement payout_total failed: {e}"))?,
            payload_json: serde_json::to_value(&plan).map_err(|e| e.to_string())?,
            created_at: Utc::now(),
            finalized_at: None,
        })
        .await
        .map_err(|e| e.to_string())?;

    settlement_service
        .record_plan(&plan, trace_id)
        .await
        .map_err(|e| e.to_string())?;

    settlement_repo
        .insert_settlement_record(&SettlementRecordInsert {
            settlement_record_id: Uuid::now_v7(),
            settlement_plan_id,
            room_id: state.snapshot.room_id,
            hand_id: state.snapshot.hand_id,
            tx_hash: None,
            settlement_status: "planned_offchain_recorded".to_string(),
            retry_count: 0,
            error_detail: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
        .await
        .map_err(|e| e.to_string())?;

    room_port
        .complete_settlement(state.snapshot.hand_id)
        .await
        .map_err(|e| format!("complete settlement failed: {e}"))?;

    info!(
        room_id = %state.snapshot.room_id.0,
        hand_id = %state.snapshot.hand_id.0,
        payout_count = plan.payouts.len(),
        "showdown settlement orchestrated with persistence"
    );

    Ok(plan)
}

pub struct DirectPayoutSettlementInput<'a> {
    pub seat_addresses: &'a std::collections::HashMap<poker_domain::SeatId, String>,
    pub min_confirmations: u64,
}

pub async fn run_showdown_settlement_with_direct_payouts<L, P, S, W>(
    engine: &PokerEngine,
    settlement_service: &SettlementService<L>,
    settlement_repo: &S,
    wallet: &W,
    room_port: &P,
    state: &EngineState,
    showdown_input: &ShowdownInput,
    rake_policy: RakePolicy,
    direct: DirectPayoutSettlementInput<'_>,
    trace_id: poker_domain::TraceId,
) -> Result<settlement::SettlementPlan, String>
where
    L: ledger_store::LedgerRepository,
    P: SettlementLifecyclePort,
    S: SettlementPersistenceRepository,
    W: SettlementWalletAdapter,
{
    run_showdown_settlement_with_direct_payouts_and_alerts(
        engine,
        settlement_service,
        settlement_repo,
        wallet,
        room_port,
        state,
        showdown_input,
        rake_policy,
        direct,
        trace_id,
        None,
        None,
    )
    .await
}

pub async fn run_showdown_settlement_with_direct_payouts_and_alerts<L, P, S, W>(
    engine: &PokerEngine,
    settlement_service: &SettlementService<L>,
    settlement_repo: &S,
    wallet: &W,
    room_port: &P,
    state: &EngineState,
    showdown_input: &ShowdownInput,
    rake_policy: RakePolicy,
    direct: DirectPayoutSettlementInput<'_>,
    trace_id: poker_domain::TraceId,
    alert_sink: Option<&dyn SettlementAlertSink>,
    audit_sink: Option<&dyn SettlementAuditSink>,
) -> Result<settlement::SettlementPlan, String>
where
    L: ledger_store::LedgerRepository,
    P: SettlementLifecyclePort,
    S: SettlementPersistenceRepository,
    W: SettlementWalletAdapter,
{
    let plan = build_settlement_plan_from_showdown(
        engine,
        settlement_service,
        state,
        showdown_input,
        rake_policy,
    )?;
    let settlement_plan_id = Uuid::now_v7();
    settlement_repo
        .insert_settlement_plan(&SettlementPlanRecordInsert {
            settlement_plan_id,
            room_id: state.snapshot.room_id,
            hand_id: state.snapshot.hand_id,
            status: "created".to_string(),
            rake_amount: plan.rake_amount,
            payout_total: plan
                .payout_total()
                .map_err(|e| format!("settlement payout_total failed: {e}"))?,
            payload_json: serde_json::to_value(&plan).map_err(|e| e.to_string())?,
            created_at: Utc::now(),
            finalized_at: None,
        })
        .await
        .map_err(|e| e.to_string())?;

    settlement_service
        .record_plan(&plan, trace_id)
        .await
        .map_err(|e| e.to_string())?;

    let submissions = settlement_service
        .submit_direct_payouts(&plan, direct.seat_addresses, wallet)
        .await
        .map_err(|e| e.to_string())?;
    persist_submission_records(
        settlement_repo,
        settlement_plan_id,
        state,
        &submissions,
        "submitted",
    )
    .await?;
    emit_settlement_submission_audits(audit_sink, state, &submissions, trace_id).await;

    let receipts = settlement_service
        .check_payout_submissions(wallet, &submissions, direct.min_confirmations)
        .await
        .map_err(|e| e.to_string())?;
    persist_receipt_status_updates(settlement_repo, &receipts).await?;
    emit_settlement_receipt_audits(audit_sink, state, &receipts, trace_id).await;
    let manual_marked = mark_failed_receipts_for_manual_review(
        settlement_repo,
        &receipts,
        1,
        "manual review required after failed direct payout receipt",
    )
    .await
    .map_err(|e| e.to_string())?;
    if manual_marked > 0 {
        emit_manual_review_audits(audit_sink, state, &receipts, trace_id).await;
        emit_failed_payout_alerts(alert_sink, state, &receipts).await;
    }
    let all_confirmed = receipts
        .iter()
        .all(|r| matches!(r.status, SettlementTxStatus::Confirmed));

    if all_confirmed {
        room_port
            .complete_settlement(state.snapshot.hand_id)
            .await
            .map_err(|e| format!("complete settlement failed: {e}"))?;
    }

    info!(
        room_id = %state.snapshot.room_id.0,
        hand_id = %state.snapshot.hand_id.0,
        submitted = submissions.len(),
        manual_marked,
        all_confirmed,
        "direct payout settlement orchestrated"
    );
    Ok(plan)
}

async fn emit_behavior(audit_sink: Option<&dyn SettlementAuditSink>, record: BehaviorEventRecord) {
    if let Some(sink) = audit_sink {
        if let Err(err) = sink.emit(record).await {
            tracing::warn!(error = %err, "failed to emit settlement audit behavior event");
        }
    }
}

async fn emit_settlement_submission_audits(
    audit_sink: Option<&dyn SettlementAuditSink>,
    state: &EngineState,
    submissions: &[SettlementTransferSubmission],
    trace_id: poker_domain::TraceId,
) {
    for submission in submissions {
        emit_behavior(
            audit_sink,
            BehaviorEventRecord {
                behavior_event_id: Uuid::now_v7().to_string(),
                event_kind: "settlement_payout_submitted".to_string(),
                event_source: "settlement_orchestrator".to_string(),
                room_id: Some(state.snapshot.room_id),
                hand_id: Some(state.snapshot.hand_id),
                seat_id: Some(submission.seat_id),
                action_seq: None,
                related_attempt_id: None,
                related_tx_hash: Some(submission.tx_hash.clone()),
                severity: "info".to_string(),
                payload_json: serde_json::json!({
                    "seat_id": submission.seat_id,
                    "tx_hash": submission.tx_hash,
                }),
                occurred_at: Utc::now(),
                trace_id,
            },
        )
        .await;
    }
}

async fn emit_settlement_receipt_audits(
    audit_sink: Option<&dyn SettlementAuditSink>,
    state: &EngineState,
    receipts: &[settlement::SettlementTransferReceipt],
    trace_id: poker_domain::TraceId,
) {
    for receipt in receipts {
        emit_behavior(
            audit_sink,
            BehaviorEventRecord {
                behavior_event_id: Uuid::now_v7().to_string(),
                event_kind: "settlement_payout_receipt".to_string(),
                event_source: "settlement_orchestrator".to_string(),
                room_id: Some(state.snapshot.room_id),
                hand_id: Some(state.snapshot.hand_id),
                seat_id: None,
                action_seq: None,
                related_attempt_id: None,
                related_tx_hash: Some(receipt.tx_hash.clone()),
                severity: match receipt.status {
                    SettlementTxStatus::Failed => "error",
                    SettlementTxStatus::Pending => "warn",
                    SettlementTxStatus::Confirmed => "info",
                }
                .to_string(),
                payload_json: serde_json::json!({
                    "tx_hash": receipt.tx_hash,
                    "status": format!("{:?}", receipt.status).to_lowercase(),
                    "confirmations": receipt.confirmations,
                }),
                occurred_at: Utc::now(),
                trace_id,
            },
        )
        .await;
    }
}

async fn emit_manual_review_audits(
    audit_sink: Option<&dyn SettlementAuditSink>,
    state: &EngineState,
    receipts: &[settlement::SettlementTransferReceipt],
    trace_id: poker_domain::TraceId,
) {
    for receipt in receipts {
        if !matches!(receipt.status, SettlementTxStatus::Failed) {
            continue;
        }
        emit_behavior(
            audit_sink,
            BehaviorEventRecord {
                behavior_event_id: Uuid::now_v7().to_string(),
                event_kind: "settlement_manual_review_required".to_string(),
                event_source: "settlement_orchestrator".to_string(),
                room_id: Some(state.snapshot.room_id),
                hand_id: Some(state.snapshot.hand_id),
                seat_id: None,
                action_seq: None,
                related_attempt_id: None,
                related_tx_hash: Some(receipt.tx_hash.clone()),
                severity: "error".to_string(),
                payload_json: serde_json::json!({
                    "tx_hash": receipt.tx_hash,
                    "reason": "direct payout receipt failed",
                }),
                occurred_at: Utc::now(),
                trace_id,
            },
        )
        .await;
    }
}

async fn emit_failed_payout_alerts(
    alert_sink: Option<&dyn SettlementAlertSink>,
    state: &EngineState,
    receipts: &[settlement::SettlementTransferReceipt],
) {
    for receipt in receipts {
        if !matches!(receipt.status, SettlementTxStatus::Failed) {
            continue;
        }
        let alert = SettlementAlert {
            room_id: state.snapshot.room_id,
            hand_id: state.snapshot.hand_id,
            tx_hash: receipt.tx_hash.clone(),
            kind: "settlement_manual_review_required",
            message: "direct payout receipt failed; manual review required".to_string(),
        };
        if let Some(sink) = alert_sink {
            if let Err(err) = sink.emit(alert.clone()).await {
                tracing::warn!(error = %err, tx_hash = %receipt.tx_hash, "failed to emit settlement alert");
            }
        } else {
            tracing::warn!(
                room_id = %alert.room_id.0,
                hand_id = %alert.hand_id.0,
                tx_hash = %alert.tx_hash,
                alert_kind = alert.kind,
                message = %alert.message,
                "settlement alert"
            );
        }
    }
}

async fn persist_submission_records<S: SettlementPersistenceRepository>(
    settlement_repo: &S,
    settlement_plan_id: Uuid,
    state: &EngineState,
    submissions: &[SettlementTransferSubmission],
    status: &str,
) -> Result<(), String> {
    for submission in submissions {
        settlement_repo
            .insert_settlement_record(&SettlementRecordInsert {
                settlement_record_id: Uuid::now_v7(),
                settlement_plan_id,
                room_id: state.snapshot.room_id,
                hand_id: state.snapshot.hand_id,
                tx_hash: Some(submission.tx_hash.clone()),
                settlement_status: status.to_string(),
                retry_count: 0,
                error_detail: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn persist_receipt_status_updates<S: SettlementPersistenceRepository>(
    settlement_repo: &S,
    receipts: &[settlement::SettlementTransferReceipt],
) -> Result<(), String> {
    for receipt in receipts {
        let (status, retry_count, error_detail) = match receipt.status {
            SettlementTxStatus::Confirmed => ("confirmed".to_string(), 0, None),
            SettlementTxStatus::Pending => ("pending".to_string(), 0, None),
            SettlementTxStatus::Failed => (
                "failed".to_string(),
                1,
                Some("wallet receipt returned failed status".to_string()),
            ),
        };
        settlement_repo
            .update_settlement_record_status_by_tx_hash(&SettlementRecordStatusUpdate {
                tx_hash: receipt.tx_hash.clone(),
                settlement_status: status,
                retry_count,
                error_detail,
                updated_at: Utc::now(),
            })
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ledger_store::{
        InMemoryLedgerRepository, InMemorySettlementPersistenceRepository,
        SettlementPersistenceReadRepository,
    };
    use poker_domain::{Chips, HandId, RoomId};
    use rs_poker::core::FlatHand;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default, Clone)]
    struct FakeRoomPort {
        completed: Arc<Mutex<Vec<HandId>>>,
    }

    #[derive(Debug, Default, Clone)]
    struct FakeWallet {
        confirmations: u64,
        fail_second: bool,
    }

    #[derive(Debug, Default, Clone)]
    struct FakeAlertSink {
        alerts: Arc<Mutex<Vec<SettlementAlert>>>,
    }

    #[derive(Debug, Default, Clone)]
    struct FakeSettlementAuditSink {
        events: Arc<Mutex<Vec<BehaviorEventRecord>>>,
    }

    #[async_trait]
    impl SettlementLifecyclePort for FakeRoomPort {
        async fn complete_settlement(&self, hand_id: HandId) -> Result<(), String> {
            self.completed
                .lock()
                .map_err(|_| "lock poisoned".to_string())?
                .push(hand_id);
            Ok(())
        }
    }

    #[async_trait]
    impl SettlementWalletAdapter for FakeWallet {
        async fn submit_transfer(
            &self,
            request: &settlement::SettlementTransferRequest,
        ) -> Result<String, String> {
            Ok(format!(
                "0x{:x}{:02x}",
                request.hand_id.0.as_u128(),
                request.to_seat_id
            ))
        }

        async fn get_transfer_receipt(
            &self,
            tx_hash: &str,
        ) -> Result<settlement::SettlementTransferReceipt, String> {
            Ok(settlement::SettlementTransferReceipt {
                tx_hash: tx_hash.to_string(),
                status: if self.fail_second && tx_hash.ends_with("01") {
                    settlement::SettlementTxStatus::Failed
                } else {
                    settlement::SettlementTxStatus::Confirmed
                },
                confirmations: self.confirmations,
            })
        }
    }

    #[async_trait]
    impl SettlementAlertSink for FakeAlertSink {
        async fn emit(&self, alert: SettlementAlert) -> Result<(), String> {
            self.alerts
                .lock()
                .map_err(|_| "lock poisoned".to_string())?
                .push(alert);
            Ok(())
        }
    }

    #[async_trait]
    impl SettlementAuditSink for FakeSettlementAuditSink {
        async fn emit(&self, event: BehaviorEventRecord) -> Result<(), String> {
            self.events
                .lock()
                .map_err(|_| "lock poisoned".to_string())?
                .push(event);
            Ok(())
        }
    }

    #[tokio::test]
    async fn orchestrates_showdown_plan_record_and_room_close() {
        let engine = PokerEngine::new();
        let ledger = InMemoryLedgerRepository::new();
        let settlement_service = SettlementService::new(ledger.clone());
        let room_port = FakeRoomPort::default();

        let mut state = EngineState::new(RoomId::new(), 1);
        state.pot.player_contributions.insert(0, Chips(100));
        state.pot.player_contributions.insert(1, Chips(100));
        state.pot.player_contributions.insert(2, Chips(40));
        state.pot.main_pot = Chips(240);
        state.snapshot.pot_total = Chips(240);

        let board = FlatHand::new_from_str("AhKhQh2c3d")
            .expect("board")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let seat0 = FlatHand::new_from_str("4c4d").expect("s0");
        let seat1 = FlatHand::new_from_str("AdAc").expect("s1");
        let seat2 = FlatHand::new_from_str("JhTh").expect("s2");
        let showdown = ShowdownInput {
            board,
            revealed_hole_cards: vec![
                (0, [seat0[0], seat0[1]]),
                (1, [seat1[0], seat1[1]]),
                (2, [seat2[0], seat2[1]]),
            ],
        };

        let plan = run_showdown_settlement(
            &engine,
            &settlement_service,
            &room_port,
            &state,
            &showdown,
            RakePolicy { rake_bps: 300 },
            poker_domain::TraceId::new(),
        )
        .await
        .expect("orchestrated");

        assert_eq!(plan.rake_amount, Chips(7));
        assert_eq!(plan.payout_total().expect("sum"), Chips(233));

        let entries = ledger.entries_snapshot();
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().any(|e| e.entry_type == "rake_accrual"));
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.entry_type == "settlement_payout")
                .count(),
            2
        );

        let completed = room_port.completed.lock().expect("lock");
        assert_eq!(completed.as_slice(), &[state.snapshot.hand_id]);
    }

    #[tokio::test]
    async fn orchestrates_showdown_with_settlement_persistence() {
        let engine = PokerEngine::new();
        let ledger = InMemoryLedgerRepository::new();
        let settlement_service = SettlementService::new(ledger.clone());
        let settlement_repo = InMemorySettlementPersistenceRepository::new();
        let room_port = FakeRoomPort::default();

        let mut state = EngineState::new(RoomId::new(), 1);
        state.pot.player_contributions.insert(0, Chips(10));
        state.pot.player_contributions.insert(1, Chips(10));
        state.pot.main_pot = Chips(20);
        state.snapshot.pot_total = Chips(20);

        let board = FlatHand::new_from_str("AhKhQhJhTh")
            .expect("board")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let seat0 = FlatHand::new_from_str("2c3d").expect("s0");
        let seat1 = FlatHand::new_from_str("4s5c").expect("s1");
        let showdown = ShowdownInput {
            board,
            revealed_hole_cards: vec![(0, [seat0[0], seat0[1]]), (1, [seat1[0], seat1[1]])],
        };

        let _plan = run_showdown_settlement_with_persistence(
            &engine,
            &settlement_service,
            &settlement_repo,
            &room_port,
            &state,
            &showdown,
            RakePolicy::zero(),
            poker_domain::TraceId::new(),
        )
        .await
        .expect("orchestrated");

        assert_eq!(settlement_repo.settlement_plans_len(), 1);
        assert_eq!(settlement_repo.settlement_records_len(), 1);
        let entries = ledger.entries_snapshot();
        assert!(!entries.is_empty());
    }

    #[tokio::test]
    async fn showdown_settlement_reconciles_pot_plan_and_ledger_entries() {
        let engine = PokerEngine::new();
        let ledger = InMemoryLedgerRepository::new();
        let settlement_service = SettlementService::new(ledger.clone());
        let settlement_repo = InMemorySettlementPersistenceRepository::new();
        let room_port = FakeRoomPort::default();

        let mut state = EngineState::new(RoomId::new(), 1);
        state.pot.player_contributions.insert(0, Chips(100));
        state.pot.player_contributions.insert(1, Chips(100));
        state.pot.player_contributions.insert(2, Chips(40));
        state.pot.main_pot = Chips(240);
        state.snapshot.pot_total = Chips(240);

        let board = FlatHand::new_from_str("AhKhQh2c3d")
            .expect("board")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let seat0 = FlatHand::new_from_str("4c4d").expect("s0");
        let seat1 = FlatHand::new_from_str("AdAc").expect("s1");
        let seat2 = FlatHand::new_from_str("JhTh").expect("s2");
        let showdown = ShowdownInput {
            board,
            revealed_hole_cards: vec![
                (0, [seat0[0], seat0[1]]),
                (1, [seat1[0], seat1[1]]),
                (2, [seat2[0], seat2[1]]),
            ],
        };

        let plan = run_showdown_settlement_with_persistence(
            &engine,
            &settlement_service,
            &settlement_repo,
            &room_port,
            &state,
            &showdown,
            RakePolicy { rake_bps: 300 },
            poker_domain::TraceId::new(),
        )
        .await
        .expect("orchestrated");

        let payout_total = plan.payout_total().expect("payout total");
        assert_eq!(
            payout_total
                .checked_add(plan.rake_amount)
                .expect("plan total"),
            state.snapshot.pot_total
        );

        let entries = ledger.entries_snapshot();
        let ledger_payout_total = entries
            .iter()
            .filter(|e| e.entry_type == "settlement_payout")
            .fold(Chips::ZERO, |acc, e| {
                acc.checked_add(e.amount).expect("sum")
            });
        let ledger_rake_total = entries
            .iter()
            .filter(|e| e.entry_type == "rake_accrual")
            .fold(Chips::ZERO, |acc, e| {
                acc.checked_add(e.amount).expect("sum")
            });
        assert_eq!(ledger_payout_total, payout_total);
        assert_eq!(ledger_rake_total, plan.rake_amount);
        assert_eq!(
            ledger_payout_total
                .checked_add(ledger_rake_total)
                .expect("ledger total"),
            state.snapshot.pot_total
        );

        let records = settlement_repo
            .list_settlement_records_by_hand(state.snapshot.hand_id)
            .await
            .expect("records");
        assert_eq!(records.len(), 1);
        let persisted_plan = settlement_repo
            .get_settlement_plan(records[0].settlement_plan_id)
            .await
            .expect("read")
            .expect("plan");
        assert_eq!(persisted_plan.rake_amount, plan.rake_amount);
        assert_eq!(persisted_plan.payout_total, payout_total);
    }

    #[tokio::test]
    async fn orchestrates_direct_payouts_and_closes_when_confirmed() {
        let engine = PokerEngine::new();
        let ledger = InMemoryLedgerRepository::new();
        let settlement_service = SettlementService::new(ledger);
        let settlement_repo = InMemorySettlementPersistenceRepository::new();
        let room_port = FakeRoomPort::default();
        let wallet = FakeWallet {
            confirmations: 6,
            fail_second: false,
        };

        let mut state = EngineState::new(RoomId::new(), 1);
        state.pot.player_contributions.insert(0, Chips(10));
        state.pot.player_contributions.insert(1, Chips(10));
        state.pot.main_pot = Chips(20);
        state.snapshot.pot_total = Chips(20);

        let board = FlatHand::new_from_str("AhKhQhJhTh")
            .expect("board")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let seat0 = FlatHand::new_from_str("2c3d").expect("s0");
        let seat1 = FlatHand::new_from_str("4s5c").expect("s1");
        let showdown = ShowdownInput {
            board,
            revealed_hole_cards: vec![(0, [seat0[0], seat0[1]]), (1, [seat1[0], seat1[1]])],
        };
        let mut seat_addresses = std::collections::HashMap::new();
        seat_addresses.insert(0_u8, "0xseat0".to_string());
        seat_addresses.insert(1_u8, "0xseat1".to_string());

        let _ = run_showdown_settlement_with_direct_payouts(
            &engine,
            &settlement_service,
            &settlement_repo,
            &wallet,
            &room_port,
            &state,
            &showdown,
            RakePolicy::zero(),
            DirectPayoutSettlementInput {
                seat_addresses: &seat_addresses,
                min_confirmations: 1,
            },
            poker_domain::TraceId::new(),
        )
        .await
        .expect("orchestrated");

        assert_eq!(settlement_repo.settlement_plans_len(), 1);
        assert_eq!(settlement_repo.settlement_records_len(), 2);
        let completed = room_port.completed.lock().expect("lock");
        assert_eq!(completed.as_slice(), &[state.snapshot.hand_id]);
    }

    #[tokio::test]
    async fn direct_payouts_failed_receipt_does_not_close_hand_and_updates_record_status() {
        let engine = PokerEngine::new();
        let ledger = InMemoryLedgerRepository::new();
        let settlement_service = SettlementService::new(ledger);
        let settlement_repo = InMemorySettlementPersistenceRepository::new();
        let room_port = FakeRoomPort::default();
        let wallet = FakeWallet {
            confirmations: 6,
            fail_second: true,
        };

        let mut state = EngineState::new(RoomId::new(), 1);
        state.pot.player_contributions.insert(0, Chips(10));
        state.pot.player_contributions.insert(1, Chips(10));
        state.pot.main_pot = Chips(20);
        state.snapshot.pot_total = Chips(20);

        let board = FlatHand::new_from_str("AhKhQhJhTh")
            .expect("board")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let seat0 = FlatHand::new_from_str("2c3d").expect("s0");
        let seat1 = FlatHand::new_from_str("4s5c").expect("s1");
        let showdown = ShowdownInput {
            board,
            revealed_hole_cards: vec![(0, [seat0[0], seat0[1]]), (1, [seat1[0], seat1[1]])],
        };
        let mut seat_addresses = std::collections::HashMap::new();
        seat_addresses.insert(0_u8, "0xseat0".to_string());
        seat_addresses.insert(1_u8, "0xseat1".to_string());

        let _ = run_showdown_settlement_with_direct_payouts(
            &engine,
            &settlement_service,
            &settlement_repo,
            &wallet,
            &room_port,
            &state,
            &showdown,
            RakePolicy::zero(),
            DirectPayoutSettlementInput {
                seat_addresses: &seat_addresses,
                min_confirmations: 1,
            },
            poker_domain::TraceId::new(),
        )
        .await
        .expect("orchestrated");

        let completed = room_port.completed.lock().expect("lock");
        assert!(
            completed.is_empty(),
            "hand should remain open on failed payout"
        );
        let records = settlement_repo.settlement_records_snapshot();
        assert!(records.iter().any(|r| {
            r.settlement_status == "manual_review_required"
                && r.error_detail
                    .as_deref()
                    .is_some_and(|d| d.contains("manual review required"))
        }));
        assert!(records.iter().any(|r| r.retry_count >= 1));
    }

    #[tokio::test]
    async fn direct_payout_failures_emit_settlement_alerts() {
        let engine = PokerEngine::new();
        let ledger = InMemoryLedgerRepository::new();
        let settlement_service = SettlementService::new(ledger);
        let settlement_repo = InMemorySettlementPersistenceRepository::new();
        let room_port = FakeRoomPort::default();
        let wallet = FakeWallet {
            confirmations: 6,
            fail_second: true,
        };
        let alert_sink = FakeAlertSink::default();

        let mut state = EngineState::new(RoomId::new(), 1);
        state.pot.player_contributions.insert(0, Chips(10));
        state.pot.player_contributions.insert(1, Chips(10));
        state.pot.main_pot = Chips(20);
        state.snapshot.pot_total = Chips(20);

        let board = FlatHand::new_from_str("AhKhQhJhTh")
            .expect("board")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let seat0 = FlatHand::new_from_str("2c3d").expect("s0");
        let seat1 = FlatHand::new_from_str("4s5c").expect("s1");
        let showdown = ShowdownInput {
            board,
            revealed_hole_cards: vec![(0, [seat0[0], seat0[1]]), (1, [seat1[0], seat1[1]])],
        };
        let mut seat_addresses = std::collections::HashMap::new();
        seat_addresses.insert(0_u8, "0xseat0".to_string());
        seat_addresses.insert(1_u8, "0xseat1".to_string());

        let _ = run_showdown_settlement_with_direct_payouts_and_alerts(
            &engine,
            &settlement_service,
            &settlement_repo,
            &wallet,
            &room_port,
            &state,
            &showdown,
            RakePolicy::zero(),
            DirectPayoutSettlementInput {
                seat_addresses: &seat_addresses,
                min_confirmations: 1,
            },
            poker_domain::TraceId::new(),
            Some(&alert_sink),
            None,
        )
        .await
        .expect("orchestrated");

        let alerts = alert_sink.alerts.lock().expect("lock");
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, "settlement_manual_review_required");
    }

    #[tokio::test]
    async fn direct_payout_flow_emits_settlement_behavior_audits() {
        let engine = PokerEngine::new();
        let ledger = InMemoryLedgerRepository::new();
        let settlement_service = SettlementService::new(ledger);
        let settlement_repo = InMemorySettlementPersistenceRepository::new();
        let room_port = FakeRoomPort::default();
        let wallet = FakeWallet {
            confirmations: 6,
            fail_second: true,
        };
        let audit_sink = FakeSettlementAuditSink::default();

        let mut state = EngineState::new(RoomId::new(), 1);
        state.pot.player_contributions.insert(0, Chips(10));
        state.pot.player_contributions.insert(1, Chips(10));
        state.pot.main_pot = Chips(20);
        state.snapshot.pot_total = Chips(20);

        let board = FlatHand::new_from_str("AhKhQhJhTh")
            .expect("board")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let seat0 = FlatHand::new_from_str("2c3d").expect("s0");
        let seat1 = FlatHand::new_from_str("4s5c").expect("s1");
        let showdown = ShowdownInput {
            board,
            revealed_hole_cards: vec![(0, [seat0[0], seat0[1]]), (1, [seat1[0], seat1[1]])],
        };
        let mut seat_addresses = std::collections::HashMap::new();
        seat_addresses.insert(0_u8, "0xseat0".to_string());
        seat_addresses.insert(1_u8, "0xseat1".to_string());

        let _ = run_showdown_settlement_with_direct_payouts_and_alerts(
            &engine,
            &settlement_service,
            &settlement_repo,
            &wallet,
            &room_port,
            &state,
            &showdown,
            RakePolicy::zero(),
            DirectPayoutSettlementInput {
                seat_addresses: &seat_addresses,
                min_confirmations: 1,
            },
            poker_domain::TraceId::new(),
            None,
            Some(&audit_sink),
        )
        .await
        .expect("orchestrated");

        let events = audit_sink.events.lock().expect("lock");
        assert!(
            events
                .iter()
                .any(|e| e.event_kind == "settlement_payout_submitted")
        );
        assert!(
            events
                .iter()
                .any(|e| e.event_kind == "settlement_payout_receipt")
        );
        assert!(
            events
                .iter()
                .any(|e| e.event_kind == "settlement_manual_review_required")
        );
    }
}
