# 设计方案：结算 / 已实现盈亏 → 日亏熔断接线（P0-RISK-001）

- **状态**：**Draft v2** — 已按独立审查意见（CHANGES_REQUESTED）修改，**待第二轮独立审查**。未通过前不得实施。
- **作者**：OpenCode（工程实施者）
- **日期**：2026-09-17
- **关联**：`MASTER_TASKS.md` G1-001、`BUG_TRACKER.md` P0-RISK-001、`audits/round_04/CONSENSUS_ROUND_04.md`
- **依赖接口变更**：视 settlement authority 选型而定（见 §5）。
- **证据基线**：`file:line` 引用基于 commit `5504579`，**未经独立审查逐条确认**。

---

## 0. 独立审查意见 → 修改对照（v1 → v2）

| 审查要求 | 本文位置 | 处理 |
|----------|----------|------|
| 不得默认 UMA identifier→token_id 映射成立 | §5.1 | 改为 **POC 验证门禁**：未证明前不实施 |
| 提供 identifier→token_id 完整可验证数据链 | §5.1 / §11 | 新增 POC 任务与验证产物要求 |
| 核实 UMA outcome encoding | §5.2 | 未验证前**禁止**实施 `price→outcome` 转换 |
| realized PnL 是否含 fee | §7.1 | 冻结为 Human 必批项；基线=gross（不含 fee，因 C-001 默认 0） |
| 明确 UTC daily reset | §7.2 | 复用现有 `DailyLossLimit` UTC 日切，不另造 |
| 跨 UTC 日 settlement 归属 | §7.2 / §10 | PnL 归属**实际结算发生的 UTC 日** |
| Rust 唯一 Risk Authority（升级为 REQUIRED） | §6 | 明确 Rust 为唯一权威，Python 为 projection |
| `RiskStateInternal` 仅为 projection | §6 | 明确不自行累计 |
| 唯一 PnL delta 来源 | §8 | `FillLedger::settle_by_token` → `pnl_delta` 单点 |
| 并发 settlement 幂等（atomic claim） | §9 | 原子 claim/guard，禁止 `contains()+insert()` |
| concurrent duplicate settlement test | §10 | 新增 |
| conflicting outcome test | §10 | 新增 |
| cross-day settlement test | §10 | 新增 |
| `risk_engine.rs` / `fill_records.rs` 最小 Allowed | §12 | 仅转发/聚合，不改口径 |
| 禁止修改 DailyLoss/BreachLevel/Risk Model/strategy/funds-control | §12 | 明确列出 |
| Settlement 与 Fill 语义分离 | §13 | 保持两独立事件 |

---

## 1. 问题（现状与证据）

日内亏损熔断**两端都未接通**：

1. **Rust**：`DailyLossLimit::report_pnl`（`fast/risk_engine.rs:555`）唯一可达路径是 `RiskEngine::post_check`（`:1049`→`:1063`），而 `post_check` **生产零调用**。`on_fill`（`:1088-1097`）明确不更新 PnL → `BreachLevel` 恒 `Normal`，`check_gates`（`:986-1005`）永不触发。
2. **Rust PnL 来源缺失**：`FillLedger::settle`（`fast/fill_records.rs:374`）/ `settle_by_token`（`:401`）生产零调用；`MarketEventPoller::mark_settled` 无生产调用者；`consume_trade_events` 仅消费 `Trade`，`Settlement` 被丢弃。
3. **状态失真**：`RiskStateInternal.daily_pnl`/`daily_circuit_breaker` 无写入路径（`grpc/service.rs:194-235`），`get_risk_state` 恒 0/false。
4. **Python 同样失效**：`risk_gate.py:107-114` 读 `state.daily_pnl`（来自 Rust 回传）→ 恒 0。

**后果**：日内亏损无上限（P0，资金安全）。

---

## 2. 目标 / 非目标

**目标**
- 建立「实际结算 → realized PnL → `DailyLossLimit` → `Halted/Restricted`」闭环。
- **Rust 为唯一 Risk Authority**；`RiskStateInternal` / Python `risk_gate` 为 projection。
- 结算记账**并发幂等**、**防重复/防冲突**。

**非目标**
- 不改 `DailyLossLimit` 阈值/分级（Risk Model 红线）。
- 不改滑点/手续费口径（`C-001`）。
- 不改策略。

---

## 3. （合并入 §5）

---

## 4. 关键原则

> **宁可暂时没有 settlement PnL，也不能接受错误 settlement PnL。**（独立审查 §13）
> 任何「未经验证的数据映射」都不得进入 PnL 链路。

---

## 5. Settlement Authority（BLOCKER：先验证，后实施）

### 5.1 identifier → token_id 映射（未证明，必须先做 POC）

`fast/uma_monitor.rs` 的 `PriceSettledEvent` 携带 `identifier`（`:94-101`），但**尚无证据**能可靠映射到 Polymarket `token_id`。

**POC 门禁（实施前置任务，建议编号 G1-001A）**：
- 输入：真实 UMA OO `PriceSettledEvent`（至少 1 个已结算样本）。
- 输出（**必须产出可验证产物**）：
  1. `identifier` 的构成（如 `questionID`/ancillary 数据哈希）；
  2. `identifier ↔ condition_id ↔ token_id` 的确定性映射算法与数据源；
  3. 用**已知结算样本**回放验证映射与 outcome 正确。
- **未通过 POC 前，本方案不实施**（不得将 UMA 设为 authority）。

**若 POC 不成立**，退回候选 B：`market_discovery` 的 `closed`/`outcome_prices`（Gamma）作为结算 authority（需另评估权威性与时效）。候选 C/D（CLOB 结算态 / Python 推送）暂不采用。

### 5.2 UMA outcome encoding（未验证，禁止实施）

v1 的 `price == 1e18 → 1.0 / 0 → 0.0` **未经证据支持，禁止实施**。POC 必须给出：
```
真实 PriceSettledEvent.price 的编码
      → YES/NO outcome
      → 对应 token_id
```
并附样本验证。在得到证据前，`apply_settlement` 不接受任何 outcome 推断逻辑。

### 5.3 Authority 冻结

- 第二轮审查通过 + POC 成功 → 在 `MASTER_TASKS` 内冻结 authority 选型（A 或 B）后方可实施。
- authority 判定为「已确认结算」的**唯一来源**；G1-002 的 registry cleanup（`ACTUALLY_SETTLED`）消费该信号，但不自行推断。

---

## 6. Rust 为唯一 Risk Authority（REQUIRED）

```
Python: Market Discovery / Settlement data producer / State consumer（projection）
                     │
                     ▼
Rust:   Settlement accounting authority
        Realized PnL authority
        DailyLoss authority
        RiskGate authority
                     │
                     ▼
        RiskStateInternal = 权威状态的 projection（不自行累计）
```

- Python `risk_gate._check_daily_loss` 保留为 **local projection / 辅助校验**，**不是**第二个 Risk Model authority。
- `RiskStateInternal.daily_pnl/daily_circuit_breaker` 的值**只能**来自 `RiskEngine`/`DailyLossLimit` 的权威快照（单一来源），避免 `RiskEngine.daily_pnl ≠ RiskStateInternal.daily_pnl`。

---

## 7. 口径冻结（需 Human 批准）

### 7.1 realized PnL 与 fee
- **基线**：`realized PnL = gross realized PnL`（**不含手续费**），因 `fee_rate_bps` 默认 0（`C-001`）；手续费若产生，单独字段展示，不混入日亏口径。
- **必须由 Human 批准**（不得由 OpenCode 决定）：若业务要求 net-of-fee，则需同步定义 fee 的记账来源与默认值变更（涉及 C-001，属红线）。

### 7.2 UTC 日切与跨日归属
- **复用现有 `DailyLossLimit` 的 UTC 日切机制**，不另造第二套 daily reset。
- **跨 UTC 日归属规则**：realized PnL 归属**实际结算事件发生的 UTC 日**。
  - 例：Day1 成交、Day2 结算 → PnL 计 Day2。
  - 必须写入 Acceptance + 单测（§10 Cross-Day）。

---

## 8. 方案：结算记账与 PnL 上报

### 8.1 唯一 PnL delta 来源

```
FillLedger::settle_by_token(token_id, outcome)
        │  处理 Filled→Settled（幂等），并输出本次的 authoritative pnl_delta
        ▼
RiskEngine.report_pnl(pnl_delta)   ← 唯一 PnL delta 上报点
        ▼
DailyLossLimit → BreachLevel → check_gates（Halted/Restricted）
        ▼
RiskStateInternal（projection）← get_risk_state 回传
```

- `settle_by_token` 当前返回条数，**不返回 PnL delta**。修改要求：**只增加返回信息/聚合能力**（如返回 `(count, pnl_delta)` 或新增只读聚合 `pnl_for_token`），**不改原有 settlement accounting 口径**（§12）。
- 禁止在 `on_fill` 里塞 PnL（§13）。

### 8.2 daily_pnl 同步
- `RiskStateInternal.daily_pnl/daily_circuit_breaker` 由 `RiskEngine` 权威快照填充（projection），不自行累计。

---

## 9. 并发幂等（BLOCKER）

- 幂等键：`token_id`（假设一个 token 最终只结算一次）。
- **必须原子 claim/guard**，禁止 `contains()` + `insert()` 两步（并发 A/B 会各自看到 not-settled → 双 `report_pnl`）。
- 冲突 outcome：同一 token 第二次不同 outcome → **拒绝/忽略**，不产生第二次 PnL（§10 Test C）。
- 幂等门与 `report_pnl` 必须在同一临界语义内（无让渡窗口）。

---

## 10. 测试计划

**Rust**
- `test_apply_settlement_updates_daily_pnl`：结算亏损 → `daily_pnl` 下降、`report_pnl` 返回预期 `BreachLevel`。
- **`test_concurrent_duplicate_settlement`（新增，P0）**：N 并发 `apply_settlement(同 token)` → **恰好一次**记账/一次 `pnl_delta`。
- **`test_conflicting_duplicate_outcome`（新增）**：同 token 先 outcome=0 再 outcome=1 → 第二次被拒，**无第二次 PnL/反向 PnL**。
- **`test_cross_utc_day_settlement`（新增）**：Day1 fill、Day2 settle → PnL 归属 Day2（与日切机制对齐）。
- `test_daily_loss_halts_after_threshold_integration`（Required）：累计达 HALT → `check_and_reserve` 拒单。
- `test_get_risk_state_reports_authoritative_pnl`：回传与 `RiskEngine` 一致。
- 鉴别力：短路 `report_pnl` / 去掉原子 claim → 上述对应测试必 FAILED。

**Python**
- `risk_gate`：`daily_circuit_breaker=True` → 拒单；`daily_pnl` 低于阈值 → 拒单。

**POC**
- identifier→token_id 映射回放；outcome encoding 样本验证。

---

## 11. POC 产出清单（实施前置）

1. 至少 1 个真实已结算市场的 `PriceSettledEvent` 原始数据。
2. identifier 构成说明 + 到 token_id 的映射算法。
3. outcome encoding 结论 + 样本。
4. 映射与 encoding 的自动化验证用例（可离线回放）。

---

## 12. 权限边界（Allowed / Forbidden，最小化）

**Allowed（若最终需动）**
- `fast/risk_engine.rs` —— **仅限**：`report_pnl` 的转发/只读快照 getter（不改阈值、不改判定）。
- `fast/fill_records.rs` —— **仅限**：`settle_by_token` 增加返回信息 / 新增只读 PnL 聚合（不改 settlement accounting 口径）。
- `grpc/service.rs`（新增 `apply_settlement` + 回传 projection）
- `bin/server.rs`（后台结算轮询接线）
- authority 选型涉及的文件（UMA 映射或 Gamma），按选定方案另行最小化。

**Forbidden**
- 修改 `DailyLossLimit` 阈值 / `BreachLevel` 阈值；
- 修改 Risk Model；
- 修改 strategy；
- 修改 trading semantics；
- 修改 funds-control semantics；
- 在 `on_fill` 中产生 PnL（Fill 与 Settlement 语义必须分离）；
- 未通过 POC 就实施 UMA 映射 / outcome 转换。

---

## 13. Settlement 与 Fill 语义分离（REQUIRED）

```
Fill Event      → Position Update
Settlement Event → Realized PnL → DailyLossLimit
```
两个独立事件；**不得**把成交价格直接解释成已实现盈亏；**不得**为让 `post_check` 有调用而污染 `on_fill`。

---

## 14. 与 G1-002 的关系

共享 token 生命周期基础设施，但**业务语义分离**（详见 `update_settlement_times_rpc.md` §12）：
- G1-002：预计结算时间（`end_date_utc`）→ 禁开仓。
- G1-001：实际结算事件 + outcome → realized PnL → 日亏熔断。
- 不得合并为单一「settlement 状态」。

---

## 15. 待 Human 拍板的开放问题

1. settlement authority：先做 POC，接受 A（UMA）作为候选；POC 失败退 B（Gamma）？（是/否）
2. realized PnL 是否含 fee？基线=不含（gross），是否批准？
3. 跨日归属=实际结算 UTC 日（批准？）
4. Rust 唯一 authority（批准？）
5. 是否同意 §12 的 `risk_engine.rs`/`fill_records.rs` 最小 Allowed（仅转发/聚合）？
6. `BreachLevel` 的 Restricted 行为是否维持「仅平仓」（不改阈值，仅确认语义）？

---

## 16. 结论

Draft v2 已落实独立审查的全部 Required Changes（§0 对照）。核心前置为 **§5 POC**：在 identifier→token_id 映射与 outcome encoding 得到证据前，**不进入实施**（LIVE = NO-GO 维持）。请进行第二轮独立审查 + Human 批准。
