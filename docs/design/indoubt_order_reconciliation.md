# 设计方案：未知结果（in-doubt）订单的对账可见性与 fail-closed（P1-EXEC-009）

- **状态**：**Draft v1** — 待独立审查。未通过前不得实施。
- **作者**：OpenCode（工程实施者）
- **日期**：2026-09-17
- **关联**：`MASTER_TASKS.md` G4、`audits/round_04/audit_opencode_round_04.md` P1-EXEC-009
- **依赖接口变更**：可能（取决于是否新增可观测字段 / RPC）。若改 proto 须先审批。
- **证据基线**：`file:line` 基于 commit `8f22902`，**未经独立审查确认**。

---

## 1. 问题（现状与证据）

下单「结果未知」（网络超时 / 5xx，订单可能已上链）时，代码**保留预留、不标记终态**，等待 StateSync 对账确认：

```text
// grpc/service.rs（fast path Err arm）
if e.is_unknown_outcome() {
    tracing::warn!("下单结果未知（可能已成交），保留预留额度待 StateSync 确认: {e}");
    return Ok(... status: Pending ...);   // OrderStore 仍为 Received
}
```

但 `OrderStore` 的**对账输入集排除了 `Received`**：

```rust
// fast/order_state.rs:352-369
pub fn pending_exchange_order_ids(&self) -> HashSet<String> {
    ... match state.status {
        Submitted | Live | PartialFilled => state.exchange_order_id.clone(),
        Received | Filled | Cancelled | Rejected | Expired => None,   // Received 被排除
    }
}
```

而生产对账输入正是该集合：

```rust
// grpc/service.rs:273-275
pub fn snapshot_pending_exchange_order_ids(&self) -> HashSet<String> {
    self.order_store.pending_exchange_order_ids()
}
// bin/server.rs：启动与后台 30s 循环均用此集合喂 full_reconcile(_with_recovery)
```

**双失效**：
1. 未知结果订单停在 `Received`（无 `exchange_order_id`），**不在对账输入集** → `build_recovery_plan`（`state_sync.rs:895-943`）永远看不到它，无法分类终态；
2. 其预留（`reserved`）长期占用，**无释放路径**（`rebuild_open_order_reserves` 也只遍历 `pending_exchange_order_ids`）。

**后果**：in-doubt 订单成为本地「幽灵」：既不参与对账，也不释放预留；极端下反复出现会持续抬高暴露、误拒新单；且 TradingGate 可能因其他差异已 Ready，无法感知该订单存在。属对账/幂等/fail-closed 缺口。

> 注：`OrderStore::pending_order_ids()`（含 `Received`，`order_state.rs:332-348`）与 G4 新增的 `active_orders()`（含 `Received`）在**生产对账路径未被使用**。

---

## 2. 目标 / 非目标

**目标**
- in-doubt 订单**可见**且**不被静默忽略**；在未获确凿证据前，保持 fail-closed（不释放预留、不假设已成交/未成交）。
- 提供**可收敛的解决路径**：通过远端 open orders 匹配确认真实状态。
- 提供可观测性，便于人工/后台处置。

**非目标**
- 不改变「未知结果 = 保守持有预留」的基本策略。
- 不引入外部结算/PnL 逻辑（属 G1）。
- 不改 `FillLedger` / Risk Model 阈值。

---

## 3. 方案

### 3.1 Option A（最小，fail-closed 可见性）

1. `OrderStore` 新增 `in_doubt_client_order_ids()`：返回 `status == Received 且 exchange_order_id 为空` 的 client_order_id 集合。
2. `ExecutionServiceImpl` 暴露 `snapshot_in_doubt_order_ids()`。
3. `bin/server.rs` 对账循环：**若存在 in-doubt 订单**：
   - 记 `error!/warn!`（含数量与 id 样例）；
   - 调用 `trading_gate.reconcile_complete(false, false)`（保持/进入 `Reconciling`，**停新单**，fail-closed）；
   - **不释放**其预留（继续保守持有）。
4. 可观测：`get_risk_state` 增加 in-doubt 计数/标志（若接受 proto 字段新增；否则先只写日志/`HealthCheck`）。

**优点**：改动极小、明确 fail-closed、消除「静默」。**缺点**：不自动收敛；只要存在 in-doubt 就长期停新单，需人工解决。

### 3.2 Option B（推荐，A + 有限自动收敛）

在 A 基础上增加**匹配解决**：
- 后台周期性拉取远端 open orders（已有 `StateSync::fetch_open_orders`）。
- 对每个 in-doubt 订单，按 `(token_id, side, price, size)` 在远端 open orders 中匹配（远端 open order 缺 `asset_id→token_id` 反向映射，需用 OrderStore 持有的 token_id 对齐；见 Q2）：
  - **匹配成功** → 取得远端 `id`，`mark_submitted(id)`（转为 `Submitted`），并入正常对账/预留重建；
  - **匹配失败且未超 TTL** → 保留 in-doubt（继续 fail-closed）；
  - **匹配失败且超 TTL** → 标记 `unresolved`，告警并**继续持有预留**（不自动释放），等待人工处置（或按 Q4 策略）。
- TTL 与匹配策略为风控语义，需 Human 批准（§8）。

### 3.3 Option C（备选，需平台支持）

若 Polymarket CLOB 支持**客户端 order id**（或等价幂等标识），则下单时带上稳定 client id，对账时用它精确匹配，避免启发式匹配的误判。需先核实平台能力（Q3）；不成立则仍用 B。

**建议**：先落地 **Option A**（立即消除盲区），B 作为紧随的收敛增强；C 待平台能力核实。

---

## 4. 数据流（Option B）

```text
place_order 结果未知
   → OrderStore(status=Received, no exchange_id)  [in-doubt]
   → server 对账循环：
        ├─ in-doubt 存在 → TradingGate 停新单 (fail-closed)
        ├─ fetch_open_orders
        ├─ 逐单匹配 (token_id, side, price, size)
        │     ├─ 命中 → mark_submitted(ex_id) → 正常对账
        │     └─ 未命中（<TTL）→ 保留
        │     └─ 未命中（≥TTL）→ unresolved 告警，预留继续持有
        └─ 全部解决 → 复核对账 → 回 Ready
```

---

## 5. 测试计划

**Rust**
- `test_indoubt_order_blocks_ready`：存在 in-doubt → `reconcile_complete(false,false)`（Gate 非 Ready）。
- `test_indoubt_resolved_by_remote_match`：mock 远端 open order 命中 → `Received→Submitted` 且 `exchange_order_id` 写入。
- `test_indoubt_unresolved_keeps_reserve`：未命中 → 预留不被释放、Gate 保持 Reconciling。
- `test_in_doubt_excludes_submitted`：已有 exchange id 的订单不计入 in-doubt。
- **鉴别力验证**：让 in-doubt 集合恒空 → 第 1 个测试必 FAILED。

**集成**
- wiremock 模拟「下单超时→远端确有 open order」，验证从 in-doubt 到已提交的收敛。

---

## 6. 权限边界（Allowed / Forbidden）

**Allowed（建议）**
- `polymarket-rust/polymarket-rust/src/fast/order_state.rs`（`in_doubt_client_order_ids()`）
- `polymarket-rust/polymarket-rust/src/grpc/service.rs`（snapshot/暴露 + 与降级联动）
- `polymarket-rust/polymarket-rust/src/bin/server.rs`（对账循环处理 in-doubt）
- `polymarket-rust/polymarket-rust/src/fast/state_sync.rs`（若需匹配辅助；不改 `TimeWindowGuard`/阈值）
- （若加可观测字段）`proto/polymarket.proto` + Python `converter.py`（须单独批准）

**Forbidden**
- 修改 Risk Model 阈值 / 集中度 / Gas / 熔断判定；
- 在无确凿证据时**释放** in-doubt 预留或标记终态；
- 修改 trading semantics / strategy / funds-control；
- 修改 `FillLedger` 记账口径。

---

## 7. 风险

| 风险 | 说明 | 缓解 |
|------|------|------|
| 启发式匹配误判 | 同 token/side/price/size 多单可碰撞 | 要求唯一匹配才采纳；多义则保持 unresolved（fail-closed） |
| 长期停新单 | in-doubt 不收敛导致 Gate 一直 Reconciling | TTL + 人工处置通道；A 可选「仅该 token 冻结」（Q4） |
| 预留长期占用 | 保守持有导致暴露偏高 | 与 P1-RISK-008 协同；提供人工释放授权路径 |
| proto 可观测字段 | 新增字段需兼容 | 优先后台日志/HealthCheck，避免过早改 proto |

---

## 8. 待 Human 拍板的开放问题

1. **Q1**：是否接受 Option A 作为第一步（存在 in-doubt 即停新单，最保守）？
2. **Q2**：in-doubt 匹配键是否采用 `(token_id, side, price, size)`？是否需要处理 `asset_id→token_id` 映射（远端 open order 的 asset_id 语义）？
3. **Q3**：是否核实/使用 Polymarket 客户端 order id（Option C）？
4. **Q4**：in-doubt 持续未决时：**全局停新单**（基线）还是**仅冻结该 token**？是否提供人工「确认已成交/确认未成交」处置入口？
5. **Q5**：TTL 数值与超时后的告警等级？
6. **Q6**：是否为 in-doubt 计数新增 proto 可观测字段（涉及接口变更）？

---

## 9. 结论

Draft v1 指出 in-doubt（未知结果）订单因 `Received` 被排除在对账输入集之外而成为幽灵，提出 Option A（可见性 + 全局 fail-closed 停新单）与 Option B（远端匹配收敛）。**涉及对账语义与可能的接口变更，须独立审查 + Human 授权后方可实施。**
