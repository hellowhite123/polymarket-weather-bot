# 设计方案：结算 / 已实现盈亏 → 日亏熔断接线（P0-RISK-001）— Draft v3

- **状态**：**Draft v3** — 已按 G1-001 V3 整改任务（P0-1/P0-2/P0-3 + P1）修订，**待独立审查**。
- **作者**：OpenCode（工程实施者）
- **日期**：2026-09-17
- **关联**：`MASTER_TASKS.md` G1-001、`BUG_TRACKER.md` P0-RISK-001、`docs/design/CONSENSUS_DESIGN_ROUND_01.md`
- **证据基线**：commit `8f22902`（`file:line` 未提交独立核验）。
- **本阶段仅设计 + POC 规划**；提交后 **STOP**，不进入生产实现。

> **诚实声明（关键）**：P0-2 的 POC（UMA `identifier→token_id` 映射、outcome encoding、settlement identity）在本环境**尚未完成**（无链上原始事件 fixture / 外网）。因此 **P0-2 仍未关闭，生产实现被阻断**。V3 冻结其余可冻结语义，并给出 POC 的精确产出要求。**不得以“显然合理/UMA 应为 0/1”作为证据。**

---

## 0. V2 → V3 修改摘要

| 整改项 | 问题 | V3 处理 |
|--------|------|---------|
| **P0-1** PnL 权威语义 | V2 仅写“gross”未冻结数学定义 | 以**现有 `FillLedger` 记账为权威**（`compute_pnl` + `settle_by_token`），逐项冻结（§3） |
| **P0-1（更正）** gross/net | V2 称 gross/excludes fee，**与代码不符** | 现有公式为 `… − fee`（**net**）。更正为**以现有 net 公式为准**，fee 逻辑不新增（§3.3） |
| **P0-2** 结算身份/粒度 | 假设“token 只结算一次”未证明 | 明确 `settlement_id` 幂等键 + 映射/编码 **POC 门禁（未完成→阻断）**（§4） |
| **P0-3** Crash 安全 | claim→report 非事务，crash 丢/重 | 冻结**持久化 SettlementRecord 状态机** + 启动重放（§5/§6） |
| **P0-4** report_pnl 幂等 | 仅靠 FillLedger claim | 冻结 `report_pnl(settlement_id, pnl_delta, settlement_ts)` 幂等语义（§6.3） |
| **P1** 模块边界 | 未冻结 | §7 |
| **P1** settlement timestamp 入链 | `mark_settled` 用 `now_secs()` | 冻结以**权威 settlement UTC 时间**入账与分桶（§8） |
| **P1** UTC DailyLoss | 依赖现有机制 | §9（现有 `DailyLossLimit` + `date_util::today_utc()`） |
| **P1** Restart Recovery | 缺失 | §10 |
| **P1** 测试规格 | 缺并发/冲突/crash | §12 |
| **P1** Production call chain | 缺失 | §13 |
| **P1** Allowed/Forbidden | 缺失 | §14 |
| **P1** Human Acceptance | 缺失 | §15 |

---

## 1. Root Cause（现状与证据）

1. **日亏熔断未接通**：`DailyLossLimit::report_pnl`（`fast/risk_engine.rs:555-594`）唯一可达路径是 `RiskEngine::post_check`（`:1049-1063`），而 `post_check` **生产零调用**；`on_fill`（`:1088-1097`）明确不更新 PnL。`RiskEngine.daily_loss` 为私有字段（`:889`），**无对外 `report_pnl`/`daily_pnl` 转发**。
2. **PnL 来源未接线**：`FillLedger::settle`（`fill_records.rs:374-394`）/ `settle_by_token`（`:401-422`）生产无调用（仅有 `service.rs` 的**未被调用**包装 `settle_token` `:334-338`）。`MarketEventPoller::mark_settled` 无生产调用者；`consume_trade_events` 仅消费 `Trade`，`Settlement` 被丢弃。
3. **状态失真**：`RiskStateInternal.daily_pnl`（`grpc/service.rs:194-235`）无写入路径；`get_risk_state` 恒 0/false。
4. **UMA 映射缺失**：`uma_monitor.rs` 能解出 `identifier`（`:266,295`），但注释明确「未知的结算请求（**需外部映射 token_id**）」（`:119`）；`PriceSettledEvent.price` 为原始字符串，**无 outcome 解码**。
5. **无持久化**：`FillLedger` 为纯内存（grep 无 persist/load/save），进程重启即丢失；`OrderStore` 亦然（R2-P2 已承认）。→ 直接威胁 P0-3 crash safety。

**后果**：日亏无上限（资金安全）；且若草率接线，crash 会造成 PnL 丢失或重复。

---

## 2. 目标 / 非目标

**目标**
- 建立「实际结算 → 现有权威 PnL → `DailyLossLimit`」闭环，**Exactly-Once（可重启）**。
- 冻结 PnL 数学/幂等/时间/边界语义；Rust 为唯一 Risk Authority。
- 明确 POC 门禁，未通过不实施。

**非目标**
- **不重新设计 PnL 算法**；不新增第二套 accounting。
- 不改 `DailyLossLimit` 阈值/分级语义、Risk Model、strategy、funds-control。
- 不引入 fee 新逻辑（见 §3.3）。
- 不把 G1-002（预计结算时间）与 G1-001 合并。

---

## 3. P0-1：Realized PnL 权威语义（冻结）

### 3.1 Accounting Authority

> **权威 = 现有 `FillLedger` 的记账实现**：`compute_pnl`（`fill_records.rs:291-297`）+ `settle_by_token`（`:401-422`）。G1-001 只做「取权威 `pnl_delta` → `report_pnl`」，**不新增算法**。

### 3.2 冻结的数学定义（逐项）

| 项 | 冻结定义 | 代码依据 |
|----|----------|----------|
| settlement value | 该 token 的最终结算价 `outcome ∈ {0.0, 1.0}`（二元市场） | `compute_pnl` 参数 `settlement` |
| acquisition cost | 每笔 fill 的 `filled_price × size`（按笔，不合并后再算） | `settle_by_token` 逐 fill 计算 |
| BUY PnL | `size × (settlement − filled_price) − fee` | `:293` |
| SELL PnL | `size × (filled_price − settlement) − fee` | `:294` |
| multi-fill | **逐笔** `compute_pnl` 后求和；订单级 = 该订单各 fill 盈亏之和 | `:405-420`；测试 `test_ledger_multi_fill_settle_by_token`（`:832`，30×(1−0.5)+20×(1−0.6)=23） |
| partial fill | 仅 **已成交（`status==Filled`）的 fill** 参与；未成交量无 `FillRecord`，**不产生 PnL** | `:406` 过滤 `FillStatus::Filled` |
| unsettled position | 非 `Filled`（未成交/已取消）不计 | `:406` |
| settled position | 结算后 `status==Settled`，二次调用跳过 → 天然不重复 | `:406`；`mark_settled` `:151` |
| YES/NO | 以 **该 token 自身** 的 outcome 表达（DEC-0001/0002 token 路由不变）；PnL 以 shares×价差计，不涉及方向翻转 | `compute_pnl` + 路由决策 |
| settlement 后剩余数量 | 结算只改变 `Filled→Settled` 与写 `pnl`，不改 qty；无「剩余未结算」概念（未成交即不入账） | `:405-421` |
| fee | **含 fee（net）**，见 §3.3 | `:293-294` |

### 3.3 Fee 语义（**更正 V2**）

- 现有权威公式为 `… − fee`，即 **net realized PnL（扣除手续费）**。V2 的“gross/excludes fee”**错误**，V3 予以更正。
- `fee` 来源为 `FillRecord.fee`（`mark_filled` `:140-148`），当前 `fee_rate_bps` 默认 0（`DECISIONS.md` C-001）→ 实际等于 gross。
- **G1-001 不新增、不修改 fee 逻辑**；fee accounting 超出本任务范围（HA-002 必批）。
- 若将来要改 fee 口径，属独立决策，不得在本任务实现中夹带。

### 3.4 唯一 `pnl_delta` 来源（消除“三套计算”）

- 禁止：`FillLedger.calculate_pnl()` / `RiskEngine.calculate_pnl()` / `Python.calculate_pnl()` 并存。
- 现状：`settle_by_token` 返回 **count**，不返回 delta。**冻结要求**：在同一临界区内计算并返回**权威 `pnl_delta`**（如新增 `settle_by_token_with_pnl(token_id, outcome, settlement_ts) -> (usize, f64)`，或在同一锁内回读），**不得在别处重算**。
- `RiskEngine`、`RiskStateInternal`、Python 只**消费**该 delta。

---

## 4. P0-2：Settlement Identity / Event Granularity（POC 门禁，未关闭）

### 4.1 现状

- `PriceSettledEvent { tx_hash, block_number, requester, identifier, timestamp, price }`（`uma_monitor.rs:94-101`）；`identifier` 由 topic 解出（`:266,295`）。
- **无** `identifier → condition_id → token_id` 映射（`:119` 注释承认）。
- `price` 为字符串，无 outcome 解码。

### 4.2 POC 门禁（**未完成 → 禁止实现**）

| POC | 目标 | 必需产出 |
|-----|------|----------|
| **POC-1** Mapping | 证明 `PriceSettledEvent.identifier → condition_id → token_id`（确定性、可回放） | 原始 log fixture、ABI/event 解码、映射算法、自动化测试 |
| **POC-2** Encoding | 证明 `raw price → YES/NO/application outcome`（禁止猜 1e18/0） | 原始事件、解码、期望结果、自动化测试 |
| **POC-3** Identity | 证明稳定 `settlement_id`（restart/duplicate/replay 不变） | 键定义 + 唯一性论证 + 测试 |

> **当前状态：POC-1/2/3 = NOT DONE（缺链上原始事件/外网）。** 本 V3 只冻结“先证明后实施”的规则；不可用假设替代证据。

### 4.3 幂等键（冻结语义，值待 POC）

- **Accounting scope**：`token_id`（因 `FillLedger` 以 token 聚合结算）。
- **Identity/幂等键**：`settlement_id`（建议 = `hash(chain_id, tx_hash, log_index)`，交易级唯一），要求：
  - **全局唯一**：由链上 (tx_hash, log_index) 决定；
  - **跨重启存活**：写入持久化记录（§5）；
  - **冲突可检测**：同 `settlement_id` 不同 `outcome` → `reject + alert`，不产生第二笔 PnL。
- 禁止继续以“token 只结算一次”为设计依据；token 可因同一市场多次事件到达而重复投递，必须靠 `settlement_id` 去重。

---

## 5. P0-3：Crash-Safe Exactly-Once 模型（冻结）

### 5.1 为什么锁不够

`Mutex/RwLock/AtomicHashSet` 仅在**单进程内存**内有效；`FillLedger` 无持久化（§1.5）。因此 `claim → crash` 或 `report_pnl → crash` 会导致**丢 PnL** 或**重复 PnL**。

### 5.2 持久化 SettlementRecord（冻结）

```
SettlementRecord {
    settlement_id: String,      // 幂等键（§4.3）
    token_id: String,
    outcome: f64,               // 0.0 / 1.0
    settlement_ts: u64,         // 权威结算时间（UTC 秒）
    pnl_delta: Option<f64>,     // 计算后写入
    status: SettlementStatus,   // 状态机
    updated_at: u64,
}
enum SettlementStatus { PENDING, ACCOUNTING, ACCOUNTED, FAILED }
```

- **存储**：append-only WAL（JSONL）或 SQLite（实现选型后定；`R2-P2` 已提出持久化诉求）。**新增依赖/存储属架构变更，须授权。**
- **状态机**：`PENDING → ACCOUNTING → ACCOUNTED`；失败可 `FAILED`（可重试）。

### 5.3 必须覆盖的 Crash Case（冻结）

| Case | 时序 | 重启后要求 |
|------|------|-----------|
| A | `PENDING`（claim 前/后 crash） | 可继续处理 |
| B | `ACCOUNTING`，`report_pnl` 前 crash | 重新提交 PnL，**不丢失** |
| C | `report_pnl` 后、标 `ACCOUNTED` 前 crash | **不得**产生第二笔 PnL（`report_pnl` 幂等，§6.3） |
| D | `ACCOUNTED` + 重复 settlement | 无额外 PnL |
| E | `ACCOUNTED(outcome=A)` + 重复 `(outcome=B)` | reject + alert，**无第二笔 PnL** |

### 5.4 顺序（冻结）

```
① durable insert SettlementRecord(status=PENDING, settlement_id, ...)   ← 先落盘
② status=ACCOUNTING
③ FillLedger 结算 → 权威 pnl_delta（同锁内）
④ RiskEngine.report_pnl(settlement_id, pnl_delta, settlement_ts)（幂等）
⑤ status=ACCOUNTED（落盘）
```
- 任一步 crash：按 §5.3 由启动重放恢复。
- `pnl_delta` 可重算但**必须确定**（同一结算输入 → 同一结果）；重算需在同一权威路径，不引入第二套。

---

## 6. P0-4：RiskEngine PnL API 幂等（冻结语义）

### 6.1 接口（冻结）

```
RiskEngine::report_pnl(settlement_id: &str, pnl_delta: f64, settlement_ts: u64) -> BreachLevel
```

- `RiskEngine` 增加**最小转发**（当前无对外 `report_pnl`/`daily_pnl`）——属 Allowed 的最小 API（§14）。

### 6.2 幂等语义

- 同一 `settlement_id` + 同一 `pnl_delta` → **no duplicate effect**（幂等）。
- 同一 `settlement_id` + 不同 `pnl_delta` → `reject + alert`，不产生第二笔。
- 幂等保证**不能只依赖** `FillLedger.claim`。

### 6.3 实现提案（V3 冻结语义，技术细节待审）

- 由 `RiskEngine`（或其持久化层，与 §5 共用 settlement record）按 `settlement_id` 去重；记录 `ACCOUNTED` 后再调用 `DailyLossLimit::report_pnl` 累加。
- `DailyLossLimit::report_pnl` 现有签名保持（不改阈值/分级）；日期分桶见 §9。

---

## 7. P1-1：模块责任边界（冻结）

| 模块 | 职责 | 不得 |
|------|------|------|
| **FillLedger** | fills、结算会计、**权威 `pnl_delta`**、settlement 幂等记录 | 不接 Risk 阈值 |
| **RiskEngine** | 消费权威 `pnl_delta` → `DailyLossLimit` → `BreachLevel` → risk gate | **不重算** settlement accounting |
| **RiskStateInternal** | **projection only**（从 RiskEngine 权威快照填充） | 不做第二本账 |
| **Python** | read projection / gate consumer | 不自算 realized PnL |

---

## 8. P1-2：Settlement Timestamp 进入 PnL 链（冻结）

- 现状：`FillRecord::mark_settled` / `OrderFill::mark_settled` 用 `now_secs()`（处理时刻，`fill_records.rs:151-155,251-255`）——**不是权威结算时间**。
- **冻结**：以**权威 settlement UTC 时间**（`settlement_ts`）作为 `settled_at` 与 DailyLoss 分桶依据。
- 需修改：`settle_by_token` / `mark_settled` 接受 `settlement_ts`（**fill_records 记账入参变更**，Allowed，见 §14）。
- 这样解决「23:59:59 结算 / 00:00:03 处理」的跨日错桶（HA-003）。

---

## 9. P1-3：UTC DailyLoss 语义（复用现有机制）

- 复用现有 `DailyLossLimit` + `date_util::today_utc()`（`risk_engine.rs:542-552`），**不另造**。
- **分桶 = 权威 settlement UTC 日**：`report_pnl` 收到的 `settlement_ts → UTC date → bucket`。
- 若 settlement 属**过去 UTC 日**（迟到结算）：更新该日 bucket，**不影响当日 gate**；并 `alert`（保守，不静默计入当日）。
- 边界测试：`23:59:59` / `00:00:00`（§12.5）。

---

## 10. P1-4：Restart Recovery（冻结）

启动时：

```
① 读取持久化 SettlementRecord
② 对 status ∈ {PENDING, ACCOUNTING}：进入重放（重算→report_pnl→ACCOUNTED）
③ 对 status == ACCOUNTED：跳过（幂等，绝不二次记账）
④ 重建/加载 FillLedger 持久化（若采用）以支撑重算；否则以 SettlementRecord 为权威重放源
⑤ 恢复后 TradingGate 行为遵循既有 FAIL-CLOSED（对账未通过不 Ready）
```

- 需区分 `received / claimed / pnl calculated / pnl reported / fully accounted`（由状态机表达）。
- 重放必须**确定性**且**幂等**。

---

## 11. POC Gate（阻断条件）

**在 POC-1/2/3（§4.2）完成前，禁止任何生产 `Settlement → PnL` 实现。** 允许先行（且仅）编写 POC 代码/fixture/decoder/mapping 测试，产物随 V3 一起提交。

---

## 12. 测试规格（implementation acceptance）

| # | 测试 | 期望 |
|---|------|------|
| 12.1 | `test_concurrent_duplicate_settlement` | N 并发同 `settlement_id`+同 outcome → **恰好一笔 PnL** |
| 12.2 | `test_conflicting_duplicate_outcome` | 同 id 先 A 后 B → reject，**无第二笔 PnL** |
| 12.3 | `test_multiple_fills_settle_pnl` | 多 fill 聚合 = 现有权威（引用 `test_ledger_multi_fill_settle_by_token` 语义） |
| 12.4 | `test_partial_fill_pnl` | requested/filled/unfilled/settled 的 PnL 会计正确 |
| 12.5 | `test_cross_utc_day_bucket` | `23:59:59` vs `00:00:00` 分桶正确 |
| 12.6 | `test_crash_before_report_replays` | ACCOUNTING + crash → 重启后 exactly-once 入账 |
| 12.7 | `test_crash_after_report_no_duplicate` | report 后 crash → 重启无重复 |
| 12.8 | `test_restart_duplicate_event` | ACCOUNTED + 重启 + 重复事件 → 额外 PnL = 0 |
| 12.9 | `test_daily_loss_circuit_breaker_integration` | 生产链：Settlement→FillLedger→pnl_delta→RiskEngine→DailyLossLimit→BreachLevel→`check_gates` 拒单 |
| 12.10 | `test_risk_state_projection_daily_pnl` | `RiskEngine.daily_pnl == RiskStateInternal.daily_pnl == Python 可见`（消除“引擎有、投影 0”） |

- **鉴别力**：短路 `report_pnl` 幂等 / 去掉持久化重放 → 对应 12.1/12.6/12.7 必 FAILED。

---

## 13. Production Call Chain（每个箭头需 code + test + production-path 证据）

```
UMA / Settlement Authority
  → event ingestion                       (uma_monitor.rs:165 poll；生产接线待定)
  → settlement event decoding             (uma_monitor.rs:261-310；outcome 解码=POC-2)
  → identifier / condition mapping        (POC-1，当前缺失)
  → token_id mapping                       (POC-1)
  → settlement identity (settlement_id)   (POC-3)
  → FillLedger settlement accounting       (fill_records.rs:401-422)
  → authoritative pnl_delta                (需新增返回，§3.4)
  → RiskEngine PnL API (幂等)              (§6)
  → DailyLossLimit                          (risk_engine.rs:555-594)
  → BreachLevel                             (risk_engine.rs:568-577)
  → check_gates                             (risk_engine.rs:986-1005)
  → RiskStateInternal projection            (service.rs:194-235；需接线)
  → Python risk_gate                        (risk_gate.py:107-114)
```

> 现状：第 2~5 环缺失/未证（POC）；第 6 环返回值待加；第 7、10、12 环待接线。**均未实现。**

---

## 14. Allowed / Forbidden Scope（冻结）

**Allowed（V3 阶段）**
- 仅 `docs/design/settlement_pnl_risk_wiring.md` 及 **POC 代码/fixture/decoder/mapping 测试**。

**Allowed（V3 获批后的 implementation，需另行授权）**
- `fast/fill_records.rs`（`settle_by_token` 返回 `pnl_delta` + 接受 `settlement_ts`；**不改既有 accounting 口径**）
- `fast/risk_engine.rs`（新增 `report_pnl(settlement_id, pnl_delta, settlement_ts)` 转发 + `daily_pnl` getter；**不改阈值/分级/窗口**）
- `grpc/service.rs`（settlement 应用入口 + projection 接线）
- event consumer / server（UMA 轮询接线）
- 持久化（SettlementRecord/WAL）相关新文件（**架构变更需授权**）

**Forbidden**
- 修改 Risk Model / `DailyLossLimit` 阈值 / `BreachLevel` 语义 / 窗口逻辑 / position limit / 集中度 / Gas / 熔断判定；
- 修改 strategy / forecast / entry / exit / pricing；
- 修改 fund limits / wallet controls / capital allocation / withdrawal；
- **重新设计 PnL accounting**（除非独立审核明确授权）；
- 在 POC-1/2/3 完成前实现生产 settlement→PnL。

---

## 15. Human Acceptance Items

| ID | 项 | 基线/建议 | 状态 |
|----|----|-----------|------|
| **HA-001** | Settlement Authority（A=链上/UMA，B=应用侧来源） | 待 POC 后定；**倾向 A** | **待 Human 冻结** |
| **HA-002** | Realized PnL gross / net | 权威=现有 **net（`− fee`）**；默认 fee=0。是否接受该定义？ | **待 Human 冻结** |
| **HA-003** | Cross-day 归属 | 使用**权威 settlement UTC 时间**入 DailyLoss bucket | **待 Human 确认** |
| **HA-004** | Rust 唯一 Risk Authority | 是（Python 仅 projection/consumer） | **待 Human 确认** |
| **HA-005** | 迟到结算（过去 UTC 日）处理 | 入对应历史 bucket + alert，不影响当日 gate | **待 Human 确认** |
| **HA-006** | 持久化方案（WAL/SQLite）与新增依赖 | 需授权（架构变更） | **待 Human 批准** |
| **HA-007** | POC 环境（提供 UMA 原始事件 fixture / RPC 访问） | 阻塞 P0-2 | **待 Human 提供** |

---

## 16. 完成度自评（对照任务 §18）

| 项 | 状态 |
|----|------|
| PnL 数学/权威冻结 | ✅（以现有代码+测试） |
| gross/net fee 冻结 | ✅（更正为 net，HA-002 待批） |
| multi-fill / partial-fill 语义 | ✅（§3.2，引用现有测试） |
| UMA event mapping POC | ❌ **未完成（缺 fixture/外网）** |
| UMA outcome encoding POC | ❌ **未完成** |
| settlement identity 冻结 | ⚠️ 语义冻结，键值待 POC-3 |
| idempotency key | ✅ 语义（§4.3） |
| crash-safe 状态机 | ✅ 语义（§5） |
| report_pnl 幂等 | ✅ 语义（§6） |
| settlement timestamp 入链 | ✅（§8） |
| UTC DailyLoss | ✅（§9） |
| restart recovery | ✅（§10） |
| 并发/冲突/多 fill/partial/crash/cross-day 测试规格 | ✅（§12） |
| production call chain | ✅（§13） |
| Allowed/Forbidden | ✅（§14） |
| Human Acceptance | ✅（§15） |

> **结论**：P0-1、P0-3 语义已冻结；**P0-2 仍开（POC 未完成）**。按任务 §22，G1-001 **仍不能进入下一阶段**。V3 提交后 **STOP**，等待独立审查；请提供 POC-1/2/3 所需数据/环境（HA-007）。
