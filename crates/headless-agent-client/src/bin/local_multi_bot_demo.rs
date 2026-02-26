#![allow(unused_crate_dependencies)]

use anyhow::Result;
use poker_domain::{ActionType, HandId, HandStatus, PlayerAction, RoomId};
use table_service::spawn_room_actor;

fn choose_action(legal_actions: &[poker_domain::LegalAction]) -> PlayerAction {
    if legal_actions
        .iter()
        .any(|a| matches!(a.action_type, ActionType::Check))
    {
        PlayerAction {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            seat_id: 0,
            action_seq: 0,
            action_type: ActionType::Check,
            amount: None,
        }
    } else if legal_actions
        .iter()
        .any(|a| matches!(a.action_type, ActionType::Call))
    {
        PlayerAction {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            seat_id: 0,
            action_seq: 0,
            action_type: ActionType::Call,
            amount: None,
        }
    } else {
        PlayerAction {
            room_id: RoomId::new(),
            hand_id: HandId::new(),
            seat_id: 0,
            action_seq: 0,
            action_type: ActionType::Fold,
            amount: None,
        }
    }
}

async fn run_local_multi_bot_demo(max_steps: u32, print_logs: bool) -> Result<HandStatus> {
    let room_id = RoomId::new();
    let room = spawn_room_actor(room_id, 64);

    for seat_id in [0_u8, 1_u8] {
        room.join(seat_id).await.map_err(anyhow::Error::msg)?;
        room.bind_address(seat_id, format!("cfx:seat{seat_id}"))
            .await
            .map_err(anyhow::Error::msg)?;
        room.bind_session_keys(
            seat_id,
            format!("encpk-{seat_id}"),
            format!("sigpk-{seat_id}"),
            "x25519+evm".to_string(),
            "proof".to_string(),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    }

    room.ready(0).await.map_err(anyhow::Error::msg)?;
    room.ready(1).await.map_err(anyhow::Error::msg)?;

    let mut steps = 0_u32;
    loop {
        steps = steps.saturating_add(1);
        let snapshot = room
            .get_state()
            .await
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow::anyhow!("missing hand snapshot"))?;

        if print_logs {
            println!(
                "step={} status={:?} street={:?} acting={:?} action_seq={} pot={}",
                steps,
                snapshot.status,
                snapshot.street,
                snapshot.acting_seat_id,
                snapshot.next_action_seq,
                snapshot.pot_total.as_u128()
            );
        }

        match snapshot.status {
            HandStatus::Settled | HandStatus::Aborted => return Ok(snapshot.status),
            HandStatus::Showdown => {
                if print_logs {
                    println!("reached showdown; demo stops before reveal/settlement");
                }
                return Ok(snapshot.status);
            }
            _ => {}
        }

        let Some(seat_id) = snapshot.acting_seat_id else {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            continue;
        };
        let legal = room
            .get_legal_actions(seat_id)
            .await
            .map_err(anyhow::Error::msg)?;
        let mut action = choose_action(&legal);
        action.room_id = snapshot.room_id;
        action.hand_id = snapshot.hand_id;
        action.seat_id = seat_id;
        action.action_seq = snapshot.next_action_seq;

        if print_logs {
            println!(
                "seat={} legal={:?} choose={:?}",
                seat_id,
                legal.iter().map(|a| a.action_type).collect::<Vec<_>>(),
                action.action_type
            );
        }
        let _ = room.act(action).await.map_err(anyhow::Error::msg)?;

        if steps > max_steps {
            anyhow::bail!("demo exceeded step limit");
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    observability::init_tracing("headless-agent-local-multi-bot-demo");
    let _ = run_local_multi_bot_demo(64, true).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_multi_bot_demo_reaches_showdown_or_terminal_state() {
        let status = run_local_multi_bot_demo(64, false)
            .await
            .expect("demo should progress");
        assert!(matches!(
            status,
            HandStatus::Showdown | HandStatus::Settled | HandStatus::Aborted
        ));
    }
}
