# P0 → P1 降级记录（供 GPT 独立审核）

> **用途**：本文档汇总 Polymarket 天气预测市场量化交易系统中「资金/仓位安全相关项从 P0 降级到 P1」的全部记录，供 GPT 独立审核降级是否合理、是否同意归档。
>
> **来源**：`MASTER_TASKS.md`「P0 → P1 降级记录」+ `BUG_TRACKER.md`（P1 清单 / 关键风险提示 / 旧清单对照）。
>
> **当前状态**：待 Human（伯森先生）最终拍板归档。按项目《AI Code Review Protocol》，资金安全相关项降级需 Human 显式确认。

---

## 一、降级清单总览

| # | 降级项 | 旧 P0 编号 | 新 P1 编号 | 降级核心理由 |
|---|--------|-----------|-----------|-------------|
| 1 | post_check 成交闭环 | P0-06 / P0-13 | P1-005 / P1-006 | 真实成交触发依赖 L2 `/data/trades` 轮询，sandbox 无外网无法端到端验证 |
| 2 | pending orders 同步 | P0-12 | P1-005 / P1-007 | 状态同步已有单测，对账闭环已在启动 + 后台 30s 循环实现 |
| 3 | 后台对账循环 | P0-14 | P1-007 | 无资金直接暴露，属健壮性增强 |

---

## 二、逐项说明

### 1. post_check 成交闭环（旧 P0-06 / P0-13 → P1-005 / P1-006）

- **原 P0 内容**：
  - 旧 P0-06 = post_check 基础实现
  - 旧 P0-13 = 成交回报连接 post_check
- **降级理由（原文）**：逻辑已实现并有单测，但真实成交触发依赖 L2 `/data/trades` 轮询，sandbox 无外网无法端到端验证 → 推迟到 Shadow Trading。
- **当前实现与验证**：
  - P1-005 Order Lifecycle：`OrderState` 补 `mark_cancelled/rejected/expired` + 4 个生命周期测试 ✅
  - P1-006 Fill 同步：`ExecutionServiceImpl::on_market_trade()` 异步成交 → `RiskEngine.on_fill` + `FillLedger` + `OrderStore` ✅
- **残留风险**：真实成交端到端（L2 `/data/trades` 轮询驱动 `on_market_trade`）尚未在沙盒验证，留待 Shadow Trading。

### 2. pending orders 同步（旧 P0-12 → P1-005 / P1-007）

- **原 P0 内容**：Pending Orders 状态同步
- **降级理由（原文）**：状态同步已有单测，对账闭环已在启动 + 后台 30s 循环实现。
- **当前实现与验证**：
  - P1-005 Order Lifecycle（同上，`OrderState` 状态机 + 测试）
  - P1-007 State 恢复机制：启动 + 后台 30s 循环 `full_reconcile` 对账
- **残留风险**：原「从 open orders 消失的订单」无法确认终态（Filled/Cancelled/Expired）——此项正是 09-13 完成的 P0-007B 所解决（见第四节）。

### 3. 后台对账循环（旧 P0-14 → P1-007）

- **原 P0 内容**：后台周期对账循环
- **降级理由（原文）**：已实现每 30s `full_reconcile` + 差异告警；无资金直接暴露，属健壮性增强。
- **当前实现与验证**：`server.rs` 后台周期对账（每 30s `full_reconcile`）+ 差异告警
- **残留风险**：原实现「只检测、不修复」（差异仅告警，不自动恢复）——此项也由 09-13 完成的 P0-007B 补齐（见第四节）。

---

## 三、编号映射上下文（8/25 旧清单 → 新编号）

> 8/25 第一版 P0 清单（14 项）已映射到新编号。下表仅列与「降级」相关的映射，防止旧任务被误重新执行。

| 旧编号 | 内容 | 新编号 | 状态 |
|--------|------|--------|------|
| P0-06 | post_check 基础实现 | P1-005 / P1-006 | DONE |
| P0-12 | Pending Orders 状态同步 | P1-005 / P1-007 | DONE |
| P0-13 | 成交回报连接 post_check | P1-005 / P1-006 | DONE |
| P0-14 | 后台周期对账循环 | P1-007 | DONE |

---

## 四、最新进展（审核时需注意，09-13）

以下 09-13 已完成的 P0 工作**直接影响上述 3 项降级的「残留风险」判断**，请 GPT 审核时结合评估：

1. **P0-007B full_reconcile 自动恢复（已完成）**：
   - 新增 `full_reconcile_with_recovery` + `build_recovery_plan` + `ExecutionService::apply_recovery` + `RiskEngine::sync_position`。
   - 消失订单通过 `/data/trades` 分类终态（Filled → 补记 fill；NonFilled → 标记终态）；仓位差异以远端为权威重建。
   - trades 拉取失败时消失订单进入 `unresolved` 保持 FAIL-CLOSED。
   - → 直接补齐了第 2、3 项降级项「消失订单终态分类」「只检测不恢复」的能力缺口。

2. **P0-013/014/015 Trade 一致性链（已完成）**：
   - P0-013 Trade 归属（`attribute_order` 按 trader_side 正确归属）
   - P0-014 MarketEventPoller → ExecutionService 接线（`consume_trade_events`，默认关 `MARKET_EVENTS_ENABLED=1`）
   - P0-015 Fill 幂等（`trade_id` 原子去重，成交记账唯一走路径 B `on_market_trade`）
   - → 补齐了第 1 项降级项「成交回报连接」的事件链路（接线已就绪，只差真实外网端到端验证）。

---

## 五、需 GPT 审核的决策点

请 GPT 针对以下问题给出独立结论：

1. **降级理由是否充分**：3 项的降级理由（依赖 L2 轮询无法沙盒验证 / 已有单测与对账闭环 / 无资金直接暴露）是否成立、是否自洽？
2. **是否存在被遗漏的资金安全隐患**：这 3 项从 P0 降到 P1 后，是否引入了未覆盖的资金/仓位风险？
3. **结合最新进展重新评估级别**：09-13 已完成 P0-007B（自动恢复）与 P0-013/014/015（事件接线），3 项降级的「能力缺口」已被补齐。据此，这些项是应「确认归档」，还是「重新提级/新增验证任务」？
4. **是否同意归档**：是否同意将这 3 项按当前 P1 级别归档关闭？
