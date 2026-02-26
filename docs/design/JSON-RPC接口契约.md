# JSON-RPC 接口契约文档（MVP / 当前实现）

本文档描述当前 Rust 服务端（`jsonrpsee`）已实现的 JSON-RPC 接口契约，供 `agent-sdk`、`headless-agent-client`、测试脚本和第三方 Bot 对接使用。

## 1. 协议约定

- 传输：
  - HTTP JSON-RPC（命令调用）
  - WebSocket JSON-RPC（命令调用 + 订阅）
- 版本：`jsonrpc = "2.0"`
- 编码：`UTF-8 JSON`

标准请求示例：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "game.get_state",
  "params": {
    "room_id": "9d8ec8a0-7a7c-42fc-bf4c-0b5b0f7744c6",
    "hand_id": null,
    "seat_id": 0
  }
}
```

标准响应（业务层 envelope）：

```json
{
  "ok": true,
  "data": {},
  "error": null
}
```

失败响应（业务层 envelope）：

```json
{
  "ok": false,
  "data": null,
  "error": {
    "code": "request_invalid",
    "message": "request expired"
  }
}
```

## 2. 通用签名字段（变更类请求）

以下请求当前要求带 `request_meta`（至少）：

- `room.bind_address`
- `room.bind_session_keys`
- `room.ready`
- `game.act`

`request_meta` 结构：

```json
{
  "request_id": "f2a0d9f5-77f3-4f1e-9c1f-7b6a53f0f4f4",
  "request_nonce": "nonce-123",
  "request_ts": "2026-02-25T10:00:00Z",
  "request_expiry_ms": 30000,
  "signature_pubkey_id": "bot-key-1",
  "signature": "hex_or_base64_signature"
}
```

说明：

- 当前 `rpc-gateway` 已校验时间窗（过期请求拒绝）。
- `room.bind_session_keys` 已校验绑定声明 proof（Ed25519）。
- 拒绝类请求审计（过期/签名缺失/无效 proof）已接入 `jsonrpsee` 方法入口并写审计行为事件。
- 重放拒绝审计仍待与防重放存储联动后补全。

## 3. 方法列表（当前实现）

### 3.1 `room.list`

请求：

```json
{
  "include_inactive": false
}
```

响应 `data`：

```json
[
  {
    "room_id": "uuid",
    "status": "active"
  }
]
```

### 3.2 `room.bind_address`

请求：

```json
{
  "room_id": "uuid",
  "seat_id": 0,
  "seat_address": "cfx:.... 或 0x....",
  "request_meta": {}
}
```

响应 `data`：`null`

### 3.3 `room.bind_session_keys`

请求：

```json
{
  "room_id": "uuid",
  "seat_id": 0,
  "seat_address": "cfx:.... 或 0x....",
  "card_encrypt_pubkey": "hex/base64",
  "request_verify_pubkey": "hex/base64",
  "key_algo": "x25519+ed25519",
  "proof_signature": "hex/base64",
  "request_meta": {}
}
```

响应 `data`：`null`

说明：

- 服务端会构造 `SeatKeyBindingClaim` 并校验 `proof_signature`。

### 3.4 `room.ready`

请求：

```json
{
  "room_id": "uuid",
  "seat_id": 0,
  "request_meta": {}
}
```

响应 `data`：`null`

### 3.5 `game.get_state`

请求：

```json
{
  "room_id": "uuid",
  "hand_id": null,
  "seat_id": 0
}
```

响应 `data`：`HandSnapshot | null`

`HandSnapshot`（当前领域模型）：

```json
{
  "room_id": "uuid",
  "hand_id": "uuid",
  "hand_no": 1,
  "status": "running",
  "street": "preflop",
  "acting_seat_id": 0,
  "next_action_seq": 1,
  "pot_total": 0
}
```

说明：

- `status` / `street` / `action_type` 等领域枚举已固定为 `snake_case` 序列化。

### 3.6 `game.act`

请求：

```json
{
  "room_id": "uuid",
  "hand_id": "uuid",
  "action_seq": 3,
  "seat_id": 1,
  "action_type": "check",
  "amount": null,
  "tx_hash": null,
  "request_meta": {}
}
```

字段说明：

- `action_type`：`fold | check | call | raise_to | all_in`
- `amount`：JSON 整数（`u128`，最小单位）
- `tx_hash`：
  - `null`：链下动作直接执行
  - 非空：进入 `tx_bindings + pending action`，待链上 `matched` 回调才真正推进

响应 `data`：`null`

## 4. 订阅接口（控制面）

### 4.1 `subscribe.events`

说明：

- 这是“控制型”订阅接口，返回 `subscription_id`（业务层），由服务端内存事件总线维护。
- 不等同于 `jsonrpsee` 原生 WS subscription。

请求：

```json
{
  "topic": "hand_events",
  "room_id": "uuid",
  "hand_id": "uuid",
  "seat_id": null
}
```

`topic` 枚举：

- `public_room_events`
- `seat_events`
- `hand_events`

响应 `data`：

```json
"sub_xxxxx"
```

### 4.2 `unsubscribe.events`

请求：

```json
{
  "subscription_id": "sub_xxxxx"
}
```

响应 `data`：`null`

## 5. 原生 WS PubSub（`jsonrpsee register_subscription`）

### 5.1 `subscribe.events.native`

说明：

- 使用 `jsonrpsee` 原生订阅通道。
- 当前事件通知名为：`events`
- 取消订阅方法：`unsubscribe.events.native`

请求参数与 `subscribe.events` 相同：

```json
{
  "topic": "hand_events",
  "room_id": "uuid",
  "hand_id": "uuid",
  "seat_id": null
}
```

推送 payload（`EventEnvelope`）：

```json
{
  "topic": "hand_events",
  "room_id": "uuid",
  "hand_id": "uuid",
  "seat_id": null,
  "event_name": "hand_started",
  "payload": {
    "event_seq": 1
  }
}
```

实现约束：

- 慢订阅者会被断开（有界队列 + 队列满断开）。
- 已有连接级并发限制与固定窗口速率限制。

## 6. 错误与限制（当前实现）

业务层 `error.code` 来自 `platform_core::ErrorCode` 映射，常见包括：

- `request_invalid`
- `room_bind_address_failed`
- `room_bind_keys_failed`
- `room_ready_failed`
- `game_get_state_failed`
- `game_act_failed`
- `subscribe_failed`
- `unsubscribe_failed`

运行时限制（已实现）：

- 连接级并发限制（默认每连接 64）
- 连接级固定窗口速率限制（默认每秒 256）
- 慢订阅者断开

环境变量（`app-server`）：

- `RPC_MAX_CONCURRENT_PER_CONNECTION`
- `RPC_MAX_RPS_PER_CONNECTION`

## 7. 当前实现范围与后续扩展

已实现（MVP 主线）：

- `room.list / room.bind_address / room.bind_session_keys / room.ready`
- `game.get_state / game.act`
- `subscribe.events / unsubscribe.events`
- `subscribe.events.native / unsubscribe.events.native`

后续计划扩展：

- `game.get_legal_actions`
- `game.get_hand_history`
- `game.get_ledger`
- 更完整的鉴权/签名校验与重放拒绝审计联动
- 私有 seat 事件（密文 payload）完整对接到发牌流程
