# R5-002 设计方案：MultiSource 建议仓位公式改造

- **状态**：**Draft v1** — 供独立审查（未授权实施）
- **作者**：OpenCode（工程实施者）
- **日期**：2026-09-18
- **关联**：`MASTER_TASKS.md` R5-002、`strategy_engine/strategies/multi_source.py`
- **证据基线**：commit `2914df1`（`file:line` 未经独立核验）

---

## 1. 要解决的问题

`MultiSourceStrategy._calculate_quantity`（`multi_source.py:414-429`）：

```python
base = abs(market_dev) * 100.0 * min(dev_sigma / 2.0, 2.0)
div_multiplier = 1.0 + min(divergence, 0.5)
base *= div_multiplier
return round(min(base, self.params.max_position_per_market), 2)
```

**缺陷**：
1. **与资本脱节**：不含 `base_capital` / `kelly_fraction`，`abs(market_dev)*100` 是硬编码任意系数 → 与账户资金无关。
2. **随市场偏差线性放大**：`base ∝ |market_dev|`，偏差越大仓位越大（只被 `max_position_per_market` 硬顶兜底），无风险预算观念。
3. **单位口径不明**：注释/其他策略暗示 shares，而 `max_position_per_market` 在 Rust 风控是 **USDC notional** 上限 → cap 与 quantity 单位可能不一致。
4. **与聚合器不一致**：聚合器 `PositionSizer`（Kelly，`position_sizer.py`）**会重算** `quantity`（`aggregator.py:301`），策略返回值基本是**顾问值**；当前实现既非 shares 口径的 Kelly，也非 notional。

**目标**：让 MultiSource 的 `quantity` 变为**受资本约束、随优势饱和（不随 market_dev 线性飙升）、单位明确**的建议仓位。

**非目标（R5-002 Forbidden）**：不改 `max_position_per_market` 硬顶语义；不改其他策略；不改聚合器 Kelly 实现；不改 Risk Model/阈值；不改交易语义/资金控制。

---

## 2. 需要冻结的 4 个决策点

### D1｜`quantity` 单位口径

| 方案 | 定义 | 优点 | 缺点 |
|------|------|------|------|
| **D1-A** | **shares**：`qty = (kelly_fraction × base_capital × edge) / cost`（cost=target_price；BUY_NO 用 1-price） | 与 `PositionSizer`/聚合器一致；`TradingSignal.quantity` 语义统一 | cap 需把 USDC 上限换算成 shares（`cap_shares = max_position_per_market / cost`） |
| **D1-B** | **USDC notional**：`notional = kelly_fraction × base_capital × edge`；`cap=min(notional, max_position_per_market)` | 与 `max_position_per_market` 直接可比、直观 | 与其他策略/聚合器的 shares 口径**不一致**；需明确下游如何消费 |

> 事实：聚合器重算 quantity，故无论选哪个，都需**同时明确聚合器是否改用策略量**（见 D5）。

### D2｜「价差优势系数」`edge` 公式（关键：同 dev_sigma 下不得随 market_dev 线性飙升）

| 方案 | 公式 | 特性 | 风险 |
|------|------|------|------|
| **D2-A（推荐）** | `edge = clamp(\|market_dev\| / max(\|dev_mu\|, eps), 0, edge_cap)` | 以历史均值 dev_mu 为标尺；有上界 `edge_cap` | `dev_mu≈0` 时依赖 eps；需定义 eps |
| **D2-B** | `edge = clamp((\|market_dev\| - μ0)/ (σ0), 0, edge_cap)`，其中 μ0/σ0 为**固定**基线（非滚动 market_dev 本身） | 去均值、按离散度归一；有界 | 需引入 μ0/σ0 参数 |
| **D2-C** | `edge = 1 - exp(-\|market_dev\| / dev_scale)`（饱和函数） | **天然饱和**，大偏差不再线性增长；无突变 | `dev_scale` 需标定；无上界到 1 需再乘 `edge_cap` |
| **D2-D** | **Kelly 胜率口径**：`f* = max(b·p − (1−p), 0)/b`，`p=fused_prob`、`b=(1−price)/price`；`edge=f*` | 与聚合器 Kelly 同源、经济意义明确 | 与「聚合器 Kelly」重复；R5-002 禁止改聚合器，但策略层复用需评估 |
| **D2-E** | **委托聚合器**：策略返回 0（或仅作提示），仓位完全由聚合器 `PositionSizer` 决定 | 单一权威、无重复实现 | 改变现有策略返回值语义，可能影响其他消费者/测试 |

**硬性约束（任一方案须满足）**：
- 单调非减于优势；
- **有上界**（`≤ kelly_fraction × base_capital × edge_cap`）；
- 优势为 0 时仓位为 0；
- 同 `dev_sigma` 下**不随 `market_dev` 线性飙升**。

### D3｜cap 语义

- `cap = min(建议仓位, max_position_per_market)`。
- 若 D1-A（shares）：`cap_shares = max_position_per_market / cost`（把 USDC 上限换算为 shares），或直接对 **notional** 做 cap 再转 shares。
- 需明确：cap 作用在 **USDC notional** 还是 **shares**；不得改 `max_position_per_market` 的 USDC 语义。

### D4｜参数来源

- 新增 `MultiSourceStrategyParams.kelly_fraction`（默认 0.25）、`base_capital`（默认 1000.0）、`edge_cap`、`eps`/`dev_scale`。
- 是否与聚合器 `.env` 的 `AGG_BASE_CAPITAL` / `AGG_KELLY_FRACTION` 对齐（避免两套资本口径漂移）？

### D5｜与聚合器的关系

聚合器（`aggregator.py:301`）用 `PositionSizer` 重算 quantity。需冻结：
- (a) 策略量为**顾问值**，仅要求 bounded/受资本约束（聚合器仍重算）；
- (b) 聚合器改用策略量（超出 R5-002 范围，需另开任务）；
- (c) 策略直接返回 0、由聚合器全权决定（D2-E）。

---

## 3. 候选组合（示例，供审查对比）

以 `base_capital=1000, kelly_fraction=0.25, edge_cap=2.0, max_position_per_market=1000(USDC)`：

| 方案 | edge | 建议（USDC） | shares@0.5 | 说明 |
|------|------|-------------|-----------|------|
| D1-B + D2-A | `min(\|dev\|/max(\|mu\|,eps),2)` | `250×edge` | — | 受资本约束、有界 |
| D1-A + D2-C | `1−exp(−\|dev\|/scale)` | `250×edge` | `÷0.5` | 饱和，不线性 |
| D1-B + D2-D | Kelly `f*` | `250×f*` | — | 与聚合器同源 |

（具体数值需按最终参数定；上表用于说明“有界/受资本约束”。）

---

## 4. 测试要求（验收，待方案定后落地）

- `test_multi_source.py` 新增：
  1. **资本约束**：`quantity`（或其 notional）`≤ kelly_fraction × base_capital × edge_cap`，且 `≤ max_position_per_market`。
  2. **不线性飙升**：固定 `dev_sigma`、成倍增大 `|market_dev|` → 仓位**不按同比翻倍**（有上界/饱和）。
  3. **零优势→零仓位**：`market_dev→0` 时仓位→0。
  4. **单位一致**：若 D1-A，断言 `quantity × cost ≤ max_position_per_market`。
- 保持既有 `multi_source` 信号方向/阈值测试全绿。

---

## 5. Allowed / Forbidden

**Allowed（实施期，待批）**：`strategy_engine/strategies/multi_source.py`、`strategy_engine/config.py`（仅新增 MultiSource 参数字段）、`test_multi_source.py`。

**Forbidden**：改 `max_position_per_market` 硬顶语义；改其他策略；改聚合器 Kelly 实现；改 Risk Model/阈值；改交易语义/资金控制；改 gRPC 契约。

---

## 6. 待独立审查回答的问题

1. **D1**：shares（A，与聚合器一致）还是 USDC notional（B，与风控上限直接可比）？
2. **D2**：edge 用 A（dev_mu 比值）/ B（μ0σ0 归一）/ C（饱和指数）/ D（Kelly f*）/ E（委托聚合器）？并给出 `edge_cap`/`eps`/`dev_scale` 取值口径。
3. **D3**：cap 作用在 notional 还是 shares？换算方式？
4. **D4**：`base_capital`/`kelly_fraction` 是否与 `AGG_*` 对齐（同一权威）？
5. **D5**：策略量是「顾问值」还是需要聚合器改用策略量（后者是否另开任务）？
6. 是否需要保留 `divergence`（源间分歧）对仓位的**有界**加成？若有，加成上限与公式？

> 结论：本方案**只做设计**；在你（或独立审查）冻结 D1–D5 后，我再实施 + 补测试。
