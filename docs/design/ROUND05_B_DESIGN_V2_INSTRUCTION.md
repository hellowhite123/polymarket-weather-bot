# Round 05 — B1–B5 Design V2 综合整改指令

> 本文件综合 Hermes 独立二审与 GPT 审核意见。
> 用途：供 OpenCode 修改 `ROUND05_B_DESIGN_DRAFTS.md`，只产出 Design V2，禁止实施代码。
> 基线：`7c75285`
> 当前总状态：`LIVE = NO-GO`

## 0. 严格执行边界

本轮属于 **Design-Only** 阶段。

OpenCode 只能修改/新增设计文档，不得修改任何 Rust/Python/proto/配置/部署代码。

### 允许

- 修改 `ROUND05_B_DESIGN_DRAFTS.md` 为 Design V2；
- 新增或修改与 Design V2 直接相关的 Markdown 设计文档；
- 输出 B5 source-to-signing trace；
- 在设计文档中定义 Required Tests。

### 禁止

- 修改 Rust/Python/proto/SQL/Docker/部署代码；
- 修改 RiskConfig、RiskGate、RiskEngine 的实际实现；
- 修改 strategy；
- 修改 trading semantics；
- 为了让测试通过而修改生产代码；
- 提前实现 B1–B4；
- 将 Required Tests 解释为“本轮必须编写/运行的代码测试”。

如果发现现有代码与 Design V2 不一致，只记录为 **Implementation Gap**，不得自行修改代码。

## 1. 本轮唯一目标

OpenCode 只能完成：

1. 修改 B1–B4 设计为可审查的 Design V2；
2. 完成 B5 source-to-signing 生命周期 trace；
3. 补齐调用链、状态机、authority、持久化、幂等和测试定义；
4. 不修改业务代码；
5. 不把任何 Finding 标记为 VERIFIED/CLOSED；
6. 提交后等待 Hermes 独立二审和人工授权。

## 2. 禁止事项

禁止：

- 直接实施 B1–B4；
- 将测试通过当作 VERIFIED；
- 将 B3 并入 R5-002；
- 修改 Risk Model 阈值或交易语义；
- 修改 strategy semantics、交易方向语义、cold wallet、EIP-712 标准实现；
- 用 `max()` 等局部手段掩盖 authority 未定义；
- 通过 TTL 自动释放未知订单 reserve；
- 通过模糊 fingerprint 自动认领未知订单；
- 因 Docker 不可用伪造 R5-009 验收结果。

---

# B1 — P1-008 Reserve Accounting

## 当前结论

`CHANGES_REQUESTED / WAITING_AUTHORIZATION`

已确认根因：

```text
reserve 入池 = limit_price × quantity
fill 抵扣   = execution_price × fill_quantity
```

price improvement 会造成 residual reserve。当前问题不是简单的减法错误，而是缺少 order-level reserve 归属。

## Design V2 必须冻结

### 1. OrderStore / RiskEngine 边界

当前 `RiskEngine::on_fill()` 没有 order ID / limit price。

禁止让 RiskEngine 反向依赖 gRPC service 或 OrderStore。

必须明确以下一种 API 方案：

```text
service.on_market_trade()
    → 根据 exchange_order_id 查询 OrderStore
    → 获取 order identity / limit_price / remaining
    → 调用明确的 RiskEngine order-level release API
```

或提供等价的上层 adapter，但必须明确：

- 谁查询 OrderStore；
- 谁计算 limit-based release；
- 谁持有订单级 identity；
- RiskEngine 接收什么参数；
- 如何保证 fill、cancel、replay 的原子性和幂等性。

### 2. Unknown limit

禁止：

```text
unknown limit → fallback fill notional → 宣称 residual 已清零
```

必须冻结：

```text
known limit
    → 按 limit × fill_delta_qty 精确释放

unknown limit
    → 不声称精确释放
    → order 进入 unresolved
    → reserve 保守持有
    → reconciliation 获取权威信息
```

### 3. 多订单同 token

必须覆盖：

```text
同 token
+ 不同 limit
+ 多个 order
+ 交错 partial fill
+ cancel
+ replay
```

设计必须证明：

```text
累计 reserve release
= 每个订单应释放的 limit × quantity 总和
```

不能只写 token-level aggregate 公式而不说明订单责任归属。

### 4. 状态机

至少包含：

```text
RECEIVED
SUBMITTED
LIVE
PARTIAL
UNKNOWN
FILLED
CANCELLED
EXPIRED
REJECTED
UNRESOLVED
```

每个状态必须定义：

- reserve 是否存在；
- reserve 是否可以释放；
- 谁可以推进状态；
- 是否可重试；
- replay 是否 no-op。

### 5. 幂等

必须定义：

```text
trade_id
exchange_order_id
client_order_id
cancel event
reconciliation action
```

之间的 identity 关系。

重复 trade、trade replay、cancel、cancel replay 都不得二次释放 reserve 或二次入账。

## B1 Required Tests

- 同 token、不同 limit 的两个订单交错 partial fill；
- full price improvement；
- partial fill → cancel；
- multiple fills → full fill；
- unknown limit；
- duplicate fill；
- duplicate cancel；
- fill/cancel 并发；
- unknown order ID；
- mutation：恢复旧 filled-notional 抵扣逻辑后测试必须失败。

---

# B2 — P1-009 Unknown Order Reconciliation

## 当前结论

`CHANGES_REQUESTED / WAITING_AUTHORIZATION`

当前链路：

```text
Unknown response
    → Received
    → pending_exchange_order_ids() 排除 Received
    → reconciliation 看不到
```

## Design V2 必须冻结

### 1. Durable persistence

当前 OrderStore 是内存结构。

如果设计声称支持 restart recovery，必须定义：

- durable journal；
- SQLite；
- WAL；
- 或明确的其他持久化机制。

如果暂时不支持跨重启 recovery，必须明确写：

```text
restart recovery not supported
```

不能同时宣称“支持 restart recovery”又依赖纯内存 OrderStore。

### 2. Unknown 状态模型

选择并定义：

```text
UnknownOutcome 独立状态
```

或：

```text
Received + 独立 in-doubt 集合
```

但必须保证 Received/Unknown 不会永久滞留且不进入 reconciliation。

### 3. Fingerprint

必须冻结字段、canonicalization、精度和时间容差：

```text
token
side
yes/no direction
price
size
client_order_id
request timestamp
```

必须定义：

- price/size 的比较精度；
- canonical serialization；
- fingerprint 唯一性；
- 多个相同候选的处理。

如果存在多个候选：

```text
不得自动认领
→ UNRESOLVED
```

### 4. Matching priority

必须采用明确优先级：

```text
1. exact exchange_order_id
2. exact client_order_id
3. unique fingerprint
4. otherwise unresolved
```

禁止模糊 fingerprint 自动认领。

### 5. TTL

TTL 不是权威 terminal outcome。

必须定义：

```text
TTL expired
    → 仍 unresolved
    → reserve 保守持有
    → TradingGate FAIL-CLOSED
```

禁止 TTL 到期直接释放 reserve。

### 6. Restart/replay 幂等

必须定义 action identity，保证 restart/reconcile/replay 不会重复：

- `on_fill`；
- reserve release；
- terminal transition；
- FillLedger record。

### 7. Trade attribution

必须明确 `/data/trades` 中：

- maker/taker；
- order attribution；
- trade_id；
- trade_id 缺失；
- 一个订单多笔 trade。

`trade_id` 缺失不得自动生成可应用 fill。

## B2 Required Tests

- in-doubt 不进入 READY；
- remote open order 唯一命中 → Submitted；
- ambiguous fingerprint → unresolved；
- TTL 到期 reserve 不释放；
- restart/replay no-op；
- duplicate trade 不重复入账；
- 缺失 trade_id 不产生 fill attribution；
- durable state 恢复后仍可继续 reconciliation。

---

# B3 — P1-RISK-UNIT-001 Python/Rust Risk Authority

## 当前结论

`CHANGES_REQUESTED / HIGHEST PRIORITY / WAITING_AUTHORIZATION`

必须保持为独立 P1，不得并回 R5-002。

## Design V2 必须冻结单位契约

| 项目 | 正式单位 |
|---|---|
| `quantity` | shares/contracts |
| `yes_price` | USDC/share |
| `no_price` | USDC/share |
| `position` | USDC notional |
| `global_exposure` | USDC notional |
| `reserve` | USDC notional |
| `max_position` | USDC |
| `max_global_exposure` | USDC |

## BUY_YES / BUY_NO

必须明确：

```text
BUY_YES:
    notional = quantity × yes_price

BUY_NO:
    notional = quantity × no_price
             = quantity × (1 - yes_price)
```

禁止：

```text
quantity × target_price
```

作为所有方向的一刀切公式。

## 唯一 Risk Authority

推荐冻结为：

```text
Python RiskGate
    = local projection / advisory

Rust RiskEngine
    = 唯一 authoritative blocking risk authority
```

如果采用其他方案，必须画出明确 authority state diagram。

不能继续保留：

```text
Python stale/incomplete state
    + Rust independent default
    = 两套 blocking decision
```

## `position_notionals` 生产链

Design V2 必须完整描述：

```text
Fill
 ↓
position_notionals write
 ↓
Sell
 ↓
Settlement
 ↓
Restart
 ↓
Reconciliation
```

每一步必须说明：

- writer；
- reader；
- lock；
- persistence；
- restart recovery；
- reconciliation correction；
- fees 和 price improvement 的处理。

当前只有字段定义和读取，不能视为生产链闭合。

## Configuration authority

必须从以下方案中选择一个：

```text
A. 统一环境配置 → Python/Rust
B. Rust 配置 authority → Python projection
C. Rust 通过 proto/RPC 下发 → Python
```

不得保留 Python default、Rust default、proto default 三套不同限制。

配置缺失、不一致、非法时必须 FAIL-CLOSED。

## B3 Required Tests

- BUY_YES notional；
- BUY_NO notional；
- shares ≠ USDC 场景不再误拒；
- Python/Rust 同一订单风险判定一致；
- price improvement；
- partial fill；
- fee；
- single source configuration；
- config missing/inconsistent → fail-closed。

## 授权前必须提交四份证据

```text
1. Unit Contract Table
2. Authority State Diagram
3. Configuration Transport Design
4. position_notionals Production Call Chain
```

---

# B4 — P1-010 Snapshot/Event Authority

## 当前结论

`CHANGES_REQUESTED / WAITING_AUTHORIZATION`

## Design V2 必须冻结

### 1. Version/sequence

当前秒级 `updated_at` 不能作为可靠并发排序依据。

必须指定：

- exchange sequence；
- local monotonic revision；
- snapshot generation；
- 或等价机制。

### 2. Snapshot/event merge

必须明确：

```text
Snapshot = cumulative filled
Event = incremental fill
```

在以下顺序中如何合并：

- snapshot → event；
- event → snapshot；
- snapshot → multiple events；
- duplicate event；
- reconciliation correction。

### 3. Filled 回退

普通事件路径必须单调不减。

如果 reconciliation 需要向下修正，必须走独立 correction channel，携带：

- authority；
- generation/version；
- reason；
- idempotency key。

禁止只使用 `max()` 掩盖 authority 缺失。

### 4. 三账本一致性

必须同时覆盖：

```text
OrderStore
RiskEngine
FillLedger
```

不能只修 OrderState。

### 5. 幂等和锁顺序

必须定义：

```text
trade_id
order_id
snapshot_version
event sequence
generation
```

以及 snapshot/event/reconciliation 的 lock ordering 和 commit ordering。

## B4 Required Tests

- stale snapshot 覆盖 event；
- event 先到、snapshot 后到；
- snapshot 先到、多个 event 后到；
- duplicate event；
- reconciliation correction；
- restart replay；
- filled 不应无理由回退；
- OrderStore/RiskEngine/FillLedger 最终一致。

---

# B5 — R5-005 Zeroization

## 当前结论

`CODE PASS / PENDING_EVIDENCE`

当前中间缓冲的 Zeroizing 方向正确，但只能承诺 best-effort zeroization。

## 必须完成 trace

```text
encrypted keystore
→ decrypt Zeroizing buffer
→ LocalWallet
→ SecretKey
→ LocalWallet clone
→ Arc<LocalWallet>
→ signing
→ cache
→ TTL
→ lock
→ sweep
→ drop
```

必须检查：

- `WalletManager::unlock()` 返回对象；
- `HotWalletCache::get()` 的 Arc clone；
- `FastOrderParams.password` 等 password clone；
- ethers/k256 内部非 Zeroizing 副本；
- `zeroize_wallet()` 调用时是否仍有 Arc clone；
- lock/TTL/sweep/drop 是否真正释放引用。

最终文档必须写清：

> 尽力清理受本项目控制的临时缓冲；外部签名库、寄存器、分配器、swap 和仍存活的 Arc 副本不作绝对保证。

---

# A1–A8 补充复核要求

这些不是 B 设计授权，但 Design V2 必须记录为限制：

- A1：`limit_price <= 0` 时可能先进入 Cancelled、再跳过 reserve release，需 unresolved release/reconciliation，不得只记录日志。
- A2：fail-closed 逻辑已加入；但 `rebuild_failed` 时日志不得继续声称 Ready。
- A3：分页 cursor 必须 URL encode；签名 request path 必须与实际请求路径完全一致；补特殊字符 cursor 测试。
- A4：非法 StateSync 数值进入 unresolved，方向正确。
- A5：HMAC raw ASCII fallback 已删除，方向正确。
- A6：global capacity active-only，方向正确。
- A7：同市场 committed numerator 已修，仍需 mutation 独立验证。
- A8：仅补 cap 测试，不扩展 R5-002 语义。

---

# 最终状态和交付要求

```text
B1  CHANGES_REQUESTED / WAITING_AUTHORIZATION
B2  CHANGES_REQUESTED / WAITING_AUTHORIZATION
B3  CHANGES_REQUESTED / HIGHEST PRIORITY
B4  CHANGES_REQUESTED / WAITING_AUTHORIZATION
B5  CODE PASS / PENDING_EVIDENCE
C1  BLOCKED
ROUND 05  LIVE NO-GO
```

OpenCode 交付时只输出：

1. Design V2 修改内容；
2. 每个 B 项是否满足设计冻结条件；
3. 未解决缺口；
4. Required Tests；
5. 是否需要人工授权。

不得输出 VERIFIED/CLOSED，不得修改代码。

只有当 B1–B4 均达到 `DESIGN PASS`、B5 达到 `EVIDENCE PASS` 后，才进入：

```text
Human Authorization
→ OpenCode implementation
→ Hermes independent code audit
→ VERIFIED/CLOSED
```
