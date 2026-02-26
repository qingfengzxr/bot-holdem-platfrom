use async_trait::async_trait;
use ops_http::{AdminUpsertShowdownInputRequest, ShowdownAdminRepository};
use poker_domain::{HandId, RoomId};
use poker_engine::ShowdownInput;
use rs_poker::core::FlatHand;

use crate::auto_settlement::AppRoomShowdownInputProvider;

#[derive(Clone)]
pub struct AppShowdownAdminRepository {
    provider: AppRoomShowdownInputProvider,
}

impl AppShowdownAdminRepository {
    #[must_use]
    pub fn new(provider: AppRoomShowdownInputProvider) -> Self {
        Self { provider }
    }

    fn parse_showdown_input(
        payload: &AdminUpsertShowdownInputRequest,
    ) -> Result<(RoomId, ShowdownInput), String> {
        let room_id = uuid::Uuid::parse_str(&payload.room_id)
            .map(RoomId)
            .map_err(|e| format!("invalid room_id: {e}"))?;
        let board = FlatHand::new_from_str(&payload.board)
            .map_err(|e| format!("invalid board cards: {e}"))?
            .iter()
            .copied()
            .collect::<Vec<_>>();

        let mut revealed_hole_cards = Vec::with_capacity(payload.revealed_hole_cards.len());
        for seat in &payload.revealed_hole_cards {
            let cards = FlatHand::new_from_str(&seat.cards)
                .map_err(|e| format!("invalid seat cards for seat {}: {e}", seat.seat_id))?;
            if cards.len() != 2 {
                return Err(format!(
                    "seat {} must provide exactly 2 cards",
                    seat.seat_id
                ));
            }
            revealed_hole_cards.push((seat.seat_id, [cards[0], cards[1]]));
        }

        Ok((
            room_id,
            ShowdownInput {
                board,
                revealed_hole_cards,
            },
        ))
    }
}

#[async_trait]
impl ShowdownAdminRepository for AppShowdownAdminRepository {
    async fn upsert_showdown_input(
        &self,
        hand_id: HandId,
        payload: AdminUpsertShowdownInputRequest,
    ) -> Result<(), String> {
        let (room_id, showdown_input) = Self::parse_showdown_input(&payload)?;
        self.provider
            .upsert_showdown_input(room_id, hand_id, showdown_input)
            .await
            .map_err(|e| format!("showdown provider write failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_settlement::ShowdownInputProvider;
    use crate::room_service_port::AppRoomService;
    use ledger_store::InMemorySettlementPersistenceRepository;
    use poker_domain::RoomId;

    #[tokio::test]
    async fn admin_repo_parses_and_writes_showdown_input() {
        let room_service = AppRoomService::default();
        let provider = AppRoomShowdownInputProvider::new(room_service);
        let repo = AppShowdownAdminRepository::new(provider.clone());
        let room_id = RoomId::new();
        let hand_id = HandId::new();

        repo.upsert_showdown_input(
            hand_id,
            AdminUpsertShowdownInputRequest {
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
        .expect("upsert showdown input");

        let got = provider
            .get_showdown_input(room_id, hand_id)
            .await
            .expect("provider ok")
            .expect("showdown input");
        assert_eq!(got.board.len(), 5);
        assert_eq!(got.revealed_hole_cards.len(), 2);
        assert_eq!(got.revealed_hole_cards[0].0, 0);
        assert_eq!(got.revealed_hole_cards[1].0, 1);
    }

    #[tokio::test]
    async fn admin_repo_rejects_invalid_cards() {
        let repo = AppShowdownAdminRepository::new(AppRoomShowdownInputProvider::new(
            AppRoomService::default(),
        ));
        let err = repo
            .upsert_showdown_input(
                HandId::new(),
                AdminUpsertShowdownInputRequest {
                    room_id: RoomId::new().0.to_string(),
                    board: "invalid".to_string(),
                    revealed_hole_cards: vec![],
                },
            )
            .await
            .expect_err("invalid board should fail");
        assert!(err.contains("invalid board"));
    }

    #[tokio::test]
    async fn admin_repo_persists_showdown_input_for_new_provider_instance() {
        let room_service = AppRoomService::default();
        let repo_store = std::sync::Arc::new(InMemorySettlementPersistenceRepository::new());
        let provider = AppRoomShowdownInputProvider::new(room_service.clone())
            .with_persistence(repo_store.clone(), repo_store.clone());
        let repo = AppShowdownAdminRepository::new(provider);
        let room_id = RoomId::new();
        let hand_id = HandId::new();

        repo.upsert_showdown_input(
            hand_id,
            AdminUpsertShowdownInputRequest {
                room_id: room_id.0.to_string(),
                board: "AhKhQhJhTh".to_string(),
                revealed_hole_cards: vec![ops_http::ShowdownSeatCardsInput {
                    seat_id: 0,
                    cards: "2c3d".to_string(),
                }],
            },
        )
        .await
        .expect("persist showdown input");

        let fresh_provider = AppRoomShowdownInputProvider::new(room_service)
            .with_persistence(repo_store.clone(), repo_store);
        let got = fresh_provider
            .get_showdown_input(room_id, hand_id)
            .await
            .expect("provider read")
            .expect("showdown input");
        assert_eq!(got.board.len(), 5);
        assert_eq!(got.revealed_hole_cards.len(), 1);
        assert_eq!(got.revealed_hole_cards[0].0, 0);
    }
}
