# Headless Agent Client 使用说明

本文档说明仓库内 `headless-agent-client` 的定位和运行方式。

## 1. 定位

- `headless-agent-client` 是无 UI 的自动执行器。
- 负责把 `agent-sdk`、策略、钱包、审计日志串起来，执行 `prepare -> observe -> act`。

## 2. 主要模块

- `runtime.rs`
  - `prepare_seat`
  - `run_single_action_if_turn`
  - `run_first_available_action`
- `wallet_adapter.rs`
  - 钱包接口与 EVM JSON-RPC 适配
- `policy_adapter.rs`
  - 规则策略 / 预留 LLM 策略
- `client.rs`
  - 连接状态、订阅恢复、fallback 策略

## 3. 本地验证方式

### 3.1 玩法 + 链路 smoke

```bash
scripts/smoke_gameplay_e2e.sh
```

### 3.2 批量结算 smoke

```bash
scripts/smoke_batch_settlement_e2e.sh
```

### 3.3 单桌单玩家常驻客户端

新增可执行程序：`single_seat_client`

```bash
RPC_ENDPOINT=http://127.0.0.1:9000 \
WALLET_RPC_URL=http://127.0.0.1:8545 \
SEAT_ID=0 \
SEAT_ADDRESS=0x... \
ROOM_CHAIN_ADDRESS=0x... \
CARD_ENCRYPT_PUBKEY_HEX=<hex32bytes> \
CARD_ENCRYPT_SECRET_HEX=<hex32bytes_optional> \
POLICY_MODE=rule \
CHAIN_ID=31337 \
TX_VALUE=1 \
TICK_MS=800 \
cargo run -p headless-agent-client --bin single_seat_client
```

启用 `codex_cli` 决策：

```bash
POLICY_MODE=codex_cli \
CODEX_CLI_BIN=/Users/cooper/Programming/git_home/bot-holdem-platfrom/scripts/codex_decide_wrapper.py \
CODEX_CLI_ARGS="" \
POLICY_TIMEOUT_MS=1500 \
POLICY_CONTEXT_MAX_ENTRIES=16 \
cargo run -p headless-agent-client --bin single_seat_client
```

说明：

- `single_seat_client` 每次调用 `codex` CLI 都是新进程。
- 客户端会维护最近 N 步对局上下文（`POLICY_CONTEXT_MAX_ENTRIES`）并在每次决策时注入 prompt，降低“无状态会话”损耗。
- 推荐通过 wrapper 调 codex，确保输出总是严格 JSON 动作。
- `ROOM_ID` 现在是可选项：
  - 有值：使用指定房间；
  - 无值：自动调用 `room.list`，优先选择 `status=active` 房间，否则选择列表第一个房间。

wrapper 额外环境变量（可选）：

```bash
CODEX_WRAPPER_CMD=codex
CODEX_WRAPPER_ARGS=""
CODEX_WRAPPER_TIMEOUT_MS=1400
```

一键脚本（默认自动建房）：

```bash
scripts/run_single_seat_codex.py
```

说明：未提供 `ROOM_ID` 且 `room.list` 为空时，脚本会先调用 `room.create`，再启动客户端。

## 4. 关键配置

- RPC：
  - `http://127.0.0.1:9000`
- 签名与私牌：
  - `seat_address`（EVM 地址）
  - `card_encrypt_pubkey_hex`
  - `card_encrypt_secret_hex`（可选；提供后可解密 `game.get_private_payloads`）

## 5. 运行行为说明

- 当轮到本 seat 时，客户端会：
  1. 获取 `game.get_state`
  2. 获取 `game.get_legal_actions`
  3. 获取 `game.get_private_payloads`（并可选解密）
  4. 交给策略做决策
  5. 调钱包转账并提交 `game.act`

## 6. 现状边界

- 当前 `src/main.rs` 仍偏 demo 引导代码；生产路径建议通过 `runtime` API 组装自己的进程入口。
- `single_seat_client` 已提供单桌单玩家常驻运行入口，默认规则策略决策。
- `codex_cli` 模式要求 CLI 输出可解析 JSON 动作，建议通过外层脚本包装 codex 输出为严格 JSON。
- 已覆盖真实 JSON-RPC + Anvil 路径的测试与 smoke 脚本，可作为回归基线。
