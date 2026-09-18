# docs/design/ — 设计提案（待审批）

本目录存放**接口/架构变更的设计提案**。提案在获得独立 AI 审查 + Human 批准前**不得实施**（`AGENTS.md` 红线）。

| 文档 | 关联任务 | 状态 | 需审批要点 |
|------|----------|------|-----------|
| [`update_settlement_times_rpc.md`](./update_settlement_times_rpc.md) | G1-002 / P0-RISK-002 | **Draft v2，待第二轮审查** | Unknown settlement fail-closed；atomic snapshot replacement；freshness；cleanup 生命周期；RPC 语义；`risk_engine.rs` 最小 Allowed |
| [`settlement_pnl_risk_wiring.md`](./settlement_pnl_risk_wiring.md) | G1-001 / P0-RISK-001 | **Draft v2，待第二轮审查** | settlement authority POC；identifier→token_id；outcome encoding；PnL/fee/UTC；并发幂等；Rust 唯一 authority |
| [`reserve_release_accounting.md`](./reserve_release_accounting.md) | G4 / P1-RISK-008 | **Draft v1，待审** | 预留释放额改按「成交增量 × 订单限价」；是否授权改 `on_fill`/`post_check` 记账接口；过期路径释放 |
| [`indoubt_order_reconciliation.md`](./indoubt_order_reconciliation.md) | G4 / P1-EXEC-009 | **Draft v1，待审** | in-doubt（Received）订单可见性 + fail-closed；远端匹配收敛；是否用 client order id；TTL/停新单范围 |
| [`REVIEW_CHECKLIST_DESIGN.md`](./REVIEW_CHECKLIST_DESIGN.md) | 通用 | 有效 | 独立审查核对项 |
| [`REVIEW_CHECKLIST_DESIGN_V2.md`](./REVIEW_CHECKLIST_DESIGN_V2.md) | G1-001/G1-002 v2 | **待第二轮审查** | 首轮 9+15 项逐条对照 + 需核验证据清单 + 待 Human 拍板项 |
| [`CONSENSUS_DESIGN_ROUND_01.md`](./CONSENSUS_DESIGN_ROUND_01.md) | 通用 | 已归档 | 第一轮独立审查结论（CHANGES_REQUESTED ×2） |

## 流程状态

```text
Draft v1 ──► Independent Review #1（CHANGES_REQUESTED ×2）
        ──► Draft v2（本次）──► Independent Review #2 ──► Human Approval
        ──► MASTER_TASKS 拆分（Allowed/Forbidden/Required Tests/Acceptance）──► Implementation
```

## 审查方式建议

1. 独立 AI 逐份核对「证据引用（file:line）是否属实」与「方案是否覆盖根因」。
2. 对每份文档末尾「待审批清单」逐条给出结论。
3. 结论写入 `audits/round_04/CONSENSUS_ROUND_04.md` 或 `docs/design/CONSENSUS_DESIGN_*.md`。
4. 批准后再拆分为 `MASTER_TASKS.md` 可实施条目。
