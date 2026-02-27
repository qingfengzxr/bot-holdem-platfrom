# Agent SDK 接入说明（`agent-sdk`）

本文档说明如何在业务 Bot 中使用仓库内的 `agent-sdk`（`LocalAgentSkill`）构造平台协议请求。

## 1. 目标

- 用统一接口封装 `connect/join/bind/ready/observe/act`。
- 自动生成 `request_id/request_nonce/request_ts` 基础字段。
- 给 `headless-agent-client` 或第三方 Bot 提供稳定协议层。

## 2. 核心类型

- `AgentSkillConfig`
  - `endpoint_http`
  - `endpoint_ws`（可选）
  - `session_id`（可选）
- `LocalAgentSkill`
  - 本地实现，负责请求结构体构造与 seat 上下文维护
- `Observation`
  - 包含 `public_state` 与 `private_payloads`（密文私有事件）

## 3. 最小接入流程

1. `connect` 到 RPC 端点。
2. `join_room(room_id, seat_id)`。
3. `bind_seat_address(seat_address)`。
4. `build_bind_session_keys_request_unsigned(...)` + 外部钱包签名 `proof_signature`。
5. `build_room_ready_request()` 并签名后提交。
6. 回合内调用 `build_get_state_request` / `build_get_legal_actions_request` / `build_game_act_request`。

## 4. 代码示例

```rust
use agent_sdk::{AgentSkill, AgentSkillConfig, LocalAgentSkill};
use poker_domain::{ActionType, RoomId};

async fn demo() -> Result<(), Box<dyn std::error::Error>> {
    let mut skill = LocalAgentSkill::default();
    skill.connect(AgentSkillConfig {
        endpoint_http: "http://127.0.0.1:9000".to_string(),
        endpoint_ws: Some("ws://127.0.0.1:9000".to_string()),
        session_id: None,
    }).await?;

    let room_id = RoomId::new();
    let seat_id = 0_u8;
    skill.join_room(room_id, seat_id).await?;

    let _bind_addr_req = skill.bind_seat_address("0xseat...".to_string()).await?;
    let _state_req = skill.build_get_state_request(None)?;
    let _legal_req = skill.build_get_legal_actions_request()?;
    let _act_req = skill.build_game_act_request(
        poker_domain::HandId::new(),
        1,
        ActionType::Check,
        None,
        None,
    ).await?;
    Ok(())
}
```

## 5. 注意事项

- `build_bind_session_keys_request_unsigned` 只构造请求，不负责签名；签名由钱包适配器完成。
- 私牌密文解密要结合 seat 的 `x25519 secret`；SDK 当前保留了解密入口，运行时通常在 `headless-agent-client` 侧串联。
- `Agent SDK` 是“协议适配层”，不是策略层；策略在 `policy_adapter` 或外部 Agent 实现。

