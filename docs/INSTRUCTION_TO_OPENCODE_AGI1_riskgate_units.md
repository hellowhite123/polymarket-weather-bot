# INSTRUCTION_TO_OPENCODE — AGI-1: risk_gate.py 单位混淆修复（Shares vs USDC）

## 背景（已由独立审查方伯森先生实测确认）
`strategy_engine/aggregator/risk_gate.py` 的两个「后检查」方法存在单位混淆，
导致正常低价值订单被风控错误 BLOCKED：

- `_check_position_limit`（第 132 行）：`new_total = existing + order.quantity`，
  把 `order.quantity`（**shares**）直接与 `self.max_position_per_market`（**USDC**）比较。
- `_check_global_exposure`（第 152 行）：`new_total = current + order.quantity`，
  同样把 shares 加进与 USDC 上限的比较。

**实测复现**：买入 1500 份 @ $0.10（实际资金 $150，远安全）→ 被误拦
`FAIL: 新增 1500.00 > 上限 1000.00`。
**实盘路径**：`aggregator.py:338-339` 调用这两个检查，经 `first_failure`(343 行)
将订单标记 `BLOCKED`。对应 MASTER_TASKS R5-002 遗留的 **E4 finding**。

## 你必须处理的依赖事实（重要）
1. **`RiskState.position_notionals`（models.py:170）在生产代码中无写入点**——
   全仓只有它的定义与序列化，没有任何赋值逻辑。因此 `get_global_exposure()`（models.py:199-200）
   在生产**总是走回退分支**，返回 `sum(abs(positions))`（shares 之和），而非 USDC。
   → **不要**假设 `get_global_exposure()` 返回的是 USDC。修复必须让「存量记录」与
   「新增 notional」**量纲显式统一为 USDC**，不得依赖 `position_notionals` 已填充。

2. **下单成本口径（权威）**：`aggregator.py:309-313` 已定义
   `cost_per_share = market_price if direction==BUY_YES else (1 - market_price)`；
   `aggregator.py:420-421` 同口径。修复必须复用同一公式与同一变量，禁止另造。

3. **BUY_NO 陷阱**：二元合约 BUY_NO 成本 = 份数 × (1 − YES价)。若用 `target_price`(=YES目标价)
   直接乘 qty 会**高估 ~4 倍**（实测 1000份NO@YES0.8 → 800 vs 真实 200）。成本必须按
   `direction` 折算，不能用裸 `target_price`。

## Allowed Files
- `strategy_engine/aggregator/risk_gate.py`
- `strategy_engine/aggregator/models.py`（仅如需给 RiskState 增加「合并位置的
  USDC 暴露」辅助方法；不得改字段语义）
- `test_aggregator.py`（新增/修改对应用例）

## Forbidden（红线）
- 不改 `aggregator.py` 的 `cost_per_share` 口径 / 下单成本逻辑 / `size_limits`。
- 不新增全局状态、不改 `SignalDirection` / `RiskCheckResult` 契约。
- 不摸 Rust 侧 / proto / 其他策略文件。

## 修复要求
### 1. 新增统一的「订单名义价值（USDC）」辅助方法
在 `RiskGate` 内加私有方法（或模块级纯函数，便于测试）：
```python
def _order_notional_usdc(self, order) -> float:
    """下单名义价值 USDC。与 aggregator 成本口径一致：YES→price，NO→1-price。"""
    if order.quantity <= 0:
        return 0.0
    cost = (
        order.market_price if order.direction == SignalDirection.BUY_YES
        else (1.0 - order.market_price)
    )
    if not (cost > 0.0 and math.isfinite(cost)):
        return 0.0
    return order.quantity * cost
```

### 2. 修正 `_check_position_limit`
存量必须折算为 USDC。由于 `position_notionals` 生产未填充，允许**保守降级**：
优先 `state.position_notionals.get(token_id, 0.0)`；若该字典为空（生产现状容错），
用「自上而下的权威存量」不可得时，按 FAIL-CLOSED 处理——
**宁可拒绝也不低估**（返回 failed + 理由「存量 USDC 暴露未知，无法验证仓位上限」）。
```python
def _check_position_limit(self, order, state):
    if order.quantity <= 0:
        return RiskCheckResult(rule="position_limit", passed=True)
    new_notional = self._order_notional_usdc(order)
    # 存量 USDC：优先 position_notionals；若生产未填充（键不存在），fail-closed
    existing = abs(state.position_notionals.get(order.token_id, 0.0))
    if not state.position_notionals:
        # FAIL-CLOSED：无可靠 USDC 存量记录时不放行新单
        return RiskCheckResult(rule="position_limit", passed=False,
            reason="存量 USDC 暴露不可用(position_notionals 未填充)，拒单防低估")
    new_total = existing + new_notional
    if new_total > self.max_position_per_market:
        return RiskCheckResult(rule="position_limit", passed=False,
            reason=f"当前暴露 {existing:.2f} USDC + 新增 {new_notional:.2f} USDC "
                   f"= {new_total:.2f} USDC > 上限 {self.max_position_per_market:.2f} USDC")
    return RiskCheckResult(rule="position_limit", passed=True)
```

### 3. 修正 `_check_global_exposure`
存量 `get_global_exposure()` 生产现状实为 shares 回退 → 不能直接当 USDC 加。
改为**显式统一到 USDC**，同样 FAIL-CLOSED：
```python
def _check_global_exposure(self, order, state):
    if order.quantity <= 0:
        return RiskCheckResult(rule="global_exposure", passed=True)
    new_notional = self._order_notional_usdc(order)
    current = state.get_global_exposure()          # 现状：position_notionals 空→shares 回退
    if not state.position_notionals:
        return RiskCheckResult(rule="global_exposure", passed=False,
            reason="全局 USDC 暴露不可用(position_notionals 未填充)，拒单防低估")
    new_total = current + new_notional
    if new_total > self.max_global_exposure:
        return RiskCheckResult(rule="global_exposure", passed=False,
            reason=f"全局暴露 {current:.2f} USDC + 新增 {new_notional:.2f} USDC "
                   f"= {new_total:.2f} USDC > 上限 {self.max_global_exposure:.2f} USDC")
    return RiskCheckResult(rule="global_exposure", passed=True)
```

### 4. 数据链补齐（可选但强烈建议，单独说明）
`position_notionals` 生产无写入点是根因之一。若本任务范围外不便补齐写入，
至少新增一个独立 P1 finding 记录「RiskState 的 USDC 存量未接线」，避免修复后
因存量恒 0 / fail-closed 导致所有后检查拒单或放行偏差被误判为本次修复的回归。

## Required Tests（`test_aggregator.py`）
1. `test_position_limit_low_value_not_blocked`：1500 份 BUY_YES @0.10，注入
   `position_notionals={'t':0}` → **passed**（回归：原实现会 FAIL）。
2. `test_position_limit_converts_no_direction`：1000 份 BUY_NO @market 0.80
   （real cost $200）→ notional=200，上限 1000 → **passed**；并断言
   `_order_notional_usdc` 返回值 ≈ 200（防 target_price 高估）。
3. `test_position_limit_over_limit_rejected`：存量 notional 900 + 新增 200 →
   **failed**，reason 含「USDC」。
4. `test_global_exposure_mixed_unit`：`position_notionals` 填充时 current=USDC +
   新增折算 → 正确加总；超限 failed。
5. `test_empty_notionals_fails_closed_position` / `_global`：`position_notionals={}`
   → 两个后检查均 **failed**（fail-closed，不误放行）。
6. 回归保底：现有 `test_aggregator.py` 全量仍绿（若因 fail-closed 语义改变某旧断言，
   记录并说明，不擅自放宽断言）。

## 鉴别力验证（交付前必做）
把 `_order_notional_usdc` 临时改回 `order.quantity * order.target_price`：
   → 测试 2 必 FAILED（NO 高估）。
把 `_check_position_limit` 存量改回 `get_position()`（shares）：
   → 测试 1 / 5 必 FAILED。
还原后全部转绿。

## 验收锚点
- `python -m pytest -q` 全绿（当前基线 123 passed，新增后 ≥125）。
- 不改动的情况下 cargo / 其余模块不受影响。
- git diff 仅限 Allowed Files。