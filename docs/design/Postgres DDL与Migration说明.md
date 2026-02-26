# Postgres DDL 与 Migration 说明（MVP）

本文档对应当前仓库的数据库迁移文件：

- `/Users/cooper/Programming/git_home/bot-holdem-platfrom/migrations/0001_init_mvp.sql`
- `/Users/cooper/Programming/git_home/bot-holdem-platfrom/migrations/0002_query_indexes.sql`

目标：

- 明确表结构边界（状态 / 审计 / 链上 / 结算 / 账本）
- 明确关键唯一约束与索引
- 说明当前 MVP 临时方案（如 `room_signing_keys` 密文入库）

## 1. Migration 概览

### `0001_init_mvp.sql`

初始化 MVP 主 schema，包含：

- 身份与会话
- 房间与座位
- 牌局快照
- 审计与领域事件
- 链上校验与结算
- 总账分录
- 房间签名密钥（MVP 临时方案）

### `0002_query_indexes.sql`

补充管理端与审计查询常用索引，重点服务：

- `/admin/audit/request/:id`
- `/admin/audit/tx/:tx_hash`
- `/admin/hands/:id/replay`
- 账本与异常账分页查询

## 2. 表边界（按模块）

## 2.1 身份与会话（Identity & Session）

### `agents`

用途：

- Bot/Agent 主体注册信息

关键字段：

- `agent_id`（PK）
- `name`
- `status`

### `agent_sessions`

用途：

- Agent 登录/会话记录（JSON-RPC 会话侧）

关键字段：

- `session_id`（PK）
- `agent_id`（FK -> `agents`）
- `auth_method`
- `session_status`
- `issued_at / expires_at / revoked_at`

索引：

- `idx_agent_sessions_agent_id`

### `seat_session_keys`

用途：

- 每个 seat 的地址、公钥绑定记录（发牌加密 + 请求验签）

关键字段：

- `room_id + seat_id + seat_address`
- `card_encrypt_pubkey`
- `request_verify_pubkey`
- `proof_signature`
- `status`

关键约束：

- `uq_seat_session_keys_active_seat`：同一 `room_id + seat_id` 同时只能有一个 `active`
- `uq_seat_session_keys_session_pubkey`：避免重复绑定

### `replay_nonces`（Redis fallback）

用途：

- 防重放持久化兜底（生产建议 Redis）

关键约束：

- `uq_replay_nonces_request_id`
- `uq_replay_nonces_agent_nonce`

## 2.2 房间与座位（Room & Seat）

### `rooms`

用途：

- 房间配置与运行参数（盲注、超时、rake）

关键字段：

- `dealer_address`
- `small_blind / big_blind`
- `min_buy_in`
- `max_players`
- `step_timeout_ms`
- `rake_bps`

### `room_signing_keys`（MVP 临时方案）

用途：

- 房间出账地址私钥密文托管（DB 存密文）

关键字段：

- `address`
- `encrypted_private_key`（`BYTEA`）
- `cipher_alg`
- `key_version`
- `kek_id`
- `nonce`
- `aad`
- `status`

关键约束：

- `uq_room_signing_keys_room_version`
- `uq_room_signing_keys_active_address`（仅 `status='active'`）

说明：

- 这是 MVP/内测方案，生产建议迁移到独立 signer/KMS。

### `room_seats`

用途：

- 房间座位实时状态与账务汇总字段

关键字段：

- `seat_status`
- `bound_address`
- `current_stack_virtual`
- `pending_credit / withdrawable_credit / unmatched_credit`

主键：

- `(room_id, seat_id)`

## 2.3 牌局快照（Hand State）

### `hands`

用途：

- 每手牌主快照（房间内 hand 序号、状态、街道、当前 action_seq、pot）

关键字段：

- `hand_no`
- `hand_status`
- `street`
- `acting_seat_id`
- `action_seq_next`
- `pot_total`
- `rake_accrued`

关键约束：

- `uq_hands_room_hand_no`

### `hand_seats`

用途：

- 每手牌的 seat 快照（地址、筹码快照、手牌密文引用、摊牌信息）

关键字段：

- `seat_address`
- `status_in_hand`
- `committed_this_hand`
- `hole_cards_cipher_ref`
- `showdown_cards_masked`

主键：

- `(hand_id, seat_id)`

## 2.4 审计与领域事件（Audit / Domain Events）

### `audit_action_attempts`

用途：

- 每次请求/动作尝试的完整审计记录（验签、重放、校验、路由、业务结果）

关键字段：

- `request_id / request_nonce`
- `method / action_type`
- `request_payload_json`
- `signature_verify_result`
- `replay_check_result`
- `idempotency_check_result`
- `validation_result`
- `router_result`
- `business_result_code`
- `error_detail`

关键约束 / 索引：

- `uq_audit_action_attempts_request_id`
- `idx_audit_action_attempts_room_hand_action`

### `audit_behavior_events`

用途：

- 运行时行为日志（链上回调、超时自动 fold、结算提交/回执/人工介入、RPC 拒绝等）

关键字段：

- `event_kind`
- `event_source`
- `severity`
- `payload_json`
- `related_attempt_id / related_tx_hash`

索引：

- `idx_audit_behavior_events_room_hand_time`
- `idx_audit_behavior_events_related_tx_hash_time`（`0002`）
- `idx_audit_behavior_events_room_time`（`0002`）

### `hand_events`

用途：

- 牌局领域事件流（回放/可视化基础）

关键字段：

- `event_seq`
- `event_type`
- `event_payload_json`

关键约束 / 索引：

- `uq_hand_events_hand_seq`
- `idx_hand_events_room_hand_seq`
- `idx_hand_events_hand_created_at`（`0002`）

### `outbox_events`

用途：

- 事件推送 outbox（先落库后异步发布）

关键字段：

- `topic`
- `partition_key`
- `status`
- `attempts`
- `available_at / delivered_at`

索引：

- `idx_outbox_events_pending`

## 2.5 链上校验与结算（Chain / Settlement）

### `tx_verifications`

用途：

- 链上交易观测与校验结果（原始 tx + 分类结果）

关键字段：

- `tx_hash`
- `chain_id`
- `from_address / to_address / value`
- `tx_status`
- `confirmations`
- `verification_status`
- `failure_reason`
- `raw_tx_json`

关键约束 / 索引：

- `uq_tx_verifications_tx_hash`
- `idx_tx_verifications_status_verified_at`（`0002`）

### `tx_bindings`

用途：

- `game.act` 与 `tx_hash` 绑定关系（`hand_id + action_seq + tx_hash`）

关键字段：

- `room_id / hand_id / action_seq / seat_id`
- `expected_amount`
- `tx_hash`
- `binding_status`

关键约束：

- `uq_tx_bindings_hand_action_seq`
- `uq_tx_bindings_tx_hash`

### `exception_credits`

用途：

- 异常到账（late/unmatched 等）分类与人工处理队列

关键字段：

- `credit_kind`
- `amount`
- `status`
- `reason`

索引（`0002`）：

- `idx_exception_credits_room_updated_at`
- `idx_exception_credits_tx_hash`

### `settlement_plans`

用途：

- 每手牌结算计划（含 rake / payout_total / 分配 payload）

关键字段：

- `status`
- `rake_amount`
- `payout_total`
- `payload_json`

### `settlement_records`

用途：

- 结算执行记录（提交 tx、重试、失败原因、人工介入）

关键字段：

- `tx_hash`
- `settlement_status`
- `retry_count`
- `error_detail`

## 2.6 总账（Ledger）

### `ledger_entries`

用途：

- append-only 总账分录（入金、结算、rake、异常处理等）

关键字段：

- `entry_type`
- `asset_type`
- `amount`
- `direction`
- `account_scope`
- `related_tx_hash`
- `related_attempt_id`

索引：

- `idx_ledger_entries_room_hand_time`
- `idx_ledger_entries_room_created_at`（`0002`）
- `idx_ledger_entries_hand_created_at`（`0002`）

## 3. 当前实现已接入的关键写路径（代码层）

已在服务层/适配层中接入（至少内存仓储 + 多数有 Postgres 骨架）：

- `audit_action_attempts`
- `audit_behavior_events`
- `hand_events`
- `outbox_events`
- `tx_bindings`
- `tx_verifications`
- `exception_credits`
- `settlement_plans`
- `settlement_records`
- `ledger_entries`
- `room_signing_keys`（读取 + 解密路径）

## 4. 注意事项（MVP）

1. 枚举状态字段目前多为 `TEXT`
- 优点：迭代快
- 风险：状态字面量需要代码与数据一致
- 后续可演进为 PG enum 或加 `CHECK` 约束

2. 数额字段使用 `NUMERIC(39,0)`
- 对齐链上最小单位整数
- 避免浮点误差

3. `room_signing_keys` 为临时方案
- 私钥密文入库仅适用于 MVP/内测
- 生产应迁移到 signer/KMS

4. 事务边界仍需继续加强
- 当前仓储层和集成测试已覆盖多条关键路径
- Postgres 事务一致性集成测试仍建议继续补齐（尤其 outbox + hand_events + ledger_entries 联合写入）
