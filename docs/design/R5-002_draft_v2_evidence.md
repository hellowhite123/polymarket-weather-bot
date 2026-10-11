# R5-002 Draft v2 — 实施前证据包（E1–E4）

- **状态**：EVIDENCE PACK — **未授权实施**（依 R5-002 Draft v2 §17）
- **作者**：OpenCode（工程实施者）
- **日期**：2026-09-18
- **关联**：`MASTER_TASKS.md` R5-002、`docs/design/R5-002_multi_source_position_sizing.md`
- **证据基线**：commit `325b37e`（`file:line` 供独立核验）

> 本文件只提供 Draft v2 §17 要求的 E1–E4 代码证据与由此得出的结论；**未修改任何生产代码**。

---

## E1 — `quantity` 单位（结论：**shares**）

调用链（Python → Rust）：

| 环节 | 位置 | 证据 |
|------|------|------|
| 策略信号字段 | `strategy_engine/models.py:152` | `quantity: float = 0.0  # 建议交易数量` |
| 聚合指令字段 | `strategy_engine/aggregator/models.py:89` | `quantity: float = 0.0  # 仓位计算后的下单数量` |
| 聚合器初始化 | `strategy_engine/aggregator/aggregator.py:259` | 构造 `AggregatedOrder(..., quantity=0.0)  # 暂未计算` |
| 聚合器重算 | `strategy_engine/aggregator/aggregator.py:301` | `quantity, sizing_detail = self.position_sizer.size(model_probability=target_price, market_price=market_price, ...)` |
| 聚合器写回 | `strategy_engine/aggregator/aggregator.py:332` | `order.quantity = quantity` |
| PositionSizer 公式 | `strategy_engine/aggregator/position_sizer.py:117-120` | `f = self.kelly_fraction * f_star`；`quantity = self.base_capital * f / cost`（`cost=price` 或 `1-price`，单位 USDC/share）→ **shares** |
| proto | `proto/polymarket.proto:75` | `double quantity = 8;  // 下单数量` |
| Rust 消费 | `polymarket-rust/polymarket-rust/src/grpc/service.rs:777` | `let notional = order.quantity * order.target_price;`（qty × price → **USDC notional**） |

**结论**：`quantity` 全链为 **shares / contract quantity**；Rust 用 `quantity × target_price` 换算为 USDC notional。→ **D1-A（shares）单位前提成立**（满足 Draft v2 §2「若确认 shares 则 D1-A」）。

---

## E2 — 资本来源（`AGG_*`：定义/加载/使用）

| 项 | 位置 | 证据 |
|----|------|------|
| 定义 | `strategy_engine/aggregator/aggregator.py:56-66` | `kelly_fraction=0.25`(`:57`)、`base_capital=1000.0`(`:59`)、`max_position_per_market=1000.0`(`:65`)、`max_global_exposure=5000.0`(`:66`) |
| 加载 | `strategy_engine/aggregator/aggregator.py:70-121` (`from_env`) | `AGG_KELLY_FRACTION`(`:95-96`)、`AGG_BASE_CAPITAL`(`:101-102`)、`AGG_MAX_POSITION`(`:109-110`)、`AGG_MAX_GLOBAL_EXPOSURE`(`:112-113`) |
| 使用（仓位） | `strategy_engine/aggregator/aggregator.py:160-164` | `PositionSizer(method=p.position_sizing, kelly_fraction=p.kelly_fraction, fixed_fraction=p.fixed_fraction, base_capital=p.base_capital)` |
| 使用（风控） | `strategy_engine/aggregator/aggregator.py:154-159` | `RiskGate(max_position_per_market=..., max_global_exposure=..., ...)` |
| 存储 | `strategy_engine/aggregator/position_sizer.py:44-47` | `self.kelly_fraction` / `self.base_capital` |

**结论**：Python 侧资本权威 = `AggregatorParams`（源 env `AGG_BASE_CAPITAL` / `AGG_KELLY_FRACTION`）。MultiSource 若需读取，应从**同一来源**（AggregatorParams 或统一配置层）取得，**不得新增第二套**（D4）。

---

## E3 — 聚合器是否重算 / MultiSource quantity 的作用（结论：**重算；MultiSource.quantity 未被消费**）

- `aggregator.py:242-259` 构造 `AggregatedOrder` 时 **`quantity=0.0`**；
- `aggregator.py:301` 由 `PositionSizer.size(...)` **重算** quantity；`:320` 再用 `size_limits.max_order_cost` 二次 cap；`:332` 写回 `order.quantity`；
- 全文件对 `quantity` 的引用仅 `order.quantity`（`:332/347/359/419/431/457`），**没有任何 `signal.quantity` 读取**。

**结论**：最终 `AggregatedOrder.quantity` = `PositionSizer` 结果；**MultiSource 的 `quantity` 不进入最终下单路径**，仅为**顾问值 / advisory**。→ 支持 Draft v2 §9 冻结 **D5-A**（MultiSource=advisory，Aggregator=final authority）。

---

## E4 — `max_position_per_market` 的 Python→Rust 单位链（含 **2 处重要发现**）

**Python**：
- 定义/加载：`aggregator.py:65`、`:109-110`（`AGG_MAX_POSITION` 默认 `1000.0`）；
- 传入 `RiskGate`：`aggregator.py:154-159`；
- 判定：`risk_gate.py:131-133` `new_total = existing + order.quantity; if new_total > self.max_position_per_market`（`order.quantity` = **shares**）；全局同理 `:151-153`。

**Rust**：
- 字段：`risk_engine.rs:110` `max_position_per_market: f64`；默认：`:132` ← `DEFAULT_MAX_POSITION_PER_MARKET = 500.0`（`:52`）；全局默认 `2000.0`（`:55`）；
- 判定：`:262` / `:310` 用 `notional`（USDC）比较；
- notional 来源：`service.rs:777` `notional = order.quantity * order.target_price`（**USDC**）；
- 接线：`bin/server.rs:107` `RiskEngine::new(RiskConfig::default())`；
- proto：**无** `max_position_per_market` 字段（`rg` 为空）。

### 发现 E4-1（单位不一致）
- **Rust**：`max_position_per_market` 与 **USDC notional** 比较 → 语义 = USDC ✅；
- **Python `risk_gate`**：与 **`order.quantity`（shares）** 直接相加比较 → **单位不是 USDC**，与 Rust 不一致（数量级还受 `price` 影响）。
- 即「`max_position_per_market` = USDC notional」**仅在 Rust 成立**；Python `risk_gate` 不成立。

### 发现 E4-2（双权威、数值不一致）
- Python 默认 `AGG_MAX_POSITION = 1000.0`（`aggregator.py:110`）；
- Rust 默认 `DEFAULT_MAX_POSITION_PER_MARKET = 500.0`（`risk_engine.rs:52`）；
- 二者**互不同步**（proto 未携带），实际上限为 **Python 1000 / Rust 500** 两套。

---

## 对 D1–D5 的影响（供独立审查）

| 决策点 | 证据结论 |
|--------|----------|
| **D1** | ✅ `quantity = shares`（E1）→ D1-A 成立；内部先算 USDC notional → cap → ÷cost 得 shares |
| **D2** | 与单位无关；D2-C 饱和函数可用，`dev_scale` 仍需数据口径（Draft v2 §4） |
| **D3** | ⚠️ 「cap = USDC notional」在 **Rust** 成立；**Python `risk_gate` 并非 USDC**。R5-002 Allowed 仅 `multi_source.py`/`config.py`/`test`，**不动 `risk_gate`** → advisory cap 应以 **USDC notional** 定义（与 Rust 一致），Python `risk_gate` 的不一致应**另立 Finding/任务** |
| **D4** | 资本权威 = `AGG_BASE_CAPITAL`/`AGG_KELLY_FRACTION`（`AggregatorParams`）；MultiSource 需**同源读取**，不得新增第二套。但 MultiSource 参数在 `config.py MultiSourceStrategyParams`，**「统一配置层」需独立审查给出读取路径**（直接注入 AggregatorParams？还是在 config.py 增加字段并在装配时从 aggregator 注入？） |
| **D5** | ✅ 冻结 D5-A（E3：MultiSource.quantity 未被消费） |

## 需独立审查新增回答的问题

1. **E4-1/E4-2 是否纳入 R5-002 范围**？（R5-002 Forbidden 明确不改 `RiskGate`/Rust；E4 问题建议**另立 P1 Finding**，本任务不碰）
2. **D4 的「统一读取」具体形态**：MultiSource 如何拿到 `AGG_BASE_CAPITAL`/`AGG_KELLY_FRACTION` 而不复制权威？（注入 vs 共享 config 对象）
3. advisory cap 以 **USDC notional**（与 Rust 一致）为准——确认？
4. D2-C 的 `dev_scale` 数据口径（Draft v2 §4 方案 A/B/C）选定。

## 结论

E1–E4 已提供（含 2 处 E4 发现）。**本任务仍处 DESIGN/EVIDENCE 阶段，未写任何生产代码**；待独立审查对上述 D1–D5 与 E4 范围问题给出裁决后，方可进入实施（实施将另附 `git diff` + `pytest -q`）。
