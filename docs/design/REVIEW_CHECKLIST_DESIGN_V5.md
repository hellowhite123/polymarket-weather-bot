# 设计提案 V5 复审投喂清单（G1-001 五处定点修改）

> 供**独立审查 AI / Human** 使用。本轮仅复审 `settlement_pnl_risk_wiring.md` 的 **v4 → v5 五处定点修改**。
> 其余文档维持原版本，见 `docs/design/REVIEW_CHECKLIST_DESIGN_V3.md`。
> **OpenCode 为工程实施者，本清单不构成批准；结论不得由实施者自证。**

## 0. 复审输入

| 项 | 值 |
|----|----|
| 快照 commit | `0ba8a78` |
| 被审文档 | `docs/design/settlement_pnl_risk_wiring.md`（**v5**，+49/-6） |
| 上一版 | v4（`b890220`） |
| 产出 | `docs/design/CONSENSUS_DESIGN_ROUND_05.md`（建议） |
| 全局状态 | **LIVE = NO-GO**；P0-2 POC **BLOCKED** |

### 文档版本总览

| 文档 | 版本 | 本轮是否被审 |
|------|------|--------------|
| `settlement_pnl_risk_wiring.md` | **v5** | ✅ 本轮 |
| `update_settlement_times_rpc.md` | v3 | 否（见 V3 清单） |
| `reserve_release_accounting.md` | v1 | 否（见 V3 清单） |
| `indoubt_order_reconciliation.md` | v1 | 否（见 V3 清单） |

---

## 1. 五处定点修改逐条核对

### 必改 1（P0）— Durable Transaction Boundary
- **位置**：§5.6
- **要求**：明确 `SettlementRecord` + `AccountingEffect` + `RiskEngine.daily_pnl` 在 crash 下如何原子。
- **复审动作**：
  - [ ] 三者“同生效/同不生效”边界是否清晰；
  - [ ] Option D1（SQLite 事务 + `UNIQUE(settlement_id)`）是否可行、幂等由谁保证；
  - [ ] Option D2（WAL `prepare→effect→commit`）是否给出 **commit=权威生效点** 与崩溃重放规则；
  - [ ] `daily_pnl` 是否明确“可持久恢复”（加载或按 effect 重放）。
- **残余**：方案二选一未定 → **HA-006 冻结**。

### 必改 2（P0）— settlement_id 前置依赖
- **位置**：§4.5
- **要求**：POC 未完成 → id 未验证 → P0-4 只设计冻结 → 不得实现。
- **复审动作**：
  - [ ] 依赖链是否显式、是否明确“不得落地”。
- **残余**：解除依赖仅靠 **HA-007**。

### 必改 3（P1）— Effect Identity 比较
- **位置**：§6.4
- **要求**：same id + same canonical `pnl_delta` + same `settlement_ts` → no-op；任一不同 → conflict。
- **复审动作**：
  - [ ] 是否覆盖 `pnl_delta`/`settlement_ts`/`token_id`/`outcome` 全部字段；
  - [ ] conflict 是否明确 **不累加、不覆盖**；
  - [ ] canonical 是否给出确定性序列化/bit-exact 复现要求（禁止近似放行）。

### 必改 4（P1）— Late Settlement Gate
- **位置**：§10
- **要求**：无法正确入历史 bucket → `RETRYABLE/FAILED` → **TradingGate 明确处理**（非仅 alert）。
- **复审动作**：
  - [ ] 是否明确 `TradingGate = reconcile_complete(false,false)`（停新单）；
  - [ ] 是否有“不得错桶入当日 / 不得静默继续”的约束；
  - [ ] 是否标注涉 HA-005。

### 必改 5（P1）— Test A/B 断言
- **位置**：§14.6 / §14.7（+ `effect_count` 定义）
- **要求**：不仅验证最终 PnL，还须验证 `effect_count==1`、持久化 effect 存在、`status==ACCOUNTED`、restart replay==no-op。
- **复审动作**：
  - [ ] A/B 两测试断言是否齐全；
  - [ ] `effect_count` 定义是否唯一可判（=持久化 effect/commit 记录条数）。

---

## 2. 仍需关闭 / 阻断项

- **P0-2 UMA POC = BLOCKED**（无 RPC / fixture / 外网）：POC-1 映射、POC-2 encoding、POC-3 identity 均未完成。
  - 解除条件：**HA-007 提供 UMA 原始事件 fixture / RPC 环境**。
  - 未解除前：**禁止生产 settlement→PnL 实现**。
- 本轮五处修改不改变 P0-2 状态。

---

## 3. Human Acceptance（G1-001）

| ID | 项 | 状态 |
|----|----|------|
| HA-001 | Settlement Authority（A=链上/UMA，B=应用侧） | 待 Human |
| HA-002 | 确认 G1-001 **不改变现有 net（`compute_pnl`）口径** | 待确认 |
| HA-003 | Cross-day 使用 `settlement_ts` 的 UTC 日 | 待确认 |
| HA-004 | Rust 唯一 Risk Authority | 待确认 |
| HA-005 | 历史 bucket / 迟到结算（gate 行为，涉 Risk Model） | 待确认 |
| HA-006 | 持久化方案（SQLite/WAL）+ 新增依赖 | 待批准 |
| HA-007 | 提供 UMA 原始事件 fixture / RPC 环境 | **待提供** |

---

## 4. 复审结论模板（`CONSENSUS_DESIGN_ROUND_05.md`）

```markdown
| 必改项 | 结论 | 证据/位置 | 备注 |
|--------|------|-----------|------|
| 1 Durable Txn Boundary | PASS / CHANGES | §5.6 | D1/D2 选型 = HA-006 |
| 2 settlement_id 前置 | PASS / CHANGES | §4.5 | 阻断=HA-007 |
| 3 Effect Identity | PASS / CHANGES | §6.4 | canonical 定义 |
| 4 Late Settlement Gate | PASS / CHANGES | §10 | HA-005 |
| 5 Test A/B 断言 | PASS / CHANGES | §14.6/14.7 | effect_count |

整体：settlement_pnl_risk_wiring.md v5 = APPROVED / CHANGES_REQUESTED / REJECTED
P0-2 POC：BLOCKED（HA-007）
下一步：拆分 MASTER_TASKS（Allowed/Forbidden/Required Tests/Acceptance）
```

---

## 5. 停止条件

> v5 通过独立复审 + Human 批准（含 HA-006/007）前，**不实施任何生产代码**；P0-2 POC 未解除 BLOCKED 前，**禁止生产 settlement→PnL 实现**。**LIVE = NO-GO。**
