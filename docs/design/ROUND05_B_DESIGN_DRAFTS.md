# Round 05 — B1–B5 Design V2（Design-Only）

- **状态**：**Design V2** — 已按 `ROUND05_B_DESIGN_V2_INSTRUCTION.md` / `ROUND05_B_DESIGN_REVIEW.md` 整改，**待 Hermes 独立二审 + Human 授权**。
- **基线**：`7c75285`（A1–A8 + B1–B5 草案）
- **总状态**：`LIVE = NO-GO`。本文件**只做设计**，未改任何 Rust/Python/proto/配置/部署代码。
- **约束**：任何设计语义 / 风险权威 / 单位定义 / 状态机 / 持久化语义 / 交易行为的后续改变，必须先暂停并提交 **Design Change Request**，经人工批准后方可继续。

> 代码与本文不一致处一律记为 **Implementation Gap**（见 §6），本阶段不修改代码。

---

## 0. 单位契约总表（Unit Contract，B3 冻结）

| 符号 | 正式单位 | 说明 |
|------|----------|------|
| `quantity` | shares / contracts | `PositionSizer` 输出（`base_capital×f/cost`） |
| `yes_price` | USDC/share | YES outcome token 价 |
| `no_price` | USDC/share = `1 − yes_price` | NO outcome token 价 |
| `position` | **USDC notional** | 单市场暴露 |
| `global_exposure` | **USDC notional** | 全组合暴露 |
| `reserve` | **USDC notional** | 挂单预留 |
| `max_position_per_market` | USDC | 单市场上限 |
| `max_global_exposure` | USDC | 全局上限 |
| `fee` | USDC | 默认 0（C-001） |
| `pnl` | USDC | 结算口径 net（含 `− fee`） |

**方向 → notional（唯一口径，禁止 `qty×target_price` 一刀切）**
```
BUY_YES： notional = quantity × yes_price
BUY_NO：  notional = quantity × no_price = quantity × (1 − yes_price)
```
**价格语义分离**：`market_price`（成交/执行参考）、`target_price`（策略目标价）、`fill_price`（实际成交）三者不得混用；notional 一律用**对应 outcome token 的市场/执行价**（见 B3 §3）。

---

# B1 — P1-008 Reserve Accounting

**结论：`CHANGES_REQUESTED` → Design V2 冻结如下；`WAITING_AUTHORIZATION`。**

## 1. 根因（确认）
`reserve 入池 = limit_price × quantity`；`fill 抵扣 = execution_price × fill_quantity`。price improvement / partial fill 留下 residual。根因是**缺 order-level reserve 归属**，非单纯减法错误。

## 2. OrderStore / RiskEngine 边界（冻结 API 方案）
**禁止** RiskEngine 反向依赖 gRPC service / OrderStore。采用**上层适配层**：

```
ExecutionServiceImpl::on_market_trade(trade)
  ① claim_trade(trade.trade_id)                      # 幂等（仅一次）
  ② let st = order_store.get_by_exchange_order_id(trade.order_id)   # 取 order identity
  ③ 计算 order-level release：
        known limit  → release = trade.size × st.limit_price
        unknown limit(≤0/缺失) → 不精确释放 → order 进 UNRESOLVED，reserve 保守持有
  ④ risk_engine.release_or_convert_on_fill(token, side, qty, exposure_notional, release_notional)
  ⑤ risk_state / fill_ledger.record / order_store.apply_fill_by_exchange_order_id
```
- **谁查询 OrderStore**：`ExecutionServiceImpl`（上层）。
- **谁计算 limit-based release**：`ExecutionServiceImpl`（持有 order identity）。
- **RiskEngine 接收参数**：`(token_id, side, qty, exposure_notional, reserve_release_notional)`；**不改阈值/判定**。
- **原子/幂等**：`claim_trade` 单点先去重；`apply_fill_by_exchange_order_id` 单写锁增量；release 与 fill 计入同一临界（RiskEngine per-token 写锁）。

## 3. Unknown limit（冻结）
```
known limit   → 按 limit × fill_delta_qty 精确释放
unknown limit → 不声称精确释放；order 进入 UNRESOLVED；reserve 保守持有；由 reconciliation 获取权威信息
```
**禁止** `unknown limit → fallback fill notional → 宣称残余清零`。

## 4. 多订单同 token（冻结）
必须满足**订单归属守恒**：
```
累计 reserve release（per token） = Σ_orders ( limit_i × quantity_i ) 对已终结订单
```
即 token 池化值只是派生；**每笔释放挂在具体 order_id/limit 上**。覆盖：不同 limit、多订单、交错 partial fill、cancel、replay。

## 5. 状态机（冻结；含 reserve 语义）

| 状态 | reserve 存在 | 可释放 | 谁推进 | 可重试 | replay |
|------|--------------|--------|--------|--------|--------|
| RECEIVED | 否（未 submit） | — | place_order | 是 | 幂等（claim_or_retry） |
| SUBMITTED | 是 | 否（未终结） | 响应/poll | 是 | no-op |
| LIVE | 是 | 否 | poll/snapshot | 是 | no-op |
| PARTIAL | 是（余量） | 否 | fill event | 是 | 按 trade_id no-op |
| UNKNOWN | 是（保守持有） | **否** | —（待对账） | — | no-op |
| FILLED | 余量→0 | 终态释放残余 | fill/trade | 否 | no-op |
| CANCELLED | 释放 remaining | 是（转换成功后） | cancel/对账 | 否 | no-op |
| EXPIRED | 释放 remaining | 是 | 对账 | 否 | no-op |
| REJECTED | 否/释放 | — | place 失败 | 否 | no-op |
| UNRESOLVED | 是（保守持有） | **否** | 人工/对账 | 否 | no-op |

推进规则（冻结）：`Filled/Cancelled/Rejected/Expired` 为终态，**不得倒退**；`UNRESOLVED` 不可自动转终态，需权威证据。

## 6. 幂等（identity 关系，冻结）
- `trade_id`：fill 级 identity（`FillLedger.claim_trade` 单点），重复 trade/replay → no-op。
- `exchange_order_id`：OrderStore 反查键（`apply_fill_by_exchange_order_id` 单写锁）。
- `client_order_id`：下单幂等键（`claim_or_retry`）。
- `cancel event`：以「活动态→Cancelled 转换成功」为唯一释放触发（A1 已实现）；重复 cancel → no-op。
- `reconciliation action`：`MarkOrderFilled` 携带 `trade_id` 去重；`MarkOrderNonFilled` 原子终态。
- **规则**：重复 trade / trade replay / cancel / cancel replay **均不得二次释放 reserve 或二次入账**。

## 7. B1 Required Tests（授权后实施）
同 token 两不同 limit 交错 partial；full price improvement；partial→cancel；multiple fills→full；unknown limit → UNRESOLVED；duplicate fill；duplicate cancel；fill/cancel 并发；unknown order ID；**mutation**：恢复旧 filled-notional 抵扣 → 必 FAILED。

---

# B2 — P1-009 Unknown Order Reconciliation

**结论：`CHANGES_REQUESTED` → Design V2 冻结如下；`WAITING_AUTHORIZATION`。**

## 1. Durable persistence（冻结：**明确不支持跨重启**）
当前 `OrderStore` 为**纯内存**（`RwLock<HashMap>`，`fast/order_state.rs`），无持久化。
**Design V2 冻结：`restart recovery not supported`（OrderStore 内存态）。**
- 明确声明：进程重启后 in-doubt/Received 订单**丢失**，不得宣称跨重启恢复。
- 若未来需跨重启：另立任务，采用 durable journal / SQLite / WAL + 启动回放（并定义 action identity）。**本设计不含该项**。

## 2. Unknown 状态模型（冻结）
采用 **`Received` + 独立 in-doubt 集合**（不新增枚举，降低侵入）：
- `OrderStore::in_doubt_client_order_ids()`：`status==Received && exchange_order_id.is_none()`。
- in-doubt 订单**必须进入 reconciliation candidate**（当前 `pending_exchange_order_ids()` 排除 Received → 需新增 in-doubt 通道，见 §6 Implementation Gap）。
- 保证：Received/in-doubt **不会永久滞留且被对账忽略**（要么被匹配推进，要么保持 unresolved 并驱动 Gate fail-closed）。

## 3. Fingerprint（冻结）
字段：`token_id, side(BUY/SELL), direction(YES/NO), price, size, client_order_id, request_timestamp`。
- **canonicalization**：price/size 归一为定点十进制字符串（避免 f64 抖动）；token/side 原样；timestamp 归一到秒。
- **比较精度**：price/size 精确相等（canonical 字符串相等）。
- **唯一性**：fingerprint 命中数 == 1 才可匹配；≥2 → `UNRESOLVED`（**禁止自动任选**）。

## 4. Matching priority（冻结）
```
1. exact exchange_order_id
2. exact client_order_id
3. unique fingerprint
4. otherwise → UNRESOLVED
```
**禁止**模糊 fingerprint 自动认领。

## 5. TTL（冻结）
`TTL expired → 仍 UNRESOLVED；reserve 保守持有；TradingGate FAIL-CLOSED`。
**禁止** TTL 到期直接释放 reserve。

## 6. Restart/replay 幂等（冻结）
- action identity：`client_order_id` + `trade_id`(+ `exchange_order_id`)。
- restart/reconcile/replay 不得重复：`on_fill` / reserve release / terminal transition / `FillLedger.record`。
- （因 §1 不支持跨重启，本条限进程内幂等。）

## 7. Trade attribution（冻结）
- `/data/trades`：`trader_side` 区分 MAKER/TAKER；`attribute_order` 归属本方 order；`trade_id` 为 fill identity。
- **`trade_id` 缺失 → 不得自动生成可应用 fill**（进 unresolved，与 A4 一致）。
- 一个订单多笔 trade：按 trade_id 逐笔增量（P1-030）。

## 8. B2 Required Tests（授权后实施）
in-doubt 不进入 READY；remote open order 唯一命中→Submitted；ambiguous fingerprint→unresolved；TTL 到期 reserve 不释放；restart/replay no-op；duplicate trade 不重复入账；缺失 trade_id 不产生 fill；durable 恢复后仍可对账（**当前不支持，测试标注 skip/预期不通过**）。

> ⚠️ **冲突声明**：§1 冻结「不支持跨重启」，故 Required Test「durable state 恢复后仍可继续 reconciliation」在**本设计范围内不成立**，标记为 `DEFERRED`（需先立项 durable persistence）。

---

# B3 — P1-RISK-UNIT-001 Python/Rust Risk Authority（**最高优先**）

**结论：`CHANGES_REQUESTED`；独立 P1，不得并回 R5-002；`WAITING_AUTHORIZATION`。**

## 1. 单位契约
见 §0。

## 2. 唯一 Risk Authority（冻结）
```
Python RiskGate  = local projection / advisory（非 blocking）
Rust RiskEngine  = 唯一 authoritative blocking risk authority
```
- Python `risk_gate._check_position_limit/_check_global_exposure` **不得**用 stale/incomplete RiskState 做另一套 hard-block。
- **禁止**：Python default（`AGG_MAX_POSITION=1000`）+ Rust default（`DEFAULT_MAX_POSITION_PER_MARKET=500`）两套 blocking decision。

## 3. Authority State Diagram（冻结）
```
 market event / fill / settlement
            │
            ▼
     Rust RiskEngine  ──(authoritative snapshot via GetRiskState/Stream)──►  Python RiskState (projection)
            │                                                                    │
       blocks/blocks?  (Buy限仓/熔断/结算窗口/Gas)                          advisory only（不得 hard-block）
            │
            ▼
     place_order 执行路径（Rust 单点放行/拒绝）
```
- 唯一 blocking 决策点：Rust `place_order → check_and_reserve`。
- Python 仅消费 Rust 快照做展示/预筛选；**不得**构成第二套资金闸门。

## 4. BUY_YES / BUY_NO notional
```
BUY_YES: notional = quantity × yes_price
BUY_NO : notional = quantity × no_price = quantity × (1 − yes_price)
```
**禁止** `quantity × target_price` 一刀切（BUY_NO 成本是 `1 − yes_price`）。

## 5. `position_notionals` 生产调用链（冻结设计）
```
Fill(成交)
  → writer: on_market_trade（or StateSync 恢复）计算 notional
  → write: RiskState.position_notionals[token] += notional（projection）
  → reader: get_global_exposure()（models.py:199-200）与 risk_gate
  → Sell: notional 递减（按方向成本）
  → Settlement: 结算后清零/归档（FillLedger.settle_by_token 对应）
  → Restart: 由 Rust RiskState 权威快照重建（Rust 重放；Python 仅接收）
  → Reconciliation: 以远端为权威修正（Rust `sync_position`）
```
每步须定义：writer / reader / lock / persistence / restart recovery / reconciliation correction / fee 与 price improvement 处理。
> **Implementation Gap**：当前 `position_notionals` **只有字段定义与读取，无生产写入点**（`models.py:170/199-200/219`），生产链**未闭合**。

## 6. Configuration authority（冻结：**方案 C**）
```
方案 A 统一环境配置 → Python/Rust
方案 B Rust 配置 authority → Python projection
方案 C（选定）Rust 通过 proto/RPC 下发 → Python
```
- 选定 **C**：Rust `RiskConfig` 为权威；通过 `GetRiskState`/新增字段下发给 Python。
- 消除三套默认值：`AGG_MAX_POSITION` / Rust `DEFAULT_MAX_POSITION_PER_MARKET` / proto 默认 → 统一为 Rust 权威。
- 配置缺失/不一致/非法 → **FAIL-CLOSED**（Python 侧不得回退自有默认）。

## 7. Configuration Transport Design（冻结）
- 传输：`RiskState` 增加/复用字段承载 `max_position_per_market` / `max_global_exposure`（**proto 变更，需单独批准**）。
- Python 启动/周期从 Rust 拉取；Rust 缺失配置 → 拒绝启动（fail-closed）。
- 版本/一致性：携带 `config_generation`；Python 检测不一致 → fail-closed。

## 8. B3 Required Tests（授权后实施）
BUY_YES notional；BUY_NO notional；shares≠USDC 不再误拒；Python/Rust 同单判定一致；price improvement；partial fill；fee；single source config；config missing/inconsistent → fail-closed。

## 9. 授权前必须提交的 4 份证据（本文件已含）
1. Unit Contract Table（§0）✅
2. Authority State Diagram（B3 §3）✅
3. Configuration Transport Design（B3 §7）✅
4. position_notionals Production Call Chain（B3 §5）✅（并标注 Implementation Gap）

Allowed（实施期，建议待批）：`aggregator/risk_gate.py`、`aggregator/models.py`、`aggregator/aggregator.py`、`config.py`、`risk_engine.rs`（仅幂等/接口，不改阈值）、`bin/server.rs`、`proto/polymarket.proto`（如需要）、对应测试。
Forbidden：交易方向语义、cold wallet、EIP-712 标准实现、Risk Model 阈值、strategy。

---

# B4 — P1-010 Snapshot/Event Authority

**结论：`CHANGES_REQUESTED` → Design V2 冻结如下；`WAITING_AUTHORIZATION`。**

## 1. Version/sequence（冻结）
秒级 `updated_at` 不作排序依据。引入：
- `local_monotonic_revision: u64`（OrderStore/OrderState 每次变更 +1）；
- `snapshot_generation: u64`（每次对账/快照 +1）；
- 事件携带 `event_seq`（本地接收序）。
合并规则以 **(generation, revision, event_seq)** 为准。

## 2. Snapshot / Event merge（冻结）
```
Snapshot = cumulative filled（权威来自 exchange snapshot/对账）
Event    = incremental fill（成交事件）
```
合并：
- snapshot→event：以 snapshot 为基线，后续 event 增量累加（revision 递增）。
- event→snapshot：若 snapshot 的 generation ≤ 当前，**不覆盖已应用 event 的增量**（stale snapshot 丢弃）。
- snapshot→multiple events：递增累加，单调不减。
- duplicate event：按 `trade_id` 去重（no-op）。
- reconciliation correction：**独立 correction channel**（见 §3）。

## 3. Filled 回退（冻结）
- 普通事件路径：`filled_size` **单调不减**。
- 向下修正仅经 **correction channel**，携带：`authority`、`generation/version`、`reason`、`idempotency_key`。
- **禁止**只加 `max()` 掩盖 authority 缺失。

## 4. 三账本一致性（冻结）
必须同时覆盖 `OrderStore` / `RiskEngine` / `FillLedger`，不得只修 `OrderState`；最终一致由复核对账驱动。

## 5. 幂等与锁/提交顺序（冻结）
identity：`trade_id`、`order_id`、`snapshot_version`、`event_seq`、`generation`。
- 锁顺序（统一，避免死锁）：`order_store` → `fill_ledger` → `risk_engine` → `risk_state`。
- 提交顺序：先持久/权威账本（envision：OrderStore 状态）→ FillLedger → RiskEngine；同 `trade_id` 只生效一次。

## 6. B4 Required Tests（授权后实施）
stale snapshot 覆盖 event；event 先到 snapshot 后到；snapshot 先到多 event 后到；duplicate event；reconciliation correction；restart replay；filled 不应无理由回退；三账本最终一致。

---

# B5 — R5-005 Zeroization（source-to-signing trace）

**结论：`CODE PASS / PENDING_EVIDENCE`。**

## Trace（已核对代码）
```
encrypted keystore (AES-256-GCM)                       keystore.rs
  → decrypt(password) -> Zeroizing<String>(0x-hex)     keystore.rs:146,160-170
  → WalletManager::unlock() -> LocalWallet(String.parse)  manager.rs:89（Zeroizing<String> 解析后即擦除）
  → LocalWallet / SecretKey（k256，默认 ZeroizeOnDrop）  hot_wallet.rs:316 注释
  → HotWalletCache::get() -> Arc<LocalWallet> clone
  → signing（EIP-712）
  → cache hit / TTL / lock / sweep：
        zeroize_wallet(&mut old/entry)                  hot_wallet.rs:180/233/242/261
  → drop
```
## 保证范围（冻结表述）
> 尽力清理**受本项目控制**的临时缓冲（`Zeroizing<Vec<u8>>` / `Zeroizing<[u8;32]>` / `Zeroizing<String>`）；**外部签名库（ethers/k256）、寄存器、分配器、swap 以及仍存活的 `Arc<LocalWallet>` 副本不作绝对保证**。

## 待补（授权前）
- 追踪 `WalletManager::unlock()` 返回对象生命周期；`HotWalletCache::get()` 的 Arc clone 计数；
- `FastOrderParams.password` 等 password clone；
- k256 内部是否产生非 Zeroizing 副本；
- `zeroize_wallet()` 调用时是否仍有 Arc clone（若有，**不得声称已清零**）。

---

# 6. Implementation Gap 清单（只记录，不改代码）

| # | Gap | 证据 | 影响 |
|---|-----|------|------|
| G1 | `RiskEngine::on_fill` 无 order_id/limit_price | `risk_engine.rs` `on_fill(token,side,qty,notional)` | B1 order-level release 需上层适配 |
| G2 | `OrderStore` 纯内存，无持久化 | `fast/order_state.rs` | B2 跨重启恢复不支持 |
| G3 | `pending_exchange_order_ids()` 排除 `Received` | `order_state.rs:438+` | B2 in-doubt 订单被对账忽略 |
| G4 | `position_notionals` 无生产写入点 | `aggregator/models.py:170/199-200/219` | B3 生产链未闭合 |
| G5 | Python `risk_gate` 用 shares 比 USDC 上限 | `risk_gate.py:131-140/151-160` | B3 单位混淆误拒 |
| G6 | Python/Rust 上限双默认（1000 vs 500） | `aggregator.py` vs `risk_engine.rs:52` | B3 双权威 |
| G7 | 无 monotonic revision/generation | `order_state.rs`（仅秒级 `updated_at`） | B4 无可靠排序 |
| G8 | cursor 直接拼接未 URL encode | `state_sync.rs`（A3） | A3 特殊字符 cursor |
| G9 | A1 `limit_price≤0`：订单转 Cancelled 但 release 跳过，无 unresolved 记录 | `service.rs release_reserve_on_cancel` | A1 需 unresolved release/reconciliation |

---

# 7. 状态与交付

```
B1  CHANGES_REQUESTED → Design V2 已冻结  WAITING_AUTHORIZATION
B2  CHANGES_REQUESTED → Design V2 已冻结  WAITING_AUTHORIZATION（含 DEFERRED: durable recovery）
B3  CHANGES_REQUESTED → Design V2 已冻结  WAITING_AUTHORIZATION（含 4 份证据）
B4  CHANGES_REQUESTED → Design V2 已冻结  WAITING_AUTHORIZATION
B5  CODE PASS / PENDING_EVIDENCE（trace 已补）
C1  BLOCKED（环境无 Docker）
ROUND 05  LIVE NO-GO
```

**未做**：未改任何 Rust/Python/proto/配置/部署代码；未将任何 Finding 标 VERIFIED/CLOSED。
**下一步**：Hermes 独立二审 + Human 授权 → 方可实施（实施后仍需 Hermes 代码审计 → VERIFIED/CLOSED）。
