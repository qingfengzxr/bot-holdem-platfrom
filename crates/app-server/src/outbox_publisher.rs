use audit_store::{AuditRepository, AuditStoreError};
use chrono::Utc;
use poker_domain::{HandEvent, HandEventKind};
use rpc_gateway::{EventEnvelope, EventSubscriptionPort, EventTopic, RpcGatewayError};
use std::sync::Arc;
use std::time::Duration;
use table_service::SeatPrivateEvent;
use tokio::sync::oneshot;

pub async fn publish_pending_outbox_once<R, E>(
    repo: &R,
    event_bus: &E,
    limit: usize,
) -> Result<usize, String>
where
    R: AuditRepository + ?Sized,
    E: EventSubscriptionPort,
{
    let records = repo
        .fetch_pending_outbox_events(limit)
        .await
        .map_err(audit_err)?;
    let mut published = 0usize;

    for record in records {
        if let Some(envelope) = to_event_envelope(&record.topic, record.payload_json.clone())? {
            let _matched_subscribers = event_bus.publish(envelope).await.map_err(rpc_err)?;
            repo.mark_outbox_event_delivered(&record.outbox_event_id, Utc::now())
                .await
                .map_err(audit_err)?;
            published = published.saturating_add(1);
        }
    }

    Ok(published)
}

pub fn spawn_outbox_publisher_loop<R, E>(
    repo: Arc<R>,
    event_bus: Arc<E>,
    poll_interval: Duration,
    batch_limit: usize,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()>
where
    R: AuditRepository + ?Sized + 'static,
    E: EventSubscriptionPort + 'static,
{
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(poll_interval);
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    break;
                }
                _ = ticker.tick() => {
                    if let Err(err) = publish_pending_outbox_once(repo.as_ref(), event_bus.as_ref(), batch_limit).await {
                        tracing::warn!(error = %err, "outbox publisher loop iteration failed");
                    }
                }
            }
        }
    })
}

fn to_event_envelope(
    topic: &str,
    payload_json: serde_json::Value,
) -> Result<Option<EventEnvelope>, String> {
    match topic {
        "hand.events" => {
            let hand_event: HandEvent =
                serde_json::from_value(payload_json.clone()).map_err(|e| e.to_string())?;
            Ok(Some(EventEnvelope {
                topic: EventTopic::HandEvents,
                room_id: hand_event.room_id,
                hand_id: Some(hand_event.hand_id),
                seat_id: hand_event_seat_id(&hand_event.kind),
                event_name: hand_event_name(&hand_event.kind).to_string(),
                payload: payload_json,
            }))
        }
        "seat.events" => {
            let seat_event: SeatPrivateEvent =
                serde_json::from_value(payload_json.clone()).map_err(|e| e.to_string())?;
            Ok(Some(EventEnvelope {
                topic: EventTopic::SeatEvents,
                room_id: seat_event.room_id,
                hand_id: Some(seat_event.hand_id),
                seat_id: Some(seat_event.seat_id),
                event_name: seat_event.event_name,
                payload: seat_event.payload,
            }))
        }
        _ => Ok(None),
    }
}

fn hand_event_name(kind: &HandEventKind) -> &'static str {
    match kind {
        HandEventKind::HandStarted => "hand_started",
        HandEventKind::TurnStarted { .. } => "turn_started",
        HandEventKind::ActionAccepted(_) => "action_accepted",
        HandEventKind::StreetChanged { .. } => "street_changed",
        HandEventKind::PotUpdated { .. } => "pot_updated",
        HandEventKind::HandClosed => "hand_closed",
    }
}

fn hand_event_seat_id(kind: &HandEventKind) -> Option<u8> {
    match kind {
        HandEventKind::TurnStarted { seat_id } => Some(*seat_id),
        HandEventKind::ActionAccepted(action) => Some(action.seat_id),
        _ => None,
    }
}

fn audit_err(err: AuditStoreError) -> String {
    err.to_string()
}

fn rpc_err(err: RpcGatewayError) -> String {
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit_event_sink::AuditRoomEventSink;
    use audit_store::{InMemoryAuditRepository, OutboxEventRecord};
    use poker_domain::{ActionType, Chips, HandId, PlayerAction, RoomId, TraceId};
    use rpc_gateway::{InMemoryEventBus, SubscribeRequest};
    use std::sync::Arc;
    use table_service::{SeatPrivateEvent, spawn_room_actor_with_sink};
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn publishes_hand_events_from_outbox_to_event_bus() {
        let repo = InMemoryAuditRepository::default();
        let bus = InMemoryEventBus::new();
        let room_id = RoomId::new();
        let hand_id = HandId::new();

        let _ = bus
            .subscribe(SubscribeRequest {
                topic: EventTopic::HandEvents,
                room_id,
                hand_id: Some(hand_id),
                seat_id: None,
            })
            .await
            .expect("subscribe");

        let hand_event = HandEvent {
            room_id,
            hand_id,
            event_seq: 1,
            trace_id: TraceId::new(),
            occurred_at: Utc::now(),
            kind: HandEventKind::ActionAccepted(PlayerAction {
                room_id,
                hand_id,
                action_seq: 1,
                seat_id: 1,
                action_type: ActionType::Call,
                amount: Some(Chips(50)),
            }),
        };

        repo.insert_outbox_event(&OutboxEventRecord {
            outbox_event_id: "evt1".to_string(),
            topic: "hand.events".to_string(),
            partition_key: room_id.0.to_string(),
            payload_json: serde_json::to_value(&hand_event).expect("serialize"),
            status: "pending".to_string(),
            attempts: 0,
            available_at: Utc::now(),
            delivered_at: None,
            created_at: Utc::now(),
        })
        .await
        .expect("insert");

        let published = publish_pending_outbox_once(&repo, &bus, 10)
            .await
            .expect("publish");
        assert_eq!(published, 1);
        assert_eq!(bus.published_count(), 1);

        let pending = repo.fetch_pending_outbox_events(10).await.expect("fetch");
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn room_events_flow_to_outbox_then_event_bus() {
        let repo = Arc::new(InMemoryAuditRepository::default());
        let sink = Arc::new(AuditRoomEventSink::new(repo.clone()));
        let bus = InMemoryEventBus::new();
        let room_id = RoomId::new();

        let room = spawn_room_actor_with_sink(room_id, 32, sink);
        room.join(0).await.expect("join");
        room.bind_address(0, "cfx:seat".to_string())
            .await
            .expect("bind addr");
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
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        let snapshot = room.get_state().await.expect("state").expect("snapshot");
        let _ = bus
            .subscribe(SubscribeRequest {
                topic: EventTopic::HandEvents,
                room_id,
                hand_id: Some(snapshot.hand_id),
                seat_id: None,
            })
            .await
            .expect("subscribe");

        let published = publish_pending_outbox_once(repo.as_ref(), &bus, 100)
            .await
            .expect("publish");
        assert!(published >= 2);
        assert!(bus.published_count() >= 2);
    }

    #[tokio::test]
    async fn outbox_publisher_loop_drains_events_until_shutdown() {
        let repo = Arc::new(InMemoryAuditRepository::default());
        let bus = Arc::new(InMemoryEventBus::new());
        let room_id = RoomId::new();
        let hand_id = HandId::new();
        let _ = bus
            .subscribe(SubscribeRequest {
                topic: EventTopic::HandEvents,
                room_id,
                hand_id: Some(hand_id),
                seat_id: None,
            })
            .await
            .expect("subscribe");

        let hand_event = HandEvent {
            room_id,
            hand_id,
            event_seq: 1,
            trace_id: TraceId::new(),
            occurred_at: Utc::now(),
            kind: HandEventKind::HandStarted,
        };
        repo.insert_outbox_event(&OutboxEventRecord {
            outbox_event_id: "evt-loop".to_string(),
            topic: "hand.events".to_string(),
            partition_key: room_id.0.to_string(),
            payload_json: serde_json::to_value(&hand_event).expect("serialize"),
            status: "pending".to_string(),
            attempts: 0,
            available_at: Utc::now(),
            delivered_at: None,
            created_at: Utc::now(),
        })
        .await
        .expect("insert");

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handle = spawn_outbox_publisher_loop(
            repo.clone(),
            bus.clone(),
            Duration::from_millis(10),
            10,
            shutdown_rx,
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = shutdown_tx.send(());
        let _ = handle.await;

        assert_eq!(bus.published_count(), 1);
        let pending = repo.fetch_pending_outbox_events(10).await.expect("fetch");
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn publishes_seat_private_events_from_outbox_to_event_bus() {
        let repo = InMemoryAuditRepository::default();
        let bus = InMemoryEventBus::new();
        let room_id = RoomId::new();
        let hand_id = HandId::new();
        let seat_id = 1_u8;

        let _ = bus
            .subscribe(SubscribeRequest {
                topic: EventTopic::SeatEvents,
                room_id,
                hand_id: Some(hand_id),
                seat_id: Some(seat_id),
            })
            .await
            .expect("subscribe");

        let seat_event = SeatPrivateEvent {
            room_id,
            hand_id,
            seat_id,
            event_name: "hole_cards_dealt".to_string(),
            payload: serde_json::json!({"ciphertext_b64":"abc"}),
        };

        repo.insert_outbox_event(&OutboxEventRecord {
            outbox_event_id: "seat-evt1".to_string(),
            topic: "seat.events".to_string(),
            partition_key: format!("{}:{seat_id}", room_id.0),
            payload_json: serde_json::to_value(&seat_event).expect("serialize"),
            status: "pending".to_string(),
            attempts: 0,
            available_at: Utc::now(),
            delivered_at: None,
            created_at: Utc::now(),
        })
        .await
        .expect("insert");

        let published = publish_pending_outbox_once(&repo, &bus, 10)
            .await
            .expect("publish");
        assert_eq!(published, 1);
        assert_eq!(bus.published_count(), 1);
    }
}
