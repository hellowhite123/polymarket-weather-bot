# Round 05 阶段 B 设计二审意见

日期：2026-10-04
基线提交：`7c75285`（A1–A8 + B1–B5 草案）
审查性质：Hermes 独立二审，不将 OpenCode 自测直接视为 VERIFIED。

## 验证结果

- Python：`126 passed / 0 failed`
- Rust：`355 + 4 + 18 + 1 = 378 passed / 0 failed`
- Rust clippy：`0 error`，仍有 warning
- Docker：当前环境无 Docker，R5-009 继续 BLOCKED

## 总裁决

B1–B5 均可保持 `WAITING_AUTHORIZATION`，暂不直接实施。草案方向总体正确，但 B1–B4 尚未达到可安全实施的设计冻结标准；B5 只能承诺 best-effort zeroization，不能承诺绝对内存擦除。

---

## B1 — P1-008 Reserve Accounting

### 结论：`CHANGES_REQUESTED / WAITING_AUTHORIZATION`

草案正确识别了：reserve 按 limit price 入池、当前 `on_fill` 按 execution notional 出池会留下 price-improvement residual。

### 必须补充

1. 当前 `RiskEngine::on_fill` 没有 order ID 或 limit price 参数。草案所说的“反查 OrderStore”必须明确放在哪一层，不能让 RiskEngine 反向依赖 gRPC service。
2. 需要明确 API 变化：由 `service.on_market_trade()` 先按 exchange order ID 获取订单限价，再把必要的 reserve-release 信息传入 RiskEngine，或新增专门的 order-level release API。
3. 方案 A 的“未知 limit 回退 filled notional”与“终态残余必须清零”存在冲突。未知 limit 时不能声称精确清零；应进入 unresolved/保守持有，并由 reconciliation 解决。
4. 必须定义多订单同 token、不同 limit price、交错 partial fill/cancel 的行为。单纯 token 聚合 reserve 无法表达订单归属，至少要证明累计释放仍严格等于每笔订单的 `limit × quantity`。
5. 状态表必须包括 `REJECTED`、`UNKNOWN`、`PARTIAL`，不应只写 ACTIVE→FILLED/CANCELLED/EXPIRED。
6. 释放必须按 order/fill identity 幂等，不能因重复 trade/cancel 产生二次释放。

### 授权前新增测试

- 同 token 两个不同 limit 的订单交错 partial fill；
- full price improvement；
- partial fill → cancel；
- unknown limit；
- 重复 fill/cancel；
- fill 与 cancel 并发；
- mutation：恢复旧的 filled-notional 抵扣后测试必须失败。

---

## B2 — P1-009 Unknown Order Reconciliation

### 结论：`CHANGES_REQUESTED / WAITING_AUTHORIZATION`

草案提出 `UnknownOutcome` / in-doubt 集合是正确方向，但还缺少可实施的身份和持久化定义。

### 必须补充

1. 当前 unknown order 只有 `client_order_id`、请求参数和 `Received` 状态，没有 exchange ID。必须定义状态持久化位置；目前 OrderStore 是内存结构，进程重启后 unknown order 会消失。若设计宣称支持 restart recovery，必须增加 durable journal/SQLite/WAL，或明确不支持跨重启。
2. fingerprint 匹配必须定义字段、精度和唯一性：token、side、price、size、client ID、timestamp、direction。两个相同订单不能自动任选一个匹配；歧义必须 unresolved。
3. 远端 open order 优先级必须是：精确 ID > 精确 client ID > 唯一 fingerprint；模糊 fingerprint 不得自动认领。
4. TTL 到期不能直接释放 reserve；没有权威 terminal 结果时应继续 fail-closed。
5. restart/reconcile 重放必须有 action identity，确保不会重复 `on_fill`、重复释放 reserve 或重复标记终态。
6. 必须定义 trades 中 maker/taker 订单归属和 trade_id 缺失时的处置。

### 不能只添加状态枚举

仅将 `Received` 改名为 `UnknownOutcome`，但仍不持久化、不进入 reconciliation candidate set，不能解决问题。

---

## B3 — P1-RISK-UNIT-001 Python/Rust Risk Authority

### 结论：`CHANGES_REQUESTED / 最高优先级 / WAITING_AUTHORIZATION`

草案正确确认：

- quantity 是 shares；
- position/global exposure 应为 USDC；
- BUY_NO 成本是 `1 - yes_price`；
- Python/Rust 存在双 authority 和默认值分歧。

### 必须补充

1. 必须选择唯一配置传输方案：统一环境配置，或 proto/RPC 下发；不能同时保留三套默认值。
2. 必须定义 Python RiskGate 的角色：
   - 只做本地 advisory；还是
   - 继续做 blocking pre-check。
   若 Rust 是唯一 authority，Python 不能用 stale/incomplete RiskState 做另一套硬阻断。
3. `position_notionals` 目前只有定义和读取，没有生产写入点。必须定义 on-fill、sell、settlement、restart reconciliation 的所有写入者、锁和持久化边界。
4. 明确价格来源：`market_price`、`target_price`、实际 fill price 各自只能用于对应语义，不能混用。
5. Python/Rust 对同一订单的 BUY_YES/BUY_NO、fee、price improvement、partial fill 必须有同一份单位契约。
6. 如果最终 authority 是 Rust，Rust 侧必须在 Python 配置缺失/不一致时 fail-closed，而不是静默使用另一套默认值。

### 授权前必须提供

- 单位契约表；
- authority 状态图；
- 配置传输方案；
- `position_notionals` 写入/回写调用链；
- Python/Rust 一致性测试。

---

## B4 — P1-010 Snapshot/Event Authority

### 结论：`CHANGES_REQUESTED / WAITING_AUTHORIZATION`

草案正确拒绝“只加 max()”的伪修复，但还没有冻结具体 authority。

### 必须补充

1. 定义 sequence/version 来源。当前 `updated_at` 是秒级时间，不能作为可靠并发排序依据。
2. 定义 snapshot 的累计 filled 与 event 的增量 filled 如何合并。
3. 明确 `filled_size` 是否单调不减；如果对账需要向下修正，必须走带 generation/authority 的独立 correction 通道。
4. 同时覆盖 OrderStore、RiskEngine、FillLedger 三个状态，不能只修 OrderState。
5. trade_id、order_id、snapshot version 的幂等关系必须明确。
6. 定义锁顺序和并发提交顺序，避免 snapshot/event 交错时出现状态倒退或重复记账。

### 必须新增测试

- stale snapshot 覆盖 event；
- event 先到、snapshot 后到；
- snapshot 先到、多个 event 后到；
- duplicate event；
- reconciliation correction；
- restart replay；
- filled quantity 不应无理由回退。

---

## B5 — R5-005 Zeroization

### 结论：`CODE PASS / PENDING_EVIDENCE`

当前 `Zeroizing<Vec<u8>>`、`Zeroizing<[u8; 32]>`、`Zeroizing<String>` 方向正确，wallet 测试通过。

但设计必须把保证范围写清楚：这是 best-effort，不是所有内存副本的绝对擦除保证。

### 必须补充 trace

```text
encrypted keystore
→ decrypt Zeroizing buffer
→ parse LocalWallet
→ SecretKey/LocalWallet clone
→ Arc<LocalWallet>
→ signing
→ cache hit / TTL / lock / sweep / drop
```

还必须追踪：

- `WalletManager::unlock()` 返回的 LocalWallet；
- `HotWalletCache::get()` 返回的 Arc clone；
- `FastOrderParams.password` 等 password clone；
- ethers/k256 内部是否产生非 Zeroizing 副本；
- `zeroize_wallet()` 不能在仍有 Arc clone 时强行声称已清零。

建议将文档表述改为：

> 尽力清理受本项目控制的临时缓冲；外部签名库、寄存器、分配器、swap 和仍存活的 Arc 副本不作绝对保证。

---

## A1–A8 实施后补充观察

- A1 的“先成功转 Cancelled 再释放”方向正确，但 `limit_price <= 0` 时订单已经进入 Cancelled、reserve 仍可能永久保留；应有 unresolved release/reconciliation 记录，不能只日志后丢失关联。
- A2 已加入启动/后台 rebuild failure 的 fail-closed gate 逻辑，方向通过；有一处日志路径在 `rebuild_failed` 时仍可能打印“复核一致/Ready”，日志需与真实 Gate 状态一致。
- A3 分页循环已实现。应补 cursor 含 URL 特殊字符时的 URL 编码测试，并确认签名路径使用编码后的 request path；当前代码直接拼接 `next_cursor`。
- A4 非法 StateSync 数值已改为 Result 并进入 unresolved，方向通过。
- A5 HMAC raw ASCII fallback 已删除，官方 vector 和 malformed secret 测试存在，方向通过。
- A6 global capacity 已改为 active-only 统计，方向通过。
- A7 R5-001 同市场 committed numerator 已补，方向通过，但仍需独立 mutation 验证后才能标 VERIFIED。
- A8 R5-002 cap 测试已补，公式没有被改变。

## 最终状态建议

```text
B1  CHANGES_REQUESTED / WAITING_AUTHORIZATION
B2  CHANGES_REQUESTED / WAITING_AUTHORIZATION
B3  CHANGES_REQUESTED / WAITING_AUTHORIZATION
B4  CHANGES_REQUESTED / WAITING_AUTHORIZATION
B5  CODE PASS / PENDING_EVIDENCE
C1  BLOCKED
```

在 B1–B4 设计冻结和人工授权前，不建议实施对应代码，也不建议把 Round 05 标记为 LIVE GO。