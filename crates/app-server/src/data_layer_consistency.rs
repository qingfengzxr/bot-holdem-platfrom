#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use audit_store::InMemoryAuditRepository;
    use chain_watcher::{TxVerificationCallback, VerificationCallbackSink, VerificationStatus};
    use ledger_store::{
        InMemoryChainTxRepository, InMemoryLedgerRepository,
        InMemorySettlementPersistenceRepository,
    };
    use poker_domain::{Chips, HandId, RoomId, TraceId};
    use poker_engine::{EngineState, PokerEngine, ShowdownInput};
    use rs_poker::core::FlatHand;
    use settlement::{
        RakePolicy, SettlementService, SettlementTransferReceipt, SettlementWalletAdapter,
    };

    use crate::chain_callback_sink::AppChainCallbackSink;
    use crate::room_service_port::AppRoomService;
    use crate::settlement_audit_sink::AuditSettlementBehaviorSink;
    use crate::settlement_orchestrator::{
        DirectPayoutSettlementInput, SettlementLifecyclePort,
        run_showdown_settlement_with_direct_payouts_and_alerts,
    };

    #[derive(Debug, Default, Clone)]
    struct FakeRoomPort {
        completed: Arc<Mutex<Vec<HandId>>>,
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

    #[derive(Debug, Default, Clone)]
    struct FailSecondWallet;

    #[async_trait]
    impl SettlementWalletAdapter for FailSecondWallet {
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
                status: if tx_hash.ends_with("01") {
                    settlement::SettlementTxStatus::Failed
                } else {
                    settlement::SettlementTxStatus::Confirmed
                },
                confirmations: 10,
            })
        }
    }

    #[tokio::test]
    async fn unmatched_chain_callback_writes_verification_exception_credit_and_audit() {
        let room_service = AppRoomService::new();
        let chain_repo = Arc::new(InMemoryChainTxRepository::new());
        let audit_repo = Arc::new(InMemoryAuditRepository::default());
        let sink = AppChainCallbackSink::new(room_service, chain_repo.clone())
            .with_audit_repo(audit_repo.clone());

        sink.on_verification_result(TxVerificationCallback {
            room_id: RoomId::new(),
            hand_id: None,
            seat_id: Some(1),
            action_seq: Some(8),
            tx_hash: "0xunmatched".to_string(),
            status: VerificationStatus::Unmatched,
            confirmations: 3,
            failure_reason: Some("value_mismatch".to_string()),
        })
        .await
        .expect("callback");

        assert_eq!(chain_repo.tx_verifications_len(), 1);
        assert_eq!(chain_repo.exception_credits_len(), 1);

        let behavior_events = audit_repo.behavior_events.lock().expect("lock");
        assert_eq!(behavior_events.len(), 1);
        assert_eq!(
            behavior_events[0].event_kind,
            "chain_tx_verification_callback"
        );
        assert_eq!(behavior_events[0].event_source, "chain_watcher");
        assert_eq!(
            behavior_events[0].related_tx_hash.as_deref(),
            Some("0xunmatched")
        );
        assert_eq!(behavior_events[0].severity, "error");
    }

    #[tokio::test]
    async fn direct_payout_failure_persists_manual_review_and_audit_behavior_chain() {
        let engine = PokerEngine::new();
        let ledger_repo = InMemoryLedgerRepository::new();
        let settlement_service = SettlementService::new(ledger_repo.clone());
        let settlement_repo = InMemorySettlementPersistenceRepository::new();
        let audit_repo = Arc::new(InMemoryAuditRepository::default());
        let audit_sink = AuditSettlementBehaviorSink::new(audit_repo.clone());
        let room_port = FakeRoomPort::default();
        let wallet = FailSecondWallet;

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
        let s0 = FlatHand::new_from_str("2c3d").expect("s0");
        let s1 = FlatHand::new_from_str("4s5c").expect("s1");
        let showdown = ShowdownInput {
            board,
            revealed_hole_cards: vec![(0, [s0[0], s0[1]]), (1, [s1[0], s1[1]])],
        };
        let seat_addresses =
            HashMap::from([(0_u8, "0xseat0".to_string()), (1_u8, "0xseat1".to_string())]);

        run_showdown_settlement_with_direct_payouts_and_alerts(
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
            TraceId::new(),
            None,
            Some(&audit_sink),
        )
        .await
        .expect("orchestrate");

        let records = settlement_repo.settlement_records_snapshot();
        assert!(!records.is_empty());
        assert!(
            records
                .iter()
                .any(|r| r.settlement_status == "manual_review_required")
        );
        assert!(records.iter().any(|r| r.retry_count >= 1));

        let behavior_events = audit_repo.behavior_events.lock().expect("lock");
        assert!(
            behavior_events
                .iter()
                .any(|e| e.event_kind == "settlement_payout_submitted")
        );
        assert!(
            behavior_events
                .iter()
                .any(|e| e.event_kind == "settlement_payout_receipt")
        );
        assert!(
            behavior_events
                .iter()
                .any(|e| e.event_kind == "settlement_manual_review_required")
        );

        let ledger_entries = ledger_repo.entries_snapshot();
        assert!(
            ledger_entries
                .iter()
                .any(|e| e.entry_type == "settlement_payout")
        );
    }
}
