# 设计方案：结算 / 已实现盈亏 → 日亏熔断接线（P0-RISK-001）

- **状态**：Draft v1，**待独立 AI / 伯森先生审批**（未授权前不得实施）
- **作者**：OpenCode（工程实施者）
- **日期**：2026-09-17
- **关联**：`MASTER_TASKS.md` G1-001、`BUG_TRACKER.md` P0-RISK-001、`audits/round_04/CONSENSUS_ROUND_04.md`
- **依赖接口变更**：可能（取决于结算来源选择，见 Q1）。若需新增 RPC/字段，须先审批。

---

## 1. 问题（现状与证据）

日内亏损熔断**两端都未接通**：

1. **Rust**：`DailyLossLimit::report_pnl`（`fast/risk_engine.rs:555`）唯一可达路径是 `RiskEngine::post_check`（`:1049` → `:1063`），而 `post_check` **生产零调用**（grep 仅测试）。`on_fill`（`:1088-1097`）明确不更新 PnL。→ `BreachLevel` 恒为 `Normal`，`check_gates`（`:986-1005`）的 `Restricted/Halted` 永不触发。
2. **Rust PnL 来源缺失**：已实现盈亏由 `FillLedger::settle`（`fast/fill_records.rs:374`）/ `settle_by_token`（`:401`）在结算时写出，但这两个方法**生产无调用**（grep 仅测试）。`MarketEventPoller::mark_settled`（`fast/market_events.rs`）会触发 `settle_by_token`，但：
   - 只有 `MARKET_EVENTS_ENABLED=1` 时才构造 poller（`bin/server.rs`，G2 已修可用性）；
   - `mark_settled` 本身**无生产调用者**——结算事件从何而来未接线；
   - `consume_trade_events` 只消费 `MarketEvent::Trade`，`MarketEvent::Settlement` 被丢弃。
3. **Rust→Python 状态失真**：`RiskStateInternal.daily_pnl` / `daily_circuit_breaker` 无写入路径（`grpc/service.rs:194-235`），`get_risk_state` 恒返回 0/false。
4. **Python 侧同样失效**：`risk_gate.py:107-114` 的 `_check_daily_loss` 读 `state.daily_pnl`，而该值来自 Rust 回传（`converter.py:166`）——恒 0 → 不触发。

**后果**：日内亏损无上限（P0，资金安全）。

---

## 2. 目标 / 非目标

**目标**
- 建立「结算 → 已实现盈亏 → `DailyLossLimit` → `Halted/Restricted`」闭环。
- 使 Rust `RiskStateInternal.daily_pnl` / `daily_circuit_breaker` 反映 `RiskEngine` 权威状态，并回传 Python。
- 结算记账幂等、防重复计 PnL。

**非目标**
- 不改 `DailyLossLimit` 阈值/分级逻辑（Risk Model 红线）。
- 不改滑点/手续费口径（`DECISIONS.md` C-001）。
- 不新增策略层信号。

---

## 3. 关键设计决策（需审批）

### Q1. 结算事件的权威来源？（决定是否需接口变更）

| 选项 | 机制 | 优点 | 缺点 |
|------|------|------|------|
| A. **UMA 预言机** | 复用 `fast/uma_monitor.rs`（`poll()` 产出 `PriceSettledEvent{identifier,price,...}`，`uma_monitor.rs:165`），后台轮询 → 映射 token → outcome | 链上权威、已有实现 | `identifier` 到 `token_id` 的映射需建立（D 的开放问题）；需 RPC |
| B. **Gamma / market_discovery 结算态** | 读 `market_discovery` 的 `closed`/`outcome_prices`（`models.py`） | 数据源已用 | 依赖外部 API；引入新的定时任务 |
| C. **CLOB 市场结算接口** | 轮询 CLOB 市场 resolved 状态 | 与 CLOB 同源 | 需确认端点/字段 |
| D. **Python 推送结算结果（新增 RPC）** | 策略层判定结算后推 `{token_id, outcome}` 给 Rust | 控制集中 | 接口变更；信任边界外移 |

**建议**：优先 **A**（链上权威、复用 `uma_monitor`），映射问题单列；若 A 的 identifier 映射成本过高，退 **B**。

### Q2. 日亏口径与重置
- 口径：当天「已实现盈亏」总和（不含未实现浮盈浮亏）。是否包含手续费（当前 `fee_rate_bps` 默认 0，`C-001`）？
- 重置：UTC 日切（`last_reset_date` 字段已存在）。重置触发点：结算/成交路径检查日期变化，还是后台定时（`bin/server.rs` 30s 循环）？

### Q3. 双端熔断口径统一
Python `risk_gate._check_daily_loss` 已有本地阈值（`daily_loss_limit`）。是：
- (a) 以 Rust 为唯一权威（Python 只消费 `daily_circuit_breaker` / `daily_pnl`）；还是
- (b) 两层各自判定（可能阈值不一致，需统一配置）。

**建议 (a)**，避免双阈值漂移。

---

## 4. 方案（推荐 A + Q2 UTC 日切 + Q3(a)）

### 4.1 Rust 新增结算应用入口（`grpc/service.rs`）

新增方法（不触碰 Risk Model 阈值）：

```
async fn apply_settlement(&self, token_id: &str, outcome: f64) -> SettlementOutcome
```

流程：
1. **幂等门**：以 `token_id` 为键维护 `settled_tokens`（或复用 `market_events.settled_markets` 语义），重复结算直接返回，避免重复计 PnL。
2. **写账簿**：`let n = self.fill_ledger.settle_by_token(token_id, outcome);` —— 内部仅把 `Filled` 记录转 `Settled` 并写 `pnl`，天然幂等（`fill_records.rs:401-420`）。
3. **汇总 PnL**：取本次结算新增/该 token 的已实现 PnL 之和 `delta_pnl`。
   - 需注意 `settle_by_token` 返回的是「处理条数」，不直接返回 PnL 增量。建议新增只读聚合（`FillLedger::pnl_for_token(token_id)`）或让 `settle_by_token` 返回 `(count, pnl_delta)`——**属 `fill_records.rs` 改动，须列入 Allowed**。
4. **上报风控**：`let level = self.risk_engine.as_ref().unwrap().report_pnl(delta_pnl);`（需 `RiskEngine` 暴露 `report_pnl` 转发；`report_pnl` 目前是 `DailyLossLimit` 的方法，`RiskEngine` 未转发——**需新增转发方法，属 `risk_engine.rs` 改动**）。
5. **同步可观测**：更新 `RiskStateInternal.daily_pnl` 与 `daily_circuit_breaker`（由 `level` 映射），供 `get_risk_state` 回传。

> 若为避免动 `risk_engine.rs`/`fill_records.rs`，可改为 `ExecutionServiceImpl` 自行累计 PnL 并调用「新增的 RiskEngine 转发 getter/setter」。无论如何，**本任务必然需要向 `MASTER_TASKS` 追加 `risk_engine.rs` / `fill_records.rs` 的有限 Allowed**（仅新增转发/聚合，不改阈值与判定）。

### 4.2 结算事件来源（若选 A）

- 在 `bin/server.rs` 后台任务（可复用现有 30s 循环或新循环，如 60s）：
  - `uma_monitor.poll()` → 对 `PriceSettledEvent`，用「identifier→token_id」映射（需建立，见 Q4）→ `service.apply_settlement(token_id, outcome_from_price)`。
  - outcome：`price == 1e18`（或 UMA 二进制结算语义）→ 1.0 / 0.0。需核对 UMA OO 价格编码。
- 备选：`MarketEventPoller` 产出 `MarketEvent::Settlement`（当前被丢弃），改为在消费循环处理 → 但结算事件来源仍需上游（UMA/CLOB）。

### 4.3 `get_risk_state` 权威化（同时关闭报告 P2「字段脱节」）

- `to_proto` 时 `daily_pnl` / `daily_circuit_breaker` 从 `RiskEngine` 权威快照取（若采用 4.1.5 的同步，则读 `RiskStateInternal` 亦可，但需保证写入路径唯一）。
- `manual_halt`：设计是否由 `TradingGate::halt` 联动写入（当前 `TradingGate` 是独立闩锁）。建议保持独立并明确文档，或增加同步（Q5）。

---

## 5. 数据流 / 时序（选 A）

```
UMA OO (链上) ──poll──► UmaMonitor.poll() ──PriceSettledEvent──► identifier→token_id 映射
                                                                        │
                                                                        ▼
                                       ExecutionServiceImpl.apply_settlement(token_id, outcome)
                                         ├─ 幂等门（token 已结算？→ 返回）
                                         ├─ FillLedger.settle_by_token → (count, pnl_delta)
                                         ├─ RiskEngine.report_pnl(pnl_delta) → BreachLevel
                                         └─ 同步 RiskStateInternal.daily_pnl / circuit_breaker
                                                                        │
                                                    ┌───────────────────┴───────────────────┐
                                                    ▼                                       ▼
                                      check_gates(Halted→全拒,                   GetRiskState 回传
                                      Restricted→仅平仓)                          daily_pnl/breaker
                                                                                            │
                                                                                            ▼
                                                                      Python risk_gate._check_daily_loss
```

---

## 6. 幂等 / 并发 / 一致性

- **幂等**：`apply_settlement` 以 token 为幂等键；`settle_by_token` 只改 `Filled→Settled`，重复调用不重复写 pnl。
- **并发**：结算后台任务与 `on_market_trade`/`apply_recovery` 可能并发访问 `FillLedger`；均已用 `std::sync::RwLock` 且无锁跨 await，保持现有模式。
- **一致性**：`daily_pnl` 累计须来自「已实现 PnL 增量」，不可用「当日总仓位市值」等口径，避免双重计算。

---

## 7. 测试计划

**Rust**
- `test_apply_settlement_updates_daily_pnl`：造 FillLedger 已成交记录 → `apply_settlement(outcome=0)`（亏损）→ `daily_pnl` 下降、`report_pnl` 返回预期 `BreachLevel`。
- `test_apply_settlement_idempotent`：同 token 二次结算 → PnL 不重复累加。
- `test_daily_loss_halts_after_threshold_integration`（G1 Required Test）：累计亏损达 HALT 阈值 → `check_and_reserve` 拒单（Buy 全拒 / Restricted 仅平仓）。
- `test_get_risk_state_reports_authoritative_pnl`：回传 `daily_pnl` / `daily_circuit_breaker` 与 `RiskEngine` 一致。
- 鉴别力：短路 `report_pnl` 后，前两个测试必 FAILED。

**Python**
- `risk_gate`：`daily_circuit_breaker=True` → 拒单；`daily_pnl` 低于阈值 → 拒单（现有逻辑，补测试）。

---

## 8. 影响面与风险

| 风险 | 说明 | 缓解 |
|------|------|------|
| UMA identifier→token_id 映射 | A 的核心难点，可能无现成数据 | 先调研；不成立则退 B（Gamma 结算态） |
| UMA 价格编码 | outcome 判定依赖 `PriceSettledEvent.price` 语义 | 查官方 UMA OO 约定 + 用已知结算样本校验 |
| 双重计 PnL | 结算路径与 `on_market_trade` 若都以 PnL 上报 | 明确「未实现不进 daily_pnl，结算才进」；幂等键 |
| PnL 与 FillLedger 口径 | `settle_by_token` 目前不返回 PnL | 新增聚合/返回值（列入 Allowed） |
| 红线 | 必然触及 `risk_engine.rs` / `fill_records.rs` | 仅新增转发/聚合，不改阈值/判定，MASTER_TASKS 显式授权 |
| 日切重置 | 重置不当导致跨日累计或漏重置 | UTC 日切 + 明确触发点 + 测试 |

---

## 9. 与 G1-002 的关系

- 两者可共用「token→结算时间」与「结算事件」基础设施：G1-002 用 `end_date_utc`（**预计**结算时间）做开仓冻结；G1-001 用**实际**结算 outcome 做盈亏入账。建议**先 G1-002 建通道，再 G1-001 复用**。
- G1-002 的 Q1（未知结算时间 fail-closed）与本文 Q2（日亏口径）需一起定，避免策略层/Rust 层口径分裂。

---

## 10. 待审批清单（请逐条回复）

1. 是否批准本方案方向（结算→PnL→日亏熔断）？
2. Q1 结算事件来源选 A/B/C/D？（若选 D，需另出 RPC 设计）
3. Q2 日亏口径（是否含手续费）与重置触发点（UTC 日切；谁触发）？
4. Q3 双端熔断：以 Rust 为唯一权威（建议）还是各自判定？
5. 是否同意将 `fast/risk_engine.rs`、`fast/fill_records.rs` 加入本任务 Allowed Files（**仅新增 PnL 转发/聚合，不改阈值与判定逻辑**）？
6. Q4 UMA `identifier → token_id` 映射：是否有现成数据源？无则是否接受退到方案 B？

审批通过前，本方案不进入实施。
