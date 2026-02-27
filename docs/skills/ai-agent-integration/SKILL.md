# skill: ai-agent-integration

用于指导 AI Agent（Codex/自动化执行器）快速接入本项目的 JSON-RPC 与结算能力。

## 何时使用

当任务包含以下任一目标时使用本 skill：

- “接入 agent 到牌桌流程（bind/ready/act）”
- “让 agent 能读取私牌密文并决策”
- “验证链上动作或结算（direct/batch）”

## 必读上下文

1. 读取接口契约：
   - `/Users/cooper/Programming/git_home/bot-holdem-platfrom/docs/design/JSON-RPC接口契约.md`
2. 读取 SDK 使用方式：
   - `/Users/cooper/Programming/git_home/bot-holdem-platfrom/docs/guides/Agent SDK接入说明.md`
3. 读取 headless 运行路径：
   - `/Users/cooper/Programming/git_home/bot-holdem-platfrom/docs/guides/Headless Agent Client使用说明.md`

## 接入步骤（强约束顺序）

1. 准备 seat 身份与密钥
   - seat 地址（EVM）
   - `card_encrypt_pubkey_hex`
   - （可选）`card_encrypt_secret_hex` 用于解密私牌
2. 完成基础握手
   - `room.bind_address`
   - `room.bind_session_keys`（EVM proof）
   - `room.ready`
3. 回合执行循环
   - `game.get_state`
   - `game.get_legal_actions`
   - `game.get_private_payloads`
   - 策略决策
   - （若需要）先链上转账拿 `tx_hash`，再 `game.act`
4. 事件订阅（可选）
   - `subscribe.events.native` 订阅 `hand_events` / `seat_events`

## 结算模式策略

AI Agent 不直接发结算；只负责动作侧。结算由服务端自动结算 loop 控制：

- `batch`：设置 `SETTLEMENT_BATCH_CONTRACT_ADDRESS`
- `direct`：未设置 batch 地址但配置了钱包参数
- `disabled`：未配置钱包

## 验收检查清单

至少执行以下脚本并通过：

- `/Users/cooper/Programming/git_home/bot-holdem-platfrom/scripts/smoke_gameplay_e2e.sh`
- `/Users/cooper/Programming/git_home/bot-holdem-platfrom/scripts/smoke_batch_settlement_e2e.sh`

## 失败排查优先级

1. 签名与 proof（`room.bind_session_keys`）
2. 请求时间窗/nonce（`request_meta`）
3. 私牌解密参数（`card_encrypt_secret_hex`）
4. 链上 RPC 可用性与账户余额
5. batch 合约地址与部署网络不一致

