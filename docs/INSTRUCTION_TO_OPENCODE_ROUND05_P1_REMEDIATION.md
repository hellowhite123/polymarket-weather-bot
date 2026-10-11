# OpenCode 执行指令 — Round 05 P1 独立二审整改

## 任务性质

这是一次受限整改任务。请先阅读：

1. `AGENTS.md`
2. `MASTER_TASKS.md`
3. `BUG_TRACKER.md`
4. `docs/UNIFIED_FIX_TASK_R6.md`
5. 本文件

本文件基于 Hermes 独立二审，不得把 OpenCode 自测报告直接当作 VERIFIED 证据。

项目路径：`D:\WorkBuddyData\AI Weather`

## 当前独立验证基线

- Python：`123 passed / 0 failed`
- Rust：`338 + 4 + 18 + 1 = 361 passed / 0 failed`
- Rust clippy：`cargo clippy --all-targets` 为 0 error，但仍有非阻断 warning
- Docker runtime：当前环境没有 Docker，R5-009 只能保持 BLOCKED

## 全局红线

禁止：

- 修改交易语义；
- 擅自修改 RiskConfig 阈值含义；
- 擅自决定 Python/Rust Risk Authority；
- 擅自改变 P1-008 的 reserve accounting 语义；
- 擅自改变 P1-009 的 Unknown Order 对账语义；
- 修改 cold wallet、EIP-712 标准实现或 proto 契约；
- 大规模重构；
- 删除、弱化现有 fail-closed 检查；
- 将测试改成只验证实现当前行为；
- 修改 BUG_TRACKER / MASTER_TASKS 的 VERIFIED/CLOSED 状态；状态由独立审查者更新；
- 未经用户指示提交 git commit。

所有代码改动必须：

1. 只触及对应 Allowed Files；
2. 增加有鉴别力的回归测试；
3. 运行相关局部测试；
4. 最后运行 Python 全量 pytest、Rust cargo test、clippy；
5. 报告真实输出，不得编造测试结果。

只使用一个 OpenCode 工作目录，不要并行启动多个代理，避免模型限流和 Windows 文件锁。

---

# 阶段 0：基线与确认

先运行并保存：

```bash
cd 'D:/WorkBuddyData/AI Weather'
C:/Users/32389/.workbuddy/binaries/python/envs/default/Scripts/python.exe -m pytest -q

export PROTOC=C:/Users/32389/tools/protoc/bin/protoc.exe
cd 'D:/WorkBuddyData/AI Weather/polymarket-rust/polymarket-rust'
cargo test
cargo clippy --all-targets
```

若工作区 Rust 文件被 Windows 进程锁定，不要杀 Hermes/WorkBuddy 宿主进程；使用：

```bash
git archive HEAD | tar -x -C <temporary-review-directory>
```

在干净副本运行验证，并说明这是副本验证。

---

# 阶段 A：允许直接实施的修复

## A1 — P1-006 Cancel Order race / reserve release

### 已确认事实

当前 `service.rs:501-511` 的 `settle_cancel()` 已有基础修复：

- BUY 按 `remaining_size × limit_price` 释放 reserve；
- 调用 `mark_cancelled()`。

但它先释放 reserve，后执行状态转换。成交事件可能在两步之间到达，导致：

```text
cancel snapshot
→ concurrent fill
→ reserve release
→ mark_cancelled 被终态保护拒绝
```

### Allowed Files

- `polymarket-rust/polymarket-rust/src/grpc/service.rs`
- `polymarket-rust/polymarket-rust/src/fast/order_state.rs`
- 对应 Rust 测试文件或模块

### 要求

- 使取消状态转换与 reserve release 可重复且不因并发顺序产生错误释放；
- 至少只有成功从活动态转为 Cancelled 时才释放对应 reserve；
- client_order_id 和 exchange_order_id 两条路径都覆盖；
- `limit_price <= 0` 的行为必须明确、告警并 fail-closed；
- 不得借此改变 P1-008 的整体 reserve accounting 设计。

### 必须新增测试

- cancel 成功释放 reserve；
- cancel_all 释放全部活动订单；
- cancel 与 fill 并发；
- 已 Filled 订单不能被 cancel 释放；
- 缺失 limit price；
- client/exchange 两种 ID。

---

## A2 — P1-007 Reconcile reserve rebuild fail-closed

### 已确认事实

重建代码已存在：

- `server.rs:175-188` 启动重建；
- `server.rs:260-283` 后台重建；
- `service.rs:522-581` 按远端 open orders 重建。

问题是重建失败只记录 warning，之后仍可能 `TradingGate -> READY`。`full_reconcile()` 不检查 reserve 一致性。

### Allowed Files

- `polymarket-rust/polymarket-rust/src/bin/server.rs`
- `polymarket-rust/polymarket-rust/src/grpc/service.rs`
- 相关测试

### 要求

- reserve 重建失败后不得无条件回到 READY；
- 选择并记录一种安全语义：
  - 保留旧 reserve，成功重建后再替换；或
  - 失败时明确降级 TradingGate 为 Reconciling；
- 不得简单吞掉 fetch error；
- 不能把“positions/orders 对账通过”错误地当作 reserve 对账通过。

### 必须新增测试

- startup rebuild success；
- background rebuild success；
- open-orders API 失败后保持 Reconciling；
- 重复 reconcile 不重复累计 reserve；
- partial fill 后按 remaining size 重建。

---

## A3 — P1-011 Pagination

### 已确认事实

`state_sync.rs:115-134` 的 `PaginatedResponse` 没有 cursor；
`fetch_positions()`、`fetch_open_orders()`、`fetch_trades()` 都只返回第一页 `data`。

### Allowed Files

- `polymarket-rust/polymarket-rust/src/fast/state_sync.rs`
- 对应 StateSync 测试

### 要求

- 支持实际响应中的分页 cursor；
- positions/orders/trades 三条路径统一处理；
- cursor 必须推进，否则中止并 fail-closed；
- 对重复页面或重复记录去重；
- 设置最大页数保护；
- 任一中间页失败不得返回不完整的“成功结果”。

### 必须新增测试

- 两页 positions；
- 两页 open orders；
- 两页 trades；
- cursor 不推进；
- 重复页面；
- 中间页 HTTP 错误。

不要只增加 `next_cursor` 字段而不实现循环。

---

## A4 — P1-012 StateSync 数值解析 fail-closed

### 已确认事实

实时 `market_events.rs:778-791` 已返回 `Result`，但 `state_sync.rs:846-852` 的 `parse_json_f64()` 仍把非法值、缺失值、null 转为 `0.0`。

该值经 `RemoteTrade::price()/size()` 进入 `build_recovery_plan()`，可能生成零价或零量成交恢复动作。

### Allowed Files

- `polymarket-rust/polymarket-rust/src/fast/state_sync.rs`
- 对应测试

### 要求

- `parse_json_f64()` 改为 Result；
- 非法价格/数量不得生成 `MarkOrderFilled`；
- recovery 应进入 unresolved/fail-closed；
- 合法 0 值与解析失败必须可区分；
- 不得只修实时路径而遗漏恢复路径。

### 必须新增测试

- invalid string；
- null；
- NaN/Inf；
- 缺失字段；
- recovery action 被拒绝且进入 unresolved；
- 合法数字字符串与 JSON number 仍通过。

---

## A5 — P1-013 HMAC secret decode fallback

### 已确认事实

`clob/signing/hmac_sig.rs:35-37` 当前存在：

```rust
.decode(secret)
.unwrap_or_else(|_| secret.as_bytes().to_vec())
```

这是错误的 raw ASCII fallback。

### Allowed Files

- `polymarket-rust/polymarket-rust/src/clob/signing/hmac_sig.rs`
- `polymarket-rust/polymarket-rust/src/clob/auth.rs`
- API key 初始化调用点（仅错误传播）
- 对应测试

### 要求

- 删除 raw ASCII fallback；
- malformed secret 返回明确错误；
- `create_l2_headers()` 传播错误；
- startup/API key 初始化不得在 secret 非法时继续运行；
- 保持官方 URL-safe base64、padding、HMAC 消息格式不变。

### 必须新增测试

- 官方 vector 继续通过；
- 非法 base64 失败；
- 空 secret 失败；
- 缺 padding/非法字符失败；
- API 请求头构造向上传递错误。

---

## A6 — P1-014 Terminal capacity cleanup

### 已确认事实

`mempool.rs:549-555` 用 `orders.len()` 判断全局容量。
`update_status()` 只减少 per-token count，不删除 terminal entry。
`sweep()`（`618-647`）排除 Confirmed/Cancelled，导致它们永久占用 global capacity。

### Allowed Files

- `polymarket-rust/polymarket-rust/src/fast/mempool.rs`
- 对应测试

### 要求

二选一并保持计数一致：

1. terminal 状态立即从 `orders` 删除；或
2. global capacity 只统计 active entries。

还必须保证：

- Confirmed/Cancelled/Failed/Stale 都能回收容量；
- 重复终态更新不会重复扣计数；
- temp key → exchange key 更新不丢失计数；
- 不破坏现有 P0-012 原子 check+reserve。

### 必须新增测试

- Confirmed 后可复用 global capacity；
- Cancelled 后可复用；
- Failed/Stale 后可复用；
- repeated terminal update；
- 并发 submit 与 terminal cleanup。

---

## A7 — R5-001 集中度分子残留

### 已确认事实

R5-001 已修复 denominator 为 `committed_total()`，但 `new_market` 仍通过 `market_position()` 只读取 filled notional，不包含同市场 reserved。

### Allowed Files

- `polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`
- 对应测试

### 要求

- 增加 `market_committed(token_id)` = filled + reserved；
- concentration numerator/denominator 统一 committed 口径；
- 保持 Sell 的 `already_reserved = false` 修复；
- 保持 Buy concentration rollback；
- 不改 RiskConfig 阈值含义。

### 必须新增测试

- 同 token 已有 pending reserve 后再次下单；
- 只有 concentration 失败，不能被 position/global limit 抢先拦截；
- mutation：恢复 filled-only 实现后测试必须失败；
- Sell 不误释放其他订单的 reserve。

---

## A8 — R5-002 cap 测试补充

仅补测试，不改已确认公式：

- advisory notional 明显超过 cap；
- cap 恰好命中；
- YES/NO cost 两种方向；
- 断言 cap 在 USDC 层而不是 shares 层生效。

Allowed：`test_multi_source.py`。

---

# 阶段 B：必须先设计，禁止直接改代码

## B1 — P1-008 Reserve Accounting

当前问题确实存在，但修复需要定义 order-level reserve ledger、price improvement、partial fill、cancel 的权威语义。

先只做：

1. 代码调用链确认；
2. 设计文档；
3. 状态转移表；
4. A/B/C/D/E 边界案例；
5. Required tests 清单；
6. 不改 `risk_engine.rs` 业务记账逻辑。

输出设计文档后停止，等待人工授权。

## B2 — P1-009 Unknown Order Reconciliation

先设计：

- `Received` 与 `UnknownOutcome` 的状态模型；
- request fingerprint；
- exchange lookup / open order / trade history 的判定优先级；
- restart/reconcile 幂等语义；
- unresolved 如何驱动 TradingGate。

不先获得授权，不得擅自改变对账语义或状态机。

## B3 — P1-RISK-UNIT-001 Python/Rust Risk Authority

先输出设计，不直接改代码：

- position 的正式单位；
- global exposure 的正式单位；
- BUY_YES / BUY_NO notional 公式；
- `position_notionals` 的写入和回写链；
- Python/Rust 哪个是最终 authority；
- `AGG_MAX_POSITION` 与 Rust `RiskConfig` 如何同步；
- 是否需要 proto 字段。

不得通过简单把 `order.quantity * target_price` 填入 Python 来“修复”。BUY_NO 必须按真实成本计算，且不得形成第三套 authority。

设计文档输出后停止。

## B4 — P1-010 Snapshot/Event Authority

先输出设计并等待授权。必须明确：

- snapshot 与 event 谁是 authority；
- sequence/version/timestamp 规则；
- filled_size 是否允许回退；
- reconciliation correction 如何覆盖正常事件。

不得只添加一个 `max()` 就宣称问题解决。

## B5 — R5-005 完整 zeroization 证据

默认不改密码学实现。先输出 source-to-signing trace，特别是 `LocalWallet`、`Arc<LocalWallet>` clone 生命周期和 lock/TTL/drop 行为；只有发现明确缺陷且获授权后再改。

---

# 阶段 C：不改代码的项目项

## C1 — R5-009

Docker runtime 当前不可用。不要修改代码来伪造验证结果。
只保留 BLOCKED，并输出可执行验收命令：

```bash
docker compose config
docker compose up -d --build
docker compose ps
docker compose stop market_discovery
docker inspect --format='{{json .State.Health}}' ai-weather-app
```

## C2 — 文档状态

不要由实施者将 Finding 写成 VERIFIED/CLOSED。完成后只报告：

- changed files；
- tests；
- remaining limitations；
- recommended independent review points。

---

# 最终验收

完成阶段 A 后运行：

```bash
cd 'D:/WorkBuddyData/AI Weather'
C:/Users/32389/.workbuddy/binaries/python/envs/default/Scripts/python.exe -m pytest -q

export PROTOC=C:/Users/32389/tools/protoc/bin/protoc.exe
cd 'D:/WorkBuddyData/AI Weather/polymarket-rust/polymarket-rust'
cargo test
cargo clippy --all-targets
```

Acceptance：

- Python 全绿，不能少跑测试；
- Rust 全绿；
- clippy 0 error；
- 新增 mutation/边界测试存在且能鉴别旧实现；
- `git diff --name-only` 只包含对应 Allowed Files；
- 未修改交易语义、RiskConfig 阈值、proto、cold wallet；
- B 阶段设计任务必须明确标记“等待授权”，不得伪称已修复。

## 交付格式

最后只输出：

1. 本次实际修改的文件；
2. 每个 Finding 的状态：FIXED / BLOCKED / WAITING_AUTHORIZATION；
3. 新增测试及真实结果；
4. 未解决风险；
5. 下一步需要人工批准的设计项。
