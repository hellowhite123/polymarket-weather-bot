# docs/design/ — 设计提案（待审批）

本目录存放**接口/架构变更的设计提案**。提案在获得独立 AI 审查 + Human 批准前**不得实施**（`AGENTS.md` 红线：加重大架构 / 接口变更）。

| 文档 | 关联任务 | 状态 | 需审批要点 |
|------|----------|------|-----------|
| [`update_settlement_times_rpc.md`](./update_settlement_times_rpc.md) | G1-002 / P0-RISK-002 | Draft v1，待审 | proto 新增 RPC；未知结算时间 fail-closed 口径 |
| [`settlement_pnl_risk_wiring.md`](./settlement_pnl_risk_wiring.md) | G1-001 / P0-RISK-001 | Draft v1，待审 | 结算事件来源（UMA/Gamma）；日亏口径；是否放开 `risk_engine.rs`/`fill_records.rs` 有限 Allowed |

## 审查方式建议

1. 独立 AI 逐份核对「证据引用（file:line）是否属实」与「方案是否覆盖根因」。
2. 对每份文档末尾「待审批清单」逐条给出结论。
3. 结论写入 `audits/round_04/CONSENSUS_ROUND_04.md`（或新建 `CONSENSUS_DESIGN_*.md`）。
4. 批准后再拆分为 `MASTER_TASKS.md` 的可实施条目（含 Allowed / Forbidden / Required Tests / Acceptance）。
