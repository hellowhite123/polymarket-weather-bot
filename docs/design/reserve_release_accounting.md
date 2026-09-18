# 设计方案：预留释放记账修正（P1-RISK-008）

- **状态**：**Draft v1** — 待独立审查（CHANGES_REQUESTED 常态流程）。未通过前不得实施。
- **作者**：OpenCode（工程实施者）
- **日期**：2026-09-17
- **关联**：`MASTER_TASKS.md` G4、`audits/round_04/audit_opencode_round_04.md` P1-RISK-008、`docs/design/CONSENSUS_DESIGN_ROUND_01.md`（流程参照）
- **依赖接口变更**：否（Rust 内部签名/记账语义变更）。但**触及 Risk Model 记账**，属 `AGENTS.md` 红线，须显式授权。
- **证据基线**：`file:line` 基于 commit `8f22902`，**未经独立审查确认**。

---

## 1. 问题（现状与证据）

`RiskEngine` 的 `reserved` 是**按 token 汇总的池化值**（无 order 维度）：

```rust
// fast/risk_engine.rs:204-209
struct PositionEntry { qty: f64, notional: f64, reserved: f64 }
// fast/risk_engine.rs:217  positions: RwLock<HashMap<String, PositionEntry>>
```

- 预留：`check_and_reserve` 按**限价名义额**预留 `notional = quantity × target_price`（`risk_engine.rs:272-306`）。
- 成交冲抵：`post_check`（`risk_engine.rs:322-344`，经 `on_fill` `:1088-1097`）按**成交名义额**冲抵：

```rust
// risk_engine.rs:332-338
Side::Buy => {
    let moved = entry.reserved.min(notional);   // notional = 实际成交价 × 成交qty
    entry.reserved -= moved;
    entry.qty += qty;
    entry.notional += notional;
}
```

**根因**：预留以「限价」入池，却以「成交价」出池。当成交价优于限价（`fill_price < limit_price`，买方向）或存在部分成交时，`Σ(成交名义额) < 预留额`，差额 `(limit - fill) × filled_qty` **永久残留在 `reserved`**，且订单已终结（成交完毕）无后续释放路径。

- G4 的 `settle_cancel` 只覆盖「取消」路径，按 `remaining_size × limit_price` 释放；对**已全部成交**订单无释放动作。
- 后果：`reserved` 单调偏高 → 单市场/全局暴露被高估 → 后续真实下单被风控误拒（可用性事故），并污染 `global_exposure` 口径。

**汇总**：`reserved` 的**生命周期应与订单的限价预留额对齐**，而非与逐笔成交名义额对齐。

---

## 2. 目标 / 非目标

**目标**
- 订单从「预留」到「终结（全成交/取消/过期）」的过程中，**恰好释放其预留额**，不残留、不超额。
- 覆盖 partial fill、价格改善、恢复路径、并发成交。
- 不改变」暴露/上限判定逻辑与阈值。

**非目标**
- 不改 `max_position_per_market` / `max_global_exposure` / 集中度阈值。
- 不改交易语义 / token 路由。
- 不改 `FillLedger` 记账。

---

## 3. 方案

### 3.1 根因视角

释放额应等于**该笔成交对应的限价预留份额**：`release = fill_delta_qty × order_limit_price`；
而**暴露**应以成交价计：`exposure = fill_delta_qty × fill_price`。
当前实现把两者混用同一 `notional`，是残差的直接来源。

### 3.2 Option A（推荐，最小）：拆分「释放额」与「暴露额」

- `PositionLimits::post_check` / `RiskEngine::on_fill` 增加一个**释放额**参数（或在内部接收限价）：

```text
post_check(token_id, side, qty, exposure_notional, reserve_release_notional)
Buy:
  moved = reserved.min(reserve_release_notional)
  reserved -= moved
  qty += qty
  notional += exposure_notional
```

- 调用方（`grpc/service.rs::on_market_trade`）在成交时：
  - `exposure_notional = trade.size × trade.price`
  - `reserve_release_notional = trade.size × order.limit_price`
  - 限价来源：`OrderStore::get_by_exchange_order_id(trade.order_id)` 的 `limit_price`（G4 已引入）。
  - **限价未知（0/缺失）时回退 `reserve_release_notional = exposure_notional`**（保守：宁可少释放，不超额释放）。
- 取消：G4 `settle_cancel` 已按 `remaining × limit_price` 释放；与逐笔释放相加后，**全生命周期恰好释放 `requested_size × limit_price`**。

**正确性论证（买方向）**：
- 全额成交：Σ 释放 = `Σ fill_qty × limit = requested × limit` = 预留额 → 残差 0。
- 部分成交 + 取消：Σ fill 释放 + 剩余取消释放 = `filled×limit + remaining×limit = requested×limit` → 残差 0。
- 部分成交 + 过期：需 `mark_expired` 路径同样释放剩余（见 §4）。

### 3.3 Option B（备选，更重）：按 order_id 的预留台账

- `PositionLimits` 维护 `reserves: HashMap<order_id, (token_id, reserve_notional, released_notional)>`，暴露由池化值聚合得出。
- 优点：可审计、可精确对账、天然支持「每单独立释放」；缺点：状态更多、并发/恢复路径更复杂，需处理 order_id 缺失（空 order_id）场景。

**建议**：采用 **Option A**（改动面小、直击根因）。若审查方认为需要更强可审计性，再评估 B。

### 3.4 与现有路径的衔接

| 路径 | 处理 |
|------|------|
| 正常成交 `on_market_trade` | 传入 `exposure_notional` 与 `reserve_release_notional` |
| 恢复补记 `apply_recovery::MarkOrderFilled` | 同 `on_market_trade`，限价由 OrderStore 反查 |
| 取消 `settle_cancel`（G4） | 已按 remaining×limit 释放，保持 |
| 过期 `mark_expired`（若触发释放） | 需按 remaining×limit 释放（**当前无释放，见 Q3**） |
| 对账重建 `sync_position` / `rebuild_open_order_reserves` | 以远端为权威，保持现有语义；`rebuild_reserve` 仍为累加（G4 已限定只重建被清空的 token） |

---

## 4. 边界与幂等

- **并发成交**：`post_check` 单写锁内完成，增量语义，无 lost update。
- **重复成交事件**：上层 `FillLedger::claim_trade` 已幂等，`on_market_trade` 不会重复调用；
  `post_check` 自身非幂等，依赖上层单点。
- **限价为 0**：回退为成交名义额（保守），并 `warn!`。
- **SELL**：不预留，不释放；保持现状。
- **恢复重建后的预留**：`sync_position` 清空该 token `reserved`，`rebuild_open_order_reserves` 回填 open 单剩余预留——与成交释放口径需一致（open 单按 `remaining × limit`，与 §3.2 一致）。

---

## 5. 测试计划

**Rust（`grpc/service.rs` / `risk_engine.rs` tests）**
- `test_reserve_release_on_price_improvement`：预留 `100×0.5=50`；成交 `100×0.45=45` → 释放额按 `100×0.5=50` → `reserved==0`，`notional==45`，可再满额预留。
- `test_reserve_release_partial_then_cancel`：部分成交后取消 → 释放全额（fill 部分 + remaining×limit），`reserved==0`。
- `test_reserve_release_limit_unknown_fallback`：限价未知 → 回退成交名义额并告警，不超额释放。
- `test_reserve_release_concurrent_fills`：并发多笔 fill 累加释放正确，无超额/残留。
- **鉴别力验证**：把释放额退回「成交名义额」→ 第 1 个测试必 FAILED。

**回归**
- 现有 `check_and_reserve` / `release_reserve` / `post_check` 相关测试需同步适配签名（若改签名），保持语义。

---

## 6. 权限边界（Allowed / Forbidden）

**Allowed（建议）**
- `polymarket-rust/polymarket-rust/src/fast/risk_engine.rs` —— **仅限**：`on_fill`/`post_check` 增加「释放额」入参并改用之；不改阈值/上限/集中度判定。
- `polymarket-rust/polymarket-rust/src/grpc/service.rs` —— 反查限价并传入两个金额。
- （若改签名）`fast/executor.rs` / 其它 `on_fill` 调用点同步。

**Forbidden**
- 修改 `max_position_per_market` / `max_global_exposure` / 集中度阈值 / Gas / 熔断判定；
- 修改 trading semantics / strategy / funds-control；
- 修改 `FillLedger` 记账口径。

---

## 7. 风险

| 风险 | 说明 | 缓解 |
|------|------|------|
| 签名/调用点遗漏 | `on_fill` 调用点需全部同步 | grep 全仓调用点 + 编译期强制 |
| 限价来源缺失 | OrderStore 无该单（空 id/恢复前） | 回退成交名义额 + 告警（保守） |
| 与 G4 取消释放重复/遗漏 | 两处释放需相加等于预留额 | 见 §3.4 表 + 组合测试 |
| 过期路径未释放 | 当前 `mark_expired` 无释放 | Q3 决定是否纳入 |

---

## 8. 待 Human 拍板的开放问题

1. **Q1**：采用 Option A（拆分释放额/暴露额，推荐）还是 Option B（per-order 台账）？
2. **Q2**：是否授权修改 `on_fill`/`post_check` 的**记账接口**（非阈值/判定）？
3. **Q3**：订单**过期**（`Expired`）路径是否也要按 `remaining × limit` 释放预留？（当前无释放，属同类泄漏）
4. **Q4**：限价未知时的回退策略：释放成交名义额（当前保守）还是**拒绝成交入账**（更强 fail-closed）？
5. **Q5**：是否要求额度池增加「不变量校验」（`reserved` 残余在订单终结后应为 0，违例告警）？

---

## 9. 结论

Draft v1 提出以「释放额 = 成交增量 × 订单限价」修正预留生命周期（Option A），并论证全成交/部分成交+取消的释放守恒。**涉及 Risk Model 记账，须独立审查 + Human 显式授权后方可实施。**
