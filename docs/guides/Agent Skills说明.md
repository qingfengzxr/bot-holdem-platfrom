# Agent Skills 说明（项目内语境）

你说得对，这个本质上是“说明文档”。

在本项目里，`Agent Skills` 主要有两层含义：

## 1. 平台内 Skill（代码层）

- 指 `agent-sdk` 的 `AgentSkill` 接口能力：
  - `connect/join/bind/ready/observe/act`
- 这是 Bot 与平台交互的**代码契约**，用于运行时执行。

## 2. Codex/工具侧 Skill（文档/流程层）

- 指开发工具里可复用的工作流说明（例如画图、安装技能等）。
- 这是**研发效率工具**，不参与牌局协议或线上链路。

## 3. 建议边界

- 业务代码只依赖第 1 类（`agent-sdk` / `headless-agent-client`）。
- 第 2 类只用于研发协作，不应耦合进生产执行路径。

## 4. 如果要“新增一个 Agent Skill”

建议至少包含：

1. 输入：需要的状态与参数（room_id/seat_id/state/private payload）
2. 输出：动作决策（action_type/amount）与可审计理由
3. 错误策略：超时、重试、fallback
4. 安全要求：签名、nonce、幂等键
5. 测试：最小可复现实例与回归用例

这样它既能作为“说明”，也能直接指导落地实现。

## 5. 项目内已提供的 AI Agent 接入 Skill

- `/Users/cooper/Programming/git_home/bot-holdem-platfrom/docs/skills/ai-agent-integration/SKILL.md`

该 Skill 面向 AI Agent，约束了接入顺序、验收脚本与常见故障排查优先级，可直接复用。
