# docs/design/ — 设计提案（待审批）

本目录存放**接口/架构变更的设计提案**。提案在获得独立 AI 审查 + Human 批准前**不得实施**（`AGENTS.md` 红线）。

| 文档 | 关联任务 | 状态 | 需审批要点 |
|------|----------|------|-----------|
| [`update_settlement_times_rpc.md`](./update_settlement_times_rpc.md) | G1-002 / P0-RISK-002 | **Draft v3，待审查** | Unknown/STALE fail-closed 落在 `place_order::settlement_gate`（不动 TimeWindowGuard 窗口计算）；双 freshness；replace_all 原子语义；增长保护；Human Acceptance 冻结 |
| [`settlement_pnl_risk_wiring.md`](./settlement_pnl_risk_wiring.md) | G1-001 / P0-RISK-001 | **Draft v6，待审查（P0-2 POC BLOCKED）** | §5.6 重做：7 问（authority / daily_pnl=A replay projection / txn boundary / commit marker / crash 判定 / 唯一恢复模块 / I1–I3 证明）+ D1 SQLite / D2 WAL 协议；联动 §5.5/§6/§14 |
| [`reserve_release_accounting.md`](./reserve_release_accounting.md) | G4 / P1-RISK-008 | **Draft v1，待审** | 预留释放额改按「成交增量 × 订单限价」；是否授权改 `on_fill`/`post_check` 记账接口；过期路径释放 |
| [`indoubt_order_reconciliation.md`](./indoubt_order_reconciliation.md) | G4 / P1-EXEC-009 | **Draft v1，待审** | in-doubt（Received）订单可见性 + fail-closed；远端匹配收敛；是否用 client order id；TTL/停新单范围 |
| [`REVIEW_CHECKLIST_DESIGN.md`](./REVIEW_CHECKLIST_DESIGN.md) | 通用 | 有效 | 独立审查核对项 |
| [`REVIEW_CHECKLIST_DESIGN_V2.md`](./REVIEW_CHECKLIST_DESIGN_V2.md) | G1-001/G1-002 v2 | 已被 V3 清单取代 | 首轮 9+15 项逐条对照 + 需核验证据清单 + 待 Human 拍板项 |
| [`REVIEW_CHECKLIST_DESIGN_V3.md`](./REVIEW_CHECKLIST_DESIGN_V3.md) | **全部 4 份提案（当前版本）** | **待第三轮复审** | G1-002 v3 / G1-001 v3 / P1-RISK-008 v1 / P1-EXEC-009 v1 的统一复审 + 证据核验 + Human Acceptance 汇总 |
| [`REVIEW_CHECKLIST_DESIGN_V5.md`](./REVIEW_CHECKLIST_DESIGN_V5.md) | G1-001 v5 | **待复审（v4→v5 五处定点修改）** | Durable Txn Boundary / settlement_id 前置 / effect identity 比较 / late-settlement gate / Test A-B 断言 |
| [`CONSENSUS_DESIGN_ROUND_01.md`](./CONSENSUS_DESIGN_ROUND_01.md) | 通用 | 已归档 | 第一轮独立审查结论（CHANGES_REQUESTED ×2） |
| [`CONSENSUS_DESIGN_ROUND_05.md`](./CONSENSUS_DESIGN_ROUND_05.md) | G1-001 v5 | 自审记录（非独立批准） | 必改 1（§5.6 Durable Txn Boundary）= **CHANGES_REQUESTED**；其余 4 项挂起 |

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
