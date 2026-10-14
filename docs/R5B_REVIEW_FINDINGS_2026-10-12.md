# Round 05-B 复核发现与裁决 — 2026-10-12

> 状态：**待明日再次审核确认后继续**（本文档为阶段性裁决记录，未改动任何代码）
> 范围：B2（未知订单结果）、B3（风险单位）、B4（订单快照/成交事件并发）
> 方法：以当前代码为准逐条核对审阅引用，区分「描述属实 / 机制可达 / 仅模式成立」

---

## 0. 结论速览

| 项 | 审阅主张 | 裁决 | 是否需代码修复 |
|---|---|---|---|
| B2 | 未知订单结果「部分实现 · 尚未闭环」 | **成立**（未闭环部分属 DCR-001 方案 A 已批准延期） | 否（延期项，建议补文档注记） |
| B3 | Python 与 Rust 风险单位不一致 | **成立**（P1 未闭环，不能关闭） | 是（需先确认 proto 契约变更） |
| B4 | 订单快照读—改—写与成交事件并发竞态 | **部分成立**：非原子 R-M-W 属实；「成交事件覆盖」机制不可达；存在同类的**可达**缺陷 | 是（建议原子化响应路径 + 补并发测试） |

---

## 1. B3 / P1-RISK-UNIT-001 — Python 与 Rust 风险单位不一致

### 1.1 审阅主张
Python `risk_gate` 混用 shares 与 USDC；Python 默认 1000 与 Rust 500 双权威；BUY_NO notional 折算；两端配置在生产装配中无证据统一。

### 1.2 逐条核对（全部属实）

**A. Python 按 shares 与限额比较**
- `strategy_engine/aggregator/risk_gate.py:32-33`：默认 `max_position_per_market=1000.0`、`max_global_exposure=5000.0`。
- `risk_gate.py`（`_check_position_limit`，约 `:131-143`）：`existing = abs(state.get_position(token_id))` + `order.quantity` 与上限比较。
- `risk_gate.py`（`_check_global_exposure`，约 `:145-162`）：`current = state.get_global_exposure()` + `order.quantity` 与上限比较。
- 单位确认：`models.py:165-166` `RiskState.positions` 注释为「数量」；`models.py:190-191` `get_position()` 直取 `positions`；`models.py:89` `AggregatedOrder.quantity` = 「下单数量」→ **shares**。
- ⇒ **shares 与（应为 USDC 的）限额直接比较，单位混淆成立。**

**B. 全局暴露混单位（比描述更严重）**
- `models.py:193-202` `get_global_exposure()`：有 `position_notionals` 则求和（USDC），否则**回退为 `positions` 数量之和（shares）**。
- **关键**：`position_notionals` 在生产链**从未被写入**：
  - `strategy_engine/grpc_bridge/converter.py:152-170`（`risk_state_from_proto`）构造 `RiskState` 时**不含** `position_notionals`；
  - `proto/polymarket.proto:100-103` `PositionEntry` 只有 `token_id` / `quantity`，**无 notional 字段**（`RiskState` 见 `:113-120`）。
- ⇒ 生产中全局暴露**恒为 shares 口径**；且即使 `position_notionals` 存在，`+ order.quantity`（shares）仍是混加。

**C. Rust 按 USDC notional（一致）**
- `polymarket-rust/polymarket-rust/src/fast/risk_engine.rs:52` `DEFAULT_MAX_POSITION_PER_MARKET=500.0`；`:55` `DEFAULT_MAX_GLOBAL_EXPOSURE=2000.0`（均为 USDC）。
- `risk_engine.rs`（`PositionLimits::pre_check`，约 `:247-276`）、`check_and_reserve`（约 `:1027-1054`）均以 notional（USDC）比较，错误文案亦为 USDC。

**D. 两端配置未统一（有证据，非「无证据」）**
- Rust 生产：`src/bin/server.rs:108` `RiskEngine::new(RiskConfig::default())` → 硬编码 500/2000，**无 env/文件覆盖**。
- Python 生产：`strategy_engine/aggregator/aggregator.py:154-156` 用 `AggregatorParams`，值来自 `from_env()`（`aggregator.py:65-66` 默认 1000/5000；`:109-114` 读 `AGG_MAX_POSITION` / `AGG_MAX_GLOBAL_EXPOSURE`）。
- 部署：`docker-compose.yml:40-41` 仅注入 Python 端 `AGG_*`。
- `strategy_engine/config.py` 另有**四处** `max_position_per_market` 默认（`:32`=1000、`:64`=1000、`:102`=500、`:137`=800）→ 多源冲突。
- proto 未承载任何限额字段 ⇒ Design V2「方案 C：Rust 权威、经 proto `RiskState` 下发」**尚未实现**。

### 1.3 量化后果
- 价 0.10：Python 于 1000 shares ≈ **100 USDC** 拦截（严于 Rust 500 USDC）→ **误拦截**。
- 价 0.90：1000 shares ≈ **900 USDC** > Rust 500 USDC → **过宽**。

### 1.4 裁决
- B3 / P1-RISK-UNIT-001 描述准确，**确认为开放 P1，不能关闭**。
- 根因 = G4（`position_notionals` 无生产写入）+ G5（Python shares/USDC 混用）+ G6（双默认权威 1000 vs 500）+ 方案 C 未实现。
- 推进前提：proto `RiskState` 增配置字段属冻结契约变更，**需 Human 明确授权**后方可实施。

---

## 2. B2 — 未知订单结果复核（部分实现 · 尚未闭环）

### 2.1 审阅主张
`in-doubt` 查询/绑定与解析已实现且有测试（保留预留、歧义不自动选择）；但 `OrderStore` 为内存实现、重启不恢复、持久化测试延期；需验证启动解析结果是否在生产路径中真正约束交易闸门。

### 2.2 引用核对（属实，行号以当前代码为准）

| 审阅引用 | 核实结果 |
|---|---|
| `order_state.rs:457-501` | ✅ `in_doubt_orders()` @`:464`、`bind_remote_order()` @`:479` |
| `state_sync.rs:1110-1375` | ✅ `InDoubtOrder` @`:1124`、`resolve_in_doubt_orders` free fn @`:1265`（方法包装 @`:739`）；优先级 2=精确 `clientOrderId`、优先级 3=唯一指纹+互惠唯一，歧义→`unresolved` |
| 保留预留 / 歧义不自动选择 / 有测试 | ✅ 测试见 `state_sync.rs:2238-2393`、`service.rs:2646-2881` |
| `order_state.rs:6-9` 内存实现 | ✅ 逐字（`:9`「当前为内存实现；生产需持久化…R2-P2」）|
| `state_sync.rs:1117-1119` 重启不恢复、持久化延期 | ✅ 注释 `:1115-1119`（「内存态（无持久化）：重启不恢复，durable-restart 测试 DEFERRED（DCR-001 方案 A）」）|

### 2.3 关键待验证项：启动解析是否真正约束交易闸门
**接线存在、闸门确实拦截新单，但在生产路径中该约束是「空转」的。**
- 接线：`server.rs:179` 快照 → `:197-199` 解析+应用 → `:252` `unresolved_total = recovery.unresolved_orders.len() + in_doubt_unresolved` → `:254` `gate_after_reconcile(...)` → `:255` `trading_gate.reconcile_complete(pos_ok, ord_ok)`。
- 闸门确实挡单：`service.rs:778-780` `place_order` 首行 `ensure_ready()`，非 `Ready` 即 `failed_precondition`；`trading_gate.rs:92-101` `reconcile_complete` 仅两项同步进 `Ready`，否则 `Reconciling`，且不覆盖 `Halted`。
- **但空转**：`OrderStore` 启动即为新构造的内存空表（`order_state.rs:6-9`），故 `snapshot_in_doubt_orders()` 启动**恒为空** → `in_doubt_unresolved` 生产恒为 `0`（`server.rs:178` 注释自认）。后台 30s 循环（`server.rs:293-327`、`:390-424`）同理：in-doubt 只来自**本进程内**下单，重启即丢失。
- ⇒ 闸门对「非 in-doubt 的对账差异 / 预留重建失败 / `unresolved_orders`」是真约束；对 **in-doubt 的约束仅在进程存活期内成立，跨重启场景恒为空**。

### 2.4 裁决
- 审阅结论「部分实现 · 尚未闭环」**成立**；未闭环部分 = 跨重启 in-doubt 约束，属 **DCR-001 方案 A 已批准延期**（Human 2026-10-04），非接线缺陷。
- 建议（非阻断）：在文档/tracker 显式标注「启动阶段 in-doubt 约束依赖 R2-P2 持久化，当前为空转」，以免被误读为已闭环。

---

## 3. B4 — 订单快照与成交事件的并发一致性

### 3.1 审阅主张
响应路径 `service.rs:1023-1043` 读订单状态、改本地副本、`upsert` 写回；成交路径 `service.rs:434-518` 独立更新。若成交在读取与 upsert 之间发生，响应路径可能把旧副本写回覆盖事件更新；无统一版本控制/原子合并；需并发测试验证。

### 3.2 引用与模式核对（属实）
- 响应路径 `service.rs:1023-1043`：`get`(:1025) → 本地改副本（`mark_submitted` :1034 / `update_fill` :1037 / `mark_live` :1040）→ `upsert`(:1042)。`get` 与 `upsert` **各取一次锁，非原子**。
- 锁语义：`get`=`read().clone()`（`order_state.rs:178-180`）；`upsert`=`write().insert()`（`:229-234`，**整条替换**）；`update_fill`=**绝对覆盖**（`:115-126`）；事件路径 `apply_fill`=**增量**（`:134-139`）。
- 团队已在三处修过同一反模式：`apply_fill_by_exchange_order_id`（P1-027，注释 `order_state.rs:200-207`）、`mark_rejected`（P1-028，`:274-277`）、`mark_cancelled`（P1-RISK-006，`:309-312`）——**唯独漏了 `service.rs:1023-1043`**。

### 3.3 关键判定：「读取与 upsert 之间被成交事件覆盖」——机制上不可达
- 事件路径**只按 `exchange_order_id` 定位**：`get_by_exchange_order_id`(`order_state.rs:188-198`)、`apply_fill_by_exchange_order_id`(`:208-226`)。
- 正常下单流程中 `exchange_order_id` **唯一**写入点就是响应路径最后一步 `upsert`(`service.rs:1042`)；此前仅有 `set_limit_price`(:811) 与 `mark_rejected`。
- ⇒ 窗口期(1025→1042)内记录**尚无 exchange id** → 成交事件窗口内命中不了该订单 → 不可能「先更新再被覆盖」。
- 重试同理：`claim_or_retry`(`order_state.rs:385-405`) 对可重试终态**整条重建**（清空 exchange id/filled）。
- 同 `order_id` 在 `Live/Submitted` 时二次下单被幂等提前返回(`service.rs:787-806`)，不二次进入 R-M-W。

### 3.4 该非原子 R-M-W 仍存在的**可达**隐患（同样需修复）
1. **丢成交**：窗口内到达的成交，`claim_trade`(:445) 已消费 `trade_id`，随后按 exchange id 查不到订单（`apply_fill_by_exchange_order_id` 返回 `None`）→ **不重试**，OrderStore 漏记（RiskEngine/risk_state/FillLedger 不受影响）。
2. **覆盖 B2 绑定**：窗口内记录仍为 `Received` 且无 exchange id → 被 `snapshot_in_doubt_orders` 视为 in-doubt，后台 `apply_in_doubt_resolution`→`bind_remote_order`(`service.rs:328` / `order_state.rs:479-499`) 可先绑定，随后响应路径 `upsert` 旧副本**覆盖该绑定**。
3. **潜在重复计数**：响应路径写绝对 `filled_size` + 事件路径增量 `apply_fill`，同一笔成交两条路径都记时可能叠加（被 clamp 到 requested，表现为提前 `Filled`）。

### 3.5 测试覆盖
- 现有并发测试仅到 store 级：`test_apply_fill_by_exchange_order_id_concurrent_no_lost_update`(`order_state.rs:656`)、`test_concurrent_fill_and_reject_no_regression`(`:806`)。
- **无**覆盖 `place_order` 响应路径 R-M-W 与成交/绑定并发的跨路径链路。

### 3.6 裁决
- B4 的**观察成立**（全仓唯一未原子化的状态写路径）；**但所述「成交事件先更新后被覆盖」的具体交错不可达**，应改述为：窗口内成交被丢弃 / 覆盖 B2 绑定 / 潜在重复计数。
- 建议修复：将响应路径改为单写锁原子方法（如 `order_state.rs` 新增 `mark_submitted_and_update_fill`，带终态守卫，语义同 `apply_fill_by_exchange_order_id`），并补一条跨路径并发测试。

---

## 4. 待办与下一步
1. **明日再次审核**本文件三项裁决；确认后决定 B3 / B4 的修复授权与顺序。
2. B3 修复前须先确认 **proto `RiskState` 增配置字段（方案 C）** —— 冻结契约变更，需 Human 授权。
3. B4 修复建议：原子化 `service.rs:1023-1043` + 跨路径并发测试。
4. B2 无需修复；建议补一条文档注记说明「启动 in-doubt 约束依赖持久化、当前空转」。

## 5. 附：本次核查未改动任何代码
- 仅只读检查；未 `git add` / `commit`。
- 行号为当前工作区版本；审阅原文所引区间整体准确，个别起止行有 ±数行偏移。
