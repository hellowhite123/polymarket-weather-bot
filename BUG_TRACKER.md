# BUG_TRACKER.md — 现有 BUG 清单

**数据源**：GPT 修复进度清单（2026-08-26，16 项）＋ 8/25 第一版 P0 清单（14 项）＋ `docs/CODE_REVIEW_STANDARD.md` ＋ `MASTER_TASKS.md` ＋ **Round 05 完整源码审查与整改方案（2026-10-13）** ＋ **Round 05 独立终审裁决与 Round 06 双 AI 交叉核验清单（2026-10-14）**
**状态口径**：按六态状态机，GPT 的「✅已修 / 基本完成」仅等于 `FIXED`（代码已改、未验证），须补三重证据（git diff + 测试 + 调用链）才 `VERIFIED`。

---

## 〇、当前总体裁决（2026-10-14 最新）

> 🔴 **LIVE = NO-GO** ｜ 终审基线 `7c75285` / `a2dde81`

| 阻塞类别 | 编号 | 状态 |
|---------|------|------|
| **P0 硬阻塞（新增）** | **P0-BUYNO-001** BUY_NO 概率/市价二次取反 + 限价错位 | 🔴 OPEN（R6-G1） |
| **P0 硬阻塞（新增）** | **P0-GAMMA-001** Gamma 字段 JSON 字符串解析失败 + 静默 Mock 回退 | 🔴 OPEN（R6-G2） |
| **P0 硬阻塞（新增）** | **P0-FORECAST-001** 非降水指标返回物理量而非 [0,1] 概率 | 🔴 OPEN（R6-G2） |
| P0 硬阻塞（延续） | **P0-RISK-001** 日亏熔断生产零调用 | 🔴 OPEN（见二-F） |
| P0 硬阻塞（延续） | **P0-RISK-002** 结算窗口未注册 + 未知即放行 | 🔴 OPEN（见二-F） |
| P1 开放 | P1-RISK-UNIT-001（＝B3）风险单位不一致 | 🟠 不能关闭（R6-G1，须 proto 变更授权） |
| P1 开放（新增） | **P1-RISK-015** 撤单后迟到成交 → 预留二次释放 + 终态倒退 | 🟠 OPEN（R6-G3） |
| P1 开放（新增） | **P1-CONSIST-016** 已成交 in-doubt 永久锁死 TradingGate + 指纹时间戳过严 + cursor 未编码 | 🟠 OPEN（R6-G3） |
| P1 开放 | P1-010（＝B4）响应路径非原子 R-M-W | 🟠 开放 |
| P1 延期 | P1-009（＝B2）跨重启 in-doubt 空转 | ⏸️ PARTIAL（DCR-001 方案 A 已批准延期） |
| 环境阻塞 | R5-009 Docker 健康检查 | ⛔ BLOCKED（无 Docker Runtime Evidence） |

**✅ Round 05 正式晋升 `VERIFIED`（9 项，🔒 禁止回退）**：R5-001+A7、R5-002+A8、R5-003、R5-004、R5-007/R5-008、A2(P1-007)、A4(P1-012)、A5(P1-013)、A6(P1-014)。详见「二-G」§1。

**测试门禁现状**：Rust `cargo check` 0 error；`cargo test` **401 passed / 0 failed**；Python `pytest` **126 passed / 0 failed**；doctest 因本机 `ERROR_PIPE_BUSY`(231) 环境性失败；`fmt/clippy -D warnings` = **PRE-EXISTING BASELINE FAILURE**（禁止声称 PASS）；Docker 未实机验证。

**详见章节**：**「二-G」＝Round 05 独立终审裁决 + Round 06 双 AI 交叉核验清单（10-14，最新）**；「二-F」＝Round 05 完整源码审查与整改方案（10-13）。

---

## 一、总览

| 级别 | 数量 | GPT 状态分布 | 按标准的状态映射 |
|------|------|-------------|-----------------|
| P0 | 7+9 | 6 已修 + 1 基本完成 + 9 新发现 | **7 项 VERIFIED（09-09）+ 9 项实盘阻塞待修（09-13 GPT×Gemini 综合）**；**10-13 复查 2 项 P0 仍开放（P0-RISK-001/002）**；**10-14 Round 06 新增 3 项 P0（P0-BUYNO-001 / P0-GAMMA-001 / P0-FORECAST-001）** |
| P1 | 10+25 | 4 待修 + 5 待验证 + 1 遗留 + 25 新发现 | **10 项完成（09-10）+ 25 项待修（09-13 综合）**；**10-13 复查：P1-RISK-UNIT-001 / P1-010 仍开放，P1-009 部分闭环延期**；**10-14 Round 06 新增 2 项（P1-RISK-015 / P1-CONSIST-016）** |
| **合计** | **17+48** | | |


**验证进展**：
- `VERIFIED`：旧 P0-07/P0-11（TradingGate）+ P0-003（RiskEngine 超仓）+ P0-004（Order Dedup 并发）+ P0-007（full_reconcile 启动）+ P0-001（FastExecutor 初始化，09-08）+ P0-005（StateSync L2 认证）+ P0-006（StateSync JSON Decode）+ **P0-002（Docker Secret，09-09）**
- **P0 全部 7 项 VERIFIED（09-09）**
- **P1-001~010 全部完成**：P1-001~009（09-07）+ P1-010 claim 占位失败清理（09-10）

---

## 二、P0 清单（7 项）

| 新编号 | 问题 | GPT 状态 | 标准映射 | 对应旧清单 | 需补证据 |
|--------|------|---------|---------|-----------|---------|
| P0-001 | FastExecutor 初始化 | ✅ 已修 | **VERIFIED** | P0-01（对应） | 调用链逐层验证 + 4 个离线 mock 测试全绿 |
| P0-002 | Docker Secret | ✅ 已修 | **VERIFIED** | P0-02 | `load_wallet_password()` 优先读 secret 文件回退 env + 4 个测试全绿 |
| P0-003 | RiskEngine pre-check | ✅ 已修 | **VERIFIED** | P0-03（对应） | 超仓拒单测试已有 + 集成测试补 3 个 |
| P0-004 | Order Dedup | ✅ 已修 | **VERIFIED** | P0-05（对应） | 并发穿透已修复（原子 claim）+ 并发测试转绿 |
| P0-005 | StateSync API Key | ✅ 已修 | **VERIFIED** | 对应 08-24/25 的 L2 认证 401 修复 | create_l2_headers→HMAC 调用链 + 官方测试向量通过 |
| P0-006 | StateSync JSON Decode | ✅ 已修 | **VERIFIED** | 对应 08-25 PaginatedResponse 修复 | PaginatedResponse 双格式解析 + 6 个测试全绿 |
| P0-007 | StateSync reconciliation | ✅ 基本完成 | **VERIFIED** | P0-09 + 旧 P0-10 | full_reconcile 启动测试已补（reconcile_complete） |

**✅ P0-007 代码 + 测试已闭环**（08-26）：
1. `full_reconcile()` 已接入启动流程（`server.rs:138-141`）；
2. 对账结果驱动 TradingGate：`in_sync` 才切 `Ready`，否则保持 `Reconciling`；
3. 决策逻辑抽为 `TradingGate::reconcile_complete()`，补 4 个单测 + 5 个集成测试全过。

**✅ P0-001 代码 + 测试已闭环**（09-08）：
1. 调用链逐层核对：`server.rs:110/112/139` → `FastExecutor::new()` → `init_api_key()`(executor.rs:292) → `create_or_derive_api_key()`(client.rs:205) → `set_api_creds()`(client.rs:87) → `prewarm_domains()`(signing_cache.rs:131) → `warmup()`(http_fast.rs:127) → `hot.prewarm()`(hot_wallet.rs:135) → `set_fast_executor()`(service.rs:309)，全部落点真实；
2. FAIL-CLOSED：`new()`/`init_api_key()`/`health_check()` 任一失败 → `halt` + `return Err`，进程在 `serve()` 前退出；下单入口 `place_order` 首行 `ensure_ready()` 兜底；
3. 补 4 个离线 mock 测试（`tests/p0_production_validation/fast_executor_init.rs`，wiremock）：400→derive 回退 / 500 传播 / 成功四步预热顺序 / 失败不预热。全绿。

**✅ P0-005 代码 + 测试已闭环**（09-09）：
1. 调用链：`state_sync.rs:337 create_l2_headers` → `auth.rs:62 build_poly_hmac_signature` → `hmac_sig.rs:21` HMAC-SHA256（secret 用 `URL_SAFE` 带 padding 解码，输出同样带 padding）；
2. 官方测试向量对齐：`test_hmac_matches_official_vector`（secret=`AAA…A=`，签名=`4gJVbox-R6XlDK4nlaicig0_ANVL1qdcahiL8CXfXLM=`）通过，`cargo test hmac` 2/2 绿。

**✅ P0-006 代码 + 测试已闭环**（09-09）：
1. 调用链：`state_sync.rs:307/358` 先按 `PaginatedResponse` 包装对象解析，失败兜底顶层数组，保留 body 前 200 字符便于诊断；
2. 6 个双格式解析测试（`state_sync.rs` tests 内 `test_paginated_*`）全绿，`cargo test state_sync` 15/15。

**✅ P0-002 代码 + 测试已闭环**（09-09，Human 授权后修复）：
1. 根因：`docker-compose.yml:81` 设 `WALLET_PASSWORD_FILE=/run/secrets/wallet_password`，但原 `server.rs:42` 只读 `WALLET_PASSWORD`，Docker 部署密码恒空 → fail-closed 起不来；
2. 修复：`server.rs` 新增 `load_wallet_password()` —— 优先读 `WALLET_PASSWORD_FILE` 文件并 trim，未配置/读失败回退 `WALLET_PASSWORD`，两者空返回空串（上层 `!password.is_empty()` 保持 fail-closed）；
3. 测试：抽纯函数 `resolve_password(file_content, env_password)`，补 4 个测试（文件优先/回退 env/空白文件回退/双空 fail-closed）全绿；
4. `MASTER_TASKS.md` P0-02 已把 `server.rs` 加入 Allowed Files。

---

## 二-B、P0 实盘阻塞清单（09-13 GPT × Gemini 综合，待修 9 项）

> 来源：GPT `POLYMARKET_WEATHER_FINAL_CODE_AUDIT.md` + Gemini《代码审查修改建议汇总》。综合见 `docs/audit_synthesis_2026-09-13.md`。

| 编号 | 问题 | 来源 | 状态 | 修复阶段 |
|------|------|------|------|---------|
| P0-013 | Trade 归属 attribution（taker_order_id ≠ 用户订单） | GPT | 🔧 FIXED（09-13） | 阶段一 |
| P0-014 | MarketEventPoller → ExecutionService 未接线 | GPT | 🔧 FIXED（09-13，默认关，`MARKET_EVENTS_ENABLED=1` 启用） | 阶段一 |
| P0-015 | Fill 幂等 / 双路重复记账 | GPT | ✅ **VERIFIED（09-16）**：`on_market_trade` 是唯一调用 `RiskEngine::on_fill` 的路径；`claim_trade` 位于其之前，单写锁内 check+insert 原子，并发下仅首个返回 `true`；未发现绕过 `claim_trade` 的第二条路径；`FillLedger::record` 已用 trade_id 为 key（P1-029）。**正常非空 trade_id 不会二次入账**。残余边界风险见 **P1-035** | 阶段一 |
| P0-007B | full_reconcile 只有检测、没有恢复 | GPT | 🔧 FIXED（09-13：`full_reconcile_with_recovery` + `build_recovery_plan` + `apply_recovery` + `sync_position`） | 阶段三 |
| P0-008 | Risk check-and-reserve 非原子 | GPT | 🔧 FIXED（09-13：`check_and_reserve` 原子预留 + `release_reserve` 回退 + `on_fill` 冲抵预留） | 阶段二 |
| P0-009 | Risk 输入校验 NaN/Inf/负值 | GPT | 🔧 FIXED（09-13：notional/qty NaN/Inf/负值全拒 + `RiskConfig::validate` fail-fast） | 阶段二 |
| P0-010 | Nonce 并发分配 | GPT + Gemini | 🔧 FIXED（09-13：`next_local` AtomicU64 原子递增游标 + 并发测试） | 阶段二 |
| P0-011 | Nonce 确认语义 | GPT | 🔧 FIXED（09-13：`NonceState` 状态机 Allocated/Pending，分配游标与确认解耦） | 阶段二 |
| P0-012 | Mempool capacity TOCTOU | GPT | 🔧 FIXED（09-13：`try_submit` 原子 check+reserve + 并发测试） | 阶段二 |
| P0-016 | OrderStore 键错配：恢复/成交路径用 exchange_order_id 查 client_order_id 键的 store，静默失效 | 旺财验收发现（09-13） | 🔧 FIXED（09-13：新增 `get_by_exchange_order_id` + 修 `on_market_trade`/`apply_recovery` 2 处 + 3 测试） | 阶段三后 |
| P0-017 | market_events JSON 入口「错误变 0」：`parse_f64`/`parse_timestamp` 静默把非法数据转 0.0/0，绕过 P0-009 数值安全；`attribute_order` 可返回空 order_id | 伯森先生点名（09-15，P0-009 数值安全的入口层延伸） | 🔧 FIXED（09-15：`parse_f64`/`parse_timestamp` 改 `Result` + `is_finite` 守卫，`attribute_order` 空 order_id 拦截，`poll_trades` 任一解析失败 warn+continue 跳过整条 TradeEvent，+5 测试） | 阶段三后 |
| P0-018 | RiskEngine 超仓拒单集成测试缺失（审核 P0-003：只读 `pre_check` 已测，但原子 `check_and_reserve` 拒单后 reserve 不泄漏未锁定；place_order 拒单未验证「不调 FastExecutor/不产生有效订单」） | 外部审核（09-15） | 🔧 FIXED（09-15：`check_and_reserve` 超仓/全局超限 reserve 不泄漏 2 测试 + service 层 `test_place_order_over_limit_rejected_before_executor` 验证风控拦截先于 FastExecutor） | 阶段三后 |
| P0-019 | StateSync 启动 reconciliation 集成测试缺失（审核 P0-007：正常同步→Ready / critical diff→不 Ready / unresolved→fail-closed 三场景无端到端接线测试） | 外部审核（09-15） | 🔧 FIXED（09-15：新增 `tests/p0_production_validation/state_sync_startup.rs`，wiremock mock CLOB 三端点复现 server.rs 启动对账决策链，3 场景全绿） | 阶段三后 |

### ✅ P0-015 Fill Idempotency / Double Accounting — VERIFIED（09-16）

**验证结果**（伯森先生复核，三重证据：git diff + 测试 + 调用链）：

1. `ExecutionServiceImpl::on_market_trade()` 是全仓**唯一**调用 `RiskEngine::on_fill()` 的路径，不存在第二条入账入口。
2. `FillLedger::claim_trade()` 位于 `RiskEngine::on_fill()` **之前**（service.rs:358 vs 367）。
3. `claim_trade()` 使用 `RwLock<HashSet>` 的**写锁**，在同一临界区内完成 check + insert（`seen.insert()` 返回语义），无 TOCTOU。
4. 相同 `trade_id` 在并发情况下**只有第一个调用能获得 `true`**；后续重复事件在进入 `RiskEngine::on_fill()` 前直接 `return`。
5. 未发现绕过 `claim_trade()` 直接调用 `RiskEngine::on_fill()` 的第二条路径。
6. `FillLedger::record()` 使用 `trade_id` 作为正常成交记录的唯一 key，不再以 `order_id` 覆盖不同 partial fills（P1-029 双索引）。
7. `claim_trade` → `on_fill` 之间**无 `.await` 让渡点**（`on_fill` 为同步函数），不存在并发窗口。

→ **结论：正常非空 `trade_id` 成交事件不会发生二次 RiskEngine 入账。**

**残余边界风险**（已单列 **P1-035**）：
`claim_trade("")` 当前返回 `true`，空 `trade_id` 不具备幂等保护。该问题是否构成 P0，取决于上游 `TradeEvent` 是否能够产生空 `trade_id`：
- 若交易所 API 的有效成交事件保证存在非空 trade ID → 属防御性校验问题；
- 若 API 异常响应可能产生空 ID → 应在 `poll_trades()` 入口 fail-closed，禁止空 `trade_id` 进入 `on_market_trade()`。

**旺财查证结论（09-16）**：上游**代码层面允许**空 trade_id 产生——`TradeApiResponse.id`（market_events.rs:121）与 `RemoteTrade.id`（state_sync.rs:61）均带 `#[serde(default)]`，API 缺字段时得到 `""` 且**反序列化不失败**；且 poller 层去重对空 id 双重失效（market_events.rs:286 不去重、360 不记录）。详见 P1-035。

**阶段一（已完成 09-13）**：P0-013 → P0-014 → P0-015（Trade 一致性链）。
**阶段二（已完成 09-13）**：P0-008（check-and-reserve 原子）+ P0-009（输入校验）+ P0-010/011（Nonce 并发分配 + 状态机）+ P0-012（Mempool capacity TOCTOU）。
**阶段三（已完成 09-13）**：P0-007B（full_reconcile 自动恢复）。

> ✅ **P0 实盘阻塞 9 项全部 FIXED（09-13）**，待三重证据（git diff + 测试 + 调用链）VERIFIED。
> ➕ **P0-016（09-13 验收新增）**：OrderStore 以 client_order_id 为键，但 `apply_recovery`/`on_market_trade` 用 exchange_order_id 查 store，恢复/成交对 OrderStore 更新静默失效。已 FIXED（新增 `get_by_exchange_order_id` + 修 2 处调用 + 3 测试），全绿 303 passed。
> 🔗 **审核编号映射（09-15 对齐）**：外部审核文件 `Polymarket_P0_P1_Current_Audit.md` 的 `P0-003`（超仓拒绝测试）＝本表 **P0-018**；审核 `P0-007`（startup reconciliation 集成测试）＝本表 **P0-019**；审核 `P0-004/P0-008/P0-015/P0-016` 与本表同编号一一对应。P1 编号见「三-B」。

---

## 二-C、Round 04 独立共识确认的 P0（09-17）

> 来源：`audits/round_04/audit_opencode_round_04.md` + 独立共识审查（`audits/round_04/CONSENSUS_ROUND_04.md`）。
> 状态口径：**CONFIRMED / P0**；已授权进入 `MASTER_TASKS.md` 对应任务组（`Status: TODO`）。总体 **LIVE = NO-GO**。

| 编号 | 问题 | 共识 | 任务组 |
|------|------|------|--------|
| P0-RISK-001 | 日亏熔断从未触发（`post_check` 生产零调用） | CONFIRMED / P0 | G1 |
| P0-RISK-002 | 结算窗口（2h 禁开 Buy）未接线，且未知即放行 | CONFIRMED / P0 | G1 |
| P0-RISK-003 | Gas 费保护被硬编码 `0` 绕过 | CONFIRMED / P0 → 🔧 FIXED（09-17，G1-003；待独立判 VERIFIED） | G1 |
| P0-EXEC-004 | 实时成交链不生效（`/data/trades` 无 L2 认证 + token 列表恒空 + 错误吞没） | CONFIRMED / P0 → 🔧 FIXED（09-17，G2；待独立判 VERIFIED） | G2 |
| P0-EXEC-005 | Maker 成交未按 `trader_side`/`maker_orders` 归属本方方向 | CONFIRMED / P0 → 🔧 FIXED（09-17，G3；待独立判 VERIFIED） | G3 |

> 独立审查方另确认：P1-035 维持 `FIXED / Verification Pending`，不得由实施者自证 VERIFIED。
>
> **Round 04 P1 实施登记（G4，09-17）**：`P1-RISK-006`（取消不释放预留）🔧 FIXED（G4，待 VERIFIED）；`P1-RISK-007`（后台不重建 open 预留）🔧 FIXED（G4，待 VERIFIED）；`P1-RISK-008`（成交价优于限价的预留残余）⬜→ 已由 Round 05 B1 覆盖（09-17 时需设计授权；2026-10-04 设计 B1 冻结、`ee839be` 实施 FIXED，待 VERIFIED）；`P1-EXEC-009`（未知结果订单被对账输入集排除）⬜→ 已由 Round 05 B2 覆盖（设计 B2 冻结 + DCR-001 方案 A 2026-10-04 批准，2026-10-07 实施 FIXED，待 VERIFIED）。

---

## 二-D、Round 05 P0 — R5-001 Sell 分支修正（09-18）

| 编号 | 问题 | 状态 | 关键点 |
|------|------|------|--------|
| R5-001（Sell 分支） | `check_and_reserve` 在引入 `already_reserved` 后对 **Sell 也传 `true`**（Buy 才预留）→ `future_total` 漏算 Sell 本单 notional，集中度门禁/比率被低估，与 `pre_check` 口径不一致 | ✅ **CONFIRMED**（09-18 独立审查） | `risk_engine.rs`：`already_reserved = matches!(side, Side::Buy)`。测试重做（只因 concentration 失败）：`test_sell_concentration_rejected_without_position_limit_breach`（Position/Global PASS + 集中度 FAIL，断言错误含「集中度限制」）、`test_sell_concentration_failure_does_not_release_existing_reserve`；mutation（改回 `true`）两测试必 FAILED。Rust `338 + 4 + 18 + 1` 全绿。⚠️ **Global `fmt --check`/`clippy -D warnings` = PRE-EXISTING BASELINE FAILURE**（非本项引入）；**R5-001 授权文件 `risk_engine.rs` = clean**；**不得声称 `clippy -D warnings` PASS** |

---

## 二-E、Round 05 P1 独立二审登记（2026-10-04，commit `7c75285`）

> 编号沿用 `round 5.md`（P1-006~014）；与既有「三、P1 清单」的 P1-006~010（09-10 批次）**同号不同义**，此处按 Round05 文档口径记录。
> 实施者仅登记 **FIXED / WAITING_AUTHORIZATION / BLOCKED**；**不写 VERIFIED/CLOSED**。
>
> **进度注记（2026-10-07）**：A1–A8 🔧 FIXED（commit `7c75285`）；B1 🔧 FIXED（commit `ee839be`，8 测试 + mutation 7 FAILED）；**B2 🔧 FIXED（2026-10-07，15 测试 + mutation 2×3 FAILED，durable-restart DEFERRED per DCR-001 方案 A，尚未提交）**；B3（P1-RISK-UNIT-001，最高优先）、B4、B5 ⛔ WAITING_AUTHORIZATION（B3 须先确认 proto `RiskState` 配置字段变更）；C1 BLOCKED（无 Docker）。全部 FIXED 项均待独立方三重证据 → VERIFIED，实施者不自证。

| 编号(round5) | 问题 | 状态 | 关键点 |
|---|---|---|---|
| P1-006 | Cancel 未释放 reserve / OrderStore 未同步 | 🔧 FIXED | `settle_cancel` 先活动态→Cancelled 成功再释放；client/exchange 两路径；`limit_price≤0` fail-closed；5 测试+mutation |
| P1-007 | Reconcile 未重建 open-order reserve | 🔧 FIXED（集成测试待 Docker） | 启动+后台重建失败**强制 fail-closed 不进入 READY**；`bin/server.rs` |
| P1-008 | on_fill 用 filled notional 抵扣 limit×qty reserve | 🔧 FIXED | 订单级释放按 `limit_price×增量`；成交暴露按执行价；身份/限价未知**保守持有**待对账；8 测试+mutation 7 FAILED（`fast/risk_engine.rs`、`grpc/service.rs`） |
| P1-009 | Unknown Result=Received 被 reconcile 排除 | 🔧 FIXED | `in_doubt_orders()`（Received 且无 exchange id）+ `resolve_in_doubt_orders` 唯一匹配（clientOrderId→唯一 fingerprint，互惠唯一，≥2 即 ambiguous 绝不自动挑）→ `bind_remote_order` Received→Submitted（幂等）；unresolved 保守持有预留、无 TTL 释放路径；`gate_after_reconcile` unresolved→非 READY；15 测试+mutation 2×3 FAILED（`fast/order_state.rs`、`fast/state_sync.rs`、`grpc/service.rs`、`bin/server.rs`；durable-restart DEFERRED per DCR-001 方案 A） |
| P1-010 | Snapshot absolute overwrite + event dual-write | ⛔ WAITING_AUTHORIZATION | 设计 B4 |
| P1-011 | Pagination 未处理 next_cursor | 🔧 FIXED | 三路径游标循环（推进/去重/页数上限/中间页错误）；5 测试 |
| P1-012 | parse_json_f64 invalid/missing→0.0 | 🔧 FIXED | 改 `Result`；非法恢复成交→`unresolved`；3 测试 |
| P1-013 | HMAC decode fallback raw ASCII | 🔧 FIXED | 删 fallback；空/非法 secret fail-closed；2 测试 |
| P1-014 | Global capacity 用 orders.len() | 🔧 FIXED | active-only 计数；terminal 释放容量；3 测试+mutation |
| P1-RISK-UNIT-001 | Python RiskGate shares/USDC 混淆 + 双权威 | ⛔ WAITING_AUTHORIZATION | 设计 B3（最高优先）；已从 R5-002 **剥离**（E4） |

> 基线→新：见 commit `7c75285`。

---

## 二-F、Round 05 完整源码审查与整改方案（2026-10-13）

> **来源**：外部独立审查《Polymarket Weather Trading Bot — Round 05 完整源码审查与整改方案》
> **审查日期**：2026-10-13 ｜ **基线 commit**：`99a89f84caa4643779607a0fd13531a8e689c6d6`（＝当前 HEAD）
> **总体裁决**：🔴 **LIVE = NO-GO**
> **编号口径**：保留现有 Finding ID，不因重新审查而重复创建同一问题。本节的 P0-RISK-001/002、P1-RISK-UNIT-001、P1-010、P1-009/B2、B1/P1-008、B5/R5-005、R5-009 均为既有 Finding 的**再确认或状态更新**；R5-001~R5-009 为 Round 05 整改批次编号。

### 0. 审查结论摘要

| 编号 | 问题 | 严重度 | 本次裁决 | BUG_TRACKER 既有编号 |
|------|------|--------|---------|---------------------|
| P0-RISK-001 | 日亏熔断从未触发（`post_check` 生产零调用） | 🔴 P0 | **CONFIRMED（仍开放）** | 同左（二-C，G1） |
| P0-RISK-002 | 结算窗口保护未在生产注册（2h 禁开 Buy 未接线，未知即放行） | 🔴 P0 | **CONFIRMED（仍开放）** | 同左（二-C，G1） |
| P1-RISK-UNIT-001 | Python 风控混用 shares 与 USDC（双权威 1000 vs 500） | 🟠 P1 | **CONFIRMED 开放，不能关闭** | 同左（二-E，B3） |
| P1-010 | 下单响应状态覆盖风险（非原子 R-M-W） | 🟠 P1 | **PARTIAL（B4 复核：非原子属实，覆盖机制不可达，存在可达隐患）** | 同左（二-E，B4） |
| P1-009（B2） | 未知订单匹配（in-doubt） | 🟠 P1 | **PARTIAL — 未闭环部分属 DCR-001 方案 A 已批准延期** | 同左（二-E） |
| B1 / P1-008 | 预留释放口径 | 🟢 — | **已实施 FIXED，待独立 VERIFIED** | 同左（二-E，B1） |
| B5 / R5-005 | 密钥零化 | 🟢 P1 | **已实施 FIXED，待独立 VERIFIED** | R5-005（本报告 §五） |
| R5-009 | Docker 健康检查 | 🟢 P2 | **⛔ BLOCKED — ENVIRONMENT（设计通过，待 Docker Runtime Evidence）** | 同左（二-D 关联，二-E 注记） |

**裁决要点**：本次审查为 `LIVE NO-GO`。**两条 P0（日亏熔断 + 结算窗口）仍处于「代码可能存在但生产调用链未接线」状态**，是阻止上线的唯一硬阻塞；P1 层存在风险单位不一致（可能导致误拦截与过宽双向偏差）等开放项。

---

### 一、审查环境及证据限制

| 项 | 说明 |
|----|------|
| 审查对象 | 基线 `99a89f84caa4643779607a0fd13531a8e689c6d6`（分支 `p0-blockers-2026-09-13` / `release-2026-10`） |
| 证据类型 | 源码静态审查（调用链追踪）+ 既有测试清单 + 设计文档比对 |
| **限制 1** | **本机无 Docker**（`docker` 命令不存在）→ 所有涉及容器的验收**未实机执行**，R5-009 保持 `BLOCKED — ENVIRONMENT` |
| **限制 2** | **全库 `cargo fmt --check` / `cargo clippy -D warnings` = PRE-EXISTING BASELINE FAILURE**（非本次引入）；**禁止**声称 clippy PASS，**禁止**借整改之名扩大范围清理 baseline lint |
| **限制 3** | 未做真实外网端到端（CLOB / RPC / Gamma API 实调用）；sandbox 环境只有 mock 降级路径 |
| 证据分级 | 三重证据（git diff + 测试 + 生产调用链）齐备方判 `VERIFIED`；仅代码可读 → `FIXED`；缺 Docker/外网 → `BLOCKED` |

---

### 二、P0 整改方案

#### P0-RISK-001 — 日亏熔断从未触发（`post_check` 生产零调用）

**源码证据**
- `RiskEngine::post_check()` 定义存在，但全仓检索**无生产调用点**——`service.rs::place_order` 路径只调 `pre_check` / `check_and_reserve`，成交路径 `on_fill` 亦不触发日亏判定。
- 结果：`daily_loss` 累计值虽可能被写入，但**没有任何门禁读取它并拒绝新单**。

**修改方案**
1. 在 `place_order` 的**风控预检阶段**（`check_and_reserve` 之前或之内）调用日亏门禁；
2. 日亏超限 → **FAIL-CLOSED 直接拒单**（返回 `failed_precondition`），并写 `tracing::error!` 告警；
3. 熔断状态**不得被后续 reconcile 覆盖**（与 `Halted` 闩锁同级，参照 `TradingGate::reconcile_complete` 不覆盖 Halted 的既有语义）；
4. 补「日亏超限 → 拒单」正向测试 + mutation 验证（去掉调用后测试必 FAILED）。

**验收标准**
- `grep -rn "post_check" src/` 显示**至少一处生产调用点**（非 `#[cfg(test)]`）；
- 新增测试断言：日亏累计超限后 `place_order` 返回错误且 **`FastExecutor` 未被调用**（与 P0-018 同款验证方式）；
- 三重证据齐备后由**非实施者**判 `VERIFIED`。

---

#### P0-RISK-002 — 结算窗口保护缺少生产注册

**源码证据**
- 结算窗口守卫（距截止 < 2h 禁开 Buy）逻辑存在于 `RiskEngine`，但**未在 `RiskConfig` / `server.rs` 生产装配中注册或启用**；
- 关键缺陷：当**结算时间未知**（`None` / 解析失败）时当前实现**放行**（fail-open），而非保守拒绝。

**修改方案**
1. 在 `server.rs` 生产装配（`RiskEngine::new(RiskConfig::default())` 处）显式注册结算窗口守卫；
2. 语义改为 **FAIL-CLOSED**：结算时间未知 → **拒绝开 Buy**（而非放行），并在错误文案中区分「已知窗口内」与「结算时间未知」；
3. 补测试三场景：窗口内拒单 / 窗口外放行 / **结算时间未知拒单**；
4. mutation 验证：把未知态改回放行 → 第三场景测试必 FAILED。

**验收标准**
- 生产装配路径可见守卫注册（非仅单测构造）；
- 三场景测试全绿 + mutation 有鉴别力；
- 非实施者三重证据判定 `VERIFIED`。

---

### 三、P1 整改方案

| 编号 | 问题 | 修改方案要点 | 前置条件 |
|------|------|-------------|---------|
| **P1-RISK-UNIT-001**（＝B3） | Python `risk_gate` 混用 shares 与 USDC；`position_notionals` 生产**从未写入**（`converter.py` 不构造、`proto.PositionEntry` 无该字段）→ 全局暴露恒为 shares 口径；两端双权威 1000（Python）vs 500（Rust） | 方案 C：**Rust 权威**，经 proto `RiskState` 下发限额配置；Python 改为纯消费方，删除本地默认 | ⚠️ **proto `RiskState` 增配置字段属冻结契约变更，须 Human 明确授权** |
| **P1-010**（＝B4） | `service.rs:1023-1043` 响应路径「`get` → 本地改副本 → `upsert`」非原子（两次取锁） | 新增 `order_state.rs::mark_submitted_and_update_fill`（单写锁原子 + 终态守卫，语义同 `apply_fill_by_exchange_order_id`）；补跨路径并发测试 | 无（但**所述「成交事件被覆盖」机制不可达**，须按下述可达隐患重述） |
| **P1-009**（＝B2） | in-doubt 解析已实现且有测试，但 `OrderStore` 为内存实现 → 启动 `snapshot_in_doubt_orders()` **恒为空** → in-doubt 约束**仅在进程存活期内成立，跨重启恒为空转** | 无需代码修复（属 DCR-001 方案 A 已批准延期，Human 2026-10-04）；**建议补文档注记**避免被误读为已闭环 | 持久化属 R2-P2 范围 |
| **B1 / P1-008** | on_fill 用 filled notional 抵扣 limit×qty reserve | 已按 `limit_price×增量` 做订单级释放（commit `ee839be`，8 测试 + mutation 7 FAILED） | 待独立 VERIFIED |
| **B5 / R5-005** | 私钥/密码未零化 | `Zeroizing<String>` 承载 `EncryptedKey::decrypt` 返回值 + `pk_bytes`/`derived_key`/`plaintext`；`manager::unlock` 作用域清零（`Cargo.toml` + `zeroize="1"`） | 待独立 VERIFIED |

**P1-RISK-UNIT-001 量化后果**（价 0.10：Python 1000 shares ≈ 100 USDC < Rust 500 USDC → **误拦截**；价 0.90：1000 shares ≈ 900 USDC > 500 USDC → **过宽**）

---

### 四、R5-001 ~ R5-009 状态汇总（Round 05 整改批次）

| 编号 | 问题 | 级别 | 状态 | 落地证据 |
|------|------|------|------|---------|
| **R5-001** | 集中度检查 TOCTOU：分母漏计 pending 预留；**Sell 分支误传 `already_reserved=true`** | 🔴 P0 | ✅ **CONFIRMED 可关闭**（2026-09-18 独立审查） | `risk_engine.rs`：`already_reserved = matches!(side, Side::Buy)`；测试重做（Test A/B/C + mutation Test D：改回 `true` → A/B 必 FAILED）；Rust `338 + 4 + 18 + 1` 全绿。**冻结语义：Buy 创建 reserve 且计算不重复加；Sell 不创建 reserve 但计算必须加、失败不得释放既有 Buy reserve**。**禁止**再修改 R5-001 代码/测试（除非新 Finding） |
| R5-002 | `MultiSourceStrategyParams` 增 `kelly_fraction`/`base_capital`，`_calculate_quantity` 改 `kelly_fraction × base_capital × 价差优势系数` | 🟠 P1 | ⏸️ **未实施**（按伯森先生指示跳过） | 「价差优势」定义与 quantity/USDC 单位口径待确认；已从 P1-RISK-UNIT-001 剥离（E4） |
| **R5-003** | `clean_forecast` 原地修改输入（违反 docstring「不修改原始对象」契约） | 🟡 P1 | 🔧 FIXED | `cleaning.py` 入口 `copy.deepcopy`；新增 `test_cleaning.py`（`cleaned is not fc` + 原对象不变） |
| **R5-004** | `AdaptiveWeighter` 最小权重语义错误 | 🟡 P1 | 🔧 FIXED | `ensemble_fusion.py`：常规归一化 → `max(w,min_weight)` → 二次归一化（超量仅从高于下限源按比例扣回）；不可行退均匀；2 测试 |
| **R5-005** | 私钥/密码未零化 | 🟡 P1 | 🔧 **FIXED**（＝B5，待独立 VERIFIED） | `zeroize="1"` + `Zeroizing<String>`；wallet 13 测试全绿 |
| **R5-006** | clippy 门禁（2 个 `approx_constant` **error**） | 🟢 P2 | 🔧 FIXED | 0 error；警告 82 → 23。⚠️ **行为修正**：`daemon.rs:88` 原 `let _ = rpc.check_health_all();`（非绑定 future，启动健康检查**实际从未执行**）改为 `.await` —— 请独立方确认符合预期。⚠️ `clippy --fix` 曾误删 `service.rs` 中仅测试使用的 `RiskConfig` import，已移入 `mod tests` 修复 |
| **R5-007** | 权益曲线年化因子硬编码 | 🟢 P2 | 🔧 FIXED | `metrics.py` 新增 `_resolve_periods_per_year`：`interval = total_hours/len(curve)`；口径采伯森先生指定式（非 `/(n-1)`）；信息不足回退 `365×4`；测试断言 6h vs 1h 采样 Sharpe 之比 = √6 |
| **R5-008** | 持仓时间用 `token_id` 字典匹配（覆盖错配） | 🟢 P2 | 🔧 FIXED | `metrics.py::_calc_avg_hold_time` 改按 token 维护 OPEN 队列 **FIFO** 一一对应；测试（同 token 两次开平 → 平均 11h，旧实现得 2h） |
| **R5-009** | 假健康检查（`sys.exit(0)` 恒真） | 🟢 P2 | ⛔ **BLOCKED — ENVIRONMENT**（设计通过） | `docker-compose.yml`：`CMD-SHELL` 扫描 `/proc/*/cmdline` **精确匹配 argv 元素** `-m` + `market_discovery`（避免误匹配 healthcheck 自身与 sh 包装）+ `start_period: 20s`。**冻结语义：Container Up ≠ Business Healthy**；entrypoint 只负责「保持容器存活 + 记录 subprocess death」，healthcheck 负责业务健康判断，Docker 负责 healthy/unhealthy。**禁止**改回 `discovery death → entrypoint exit 1 → container restart`；**禁止**在 entrypoint 内自行实现「失败三次」计数。解除需在有 Docker 的主机执行 `docs/DEPLOY_HEALTHCHECK_RUNBOOK.md` §5 五步验收并附证据 |

---

### 五、R5-009 Docker 健康检查整改（详）

```
market_discovery 正常        → container Up → health healthy
market_discovery 异常退出    → entrypoint 不立即退出 → container 继续 Up
                            → healthcheck 检查真实 market_discovery
                            → 连续失败 3 次 → health unhealthy
```

**职责划分（保持，不得回退）**
- **entrypoint**：只负责「保持容器存活 + 记录 subprocess death」
- **healthcheck**：负责「业务健康判断」
- **Docker**：负责标定 `healthy / unhealthy`

**解除 BLOCKED 的验收步骤**：在有 Docker 的主机执行 `docs/DEPLOY_HEALTHCHECK_RUNBOOK.md` §5 的 5 步实机验收，留存 `docker inspect` 健康状态证据 + 日志。

---

### 六、统一测试门禁

| 门禁 | 命令 | 期望 | 本次已知状态 |
|------|------|------|-------------|
| Rust 编译 | `cargo check --target x86_64-pc-windows-msvc` | **0 error** | ✅ 0 error |
| Rust 测试 | `cargo test --target x86_64-pc-windows-msvc` | **全绿** | ✅ **401 passed**（lib 379 + server 4 + p0_production_validation 18），0 failed |
| Rust 文档测试 | `cargo test --doc` | 全绿 | ⚠️ FAILED — `ERROR_PIPE_BUSY`(code 231) spawn `rustc.exe` 失败，**本机环境级管道耗尽，非代码缺陷**（`lib.rs:10` 不在改动范围） |
| Rust Lint | `cargo fmt --check` / `cargo clippy -D warnings` | — | ⚠️ **PRE-EXISTING BASELINE FAILURE**（非本次引入）；**禁止声称 PASS**，**禁止借整改之名扩大范围清理** |
| Python 测试 | `python -m pytest` | **全绿** | ✅ **109 passed / 0 failed / 0 error**（基线 103，+6） |
| Docker | `docker compose config` + 容器内健康探针 | 实机验收 | ⛔ **未执行**（本机无 Docker） |

> 环境：Rust `PROTOC=C:/Users/32389/tools/protoc/bin/protoc.exe` + `--target x86_64-pc-windows-msvc`；Python venv `C:\Users\32389\.workbuddy\binaries\python\envs\default\Scripts\python.exe`

---

### 七、推荐整改顺序

1. **P0-RISK-001**（日亏熔断接线）→ **P0-RISK-002**（结算窗口 FAIL-CLOSED 注册）—— 唯一硬阻塞，先闭环；
2. **P1-RISK-UNIT-001 / B3**（风险单位统一）—— **须先取得 Human 对 proto 契约变更的授权**；
3. **P1-010 / B4**（响应路径原子化 + 跨路径并发测试）；
4. **R5-009**（Docker Runtime Evidence，需有 Docker 主机）；
5. **B2 / P1-009** 补文档注记（说明启动 in-doubt 约束依赖 R2-P2 持久化、当前空转）；
6. B1/P1-008、B5/R5-005 补独立三重证据 → VERIFIED。

---

### 八、审计关闭规则

- **六态状态机**：`OPEN → FIXED → VERIFIED → CLOSED`（另 `BLOCKED` / `WAITING_AUTHORIZATION`）。**`FIXED` ≠ 修好**。
- **三重证据**：`git diff` + 测试（含 mutation 鉴别力验证）+ **生产调用链**，三者齐备且由**非实施者**判定，方可 `VERIFIED`。
- **实施者不自证**：实施者（OpenCode / 旺财）只登记 `FIXED` / `WAITING_AUTHORIZATION` / `BLOCKED`，**不写 VERIFIED / CLOSED**。
- **FAIL-CLOSED 优先**：任何「未知 / 解析失败 / 缺字段」场景一律保守拒绝，不得 fail-open。
- **冻结契约**：R5-001 代码/测试、R5-009 代码不得以任何非新 Finding 的理由改动；proto 契约变更须 Human 授权。
- **禁止以整改为名扩大范围**：不得为全库 lint 改动 `RiskConfig` 阈值 / 集中度阈值 / Position Limit / Buy-Sell 语义 / 重构 `RiskEngine` / 策略 / Kelly / 资金控制逻辑。

---

### 九、最终裁决

> 🔴 **LIVE = NO-GO**
>
> 阻塞项：**P0-RISK-001**（日亏熔断生产零调用）+ **P0-RISK-002**（结算窗口未注册且未知即放行）。
> 开放 P1：**P1-RISK-UNIT-001**（风险单位不一致，双向偏差）、**P1-010/B4**（响应路径非原子）、**P1-009/B2**（跨重启 in-doubt 空转，已批准延期）。
> 环境阻塞：**R5-009**（无 Docker Runtime Evidence）。
>
> **在 P0 两项闭环并取得独立 VERIFIED 之前，不得上线实盘。**

---

## 三、P1 清单（10 项，全部完成 09-10）

| 新编号 | 问题 | 处置 | 落地 |
|--------|------|------|------|
| P1-001 | API Key Fail Gate | ✅ 已实现 | server.rs API Key 失败 → halt + return；TradingGate halt 优先（reconcile 不覆盖） |
| P1-002 | Network Health Check | ✅ 已实现 | ClobClient.health_check()（GET /time）+ server.rs 启动健康检查 FAIL-CLOSED |
| P1-003 | Retry Backoff | ✅ 已实现 | ClobClient.get_with_retry() 指数退避（0.5s→1s→2s，5xx/网络错误重试） |
| P1-004 | API Key 流程优化 | ✅ 已实现 | create_or_derive 仅 400「已存在」时回退派生，网络/认证错误直接返回 |
| P1-005 | Order Lifecycle | ✅ 已补 | OrderState 补 mark_cancelled/rejected/expired + 4 个生命周期测试 |
| P1-006 | Fill 同步 | ✅ 已实现 | ExecutionServiceImpl.on_market_trade()：异步成交 → RiskEngine.on_fill + FillLedger + OrderStore |
| P1-007 | State 恢复机制 | ✅ 已实现 | server.rs 后台周期对账（每 30s full_reconcile）+ 差异告警 |
| P1-008 | Settlement Observation | ✅ 已补 | market_events.mark_settled 回填盈亏测试（settle_by_token 链路） |
| P1-009 | Ensemble Calibration | ✅ 已补 | test_ensemble_fusion.py 5 个测试（Brier/权重/贝叶斯收敛/CI） |
| P1-010 | Order Dedup claim 占位失败清理（P0-004 遗留） | ✅ 已实现 | OrderStore.mark_rejected + claim_or_retry；place_order 6 失败路径清理占位；4 测试全绿 |

**测试结果（09-07）**：Rust 241 单元测试 + 9 集成测试全绿；Python 5 测试全过。

> 已知限制：P1-006 的 `on_market_trade` 已实现但 `MarketEventPoller` 尚未接入启动流程（需 L2 认证的 `/data/trades` 轮询驱动），异步成交的端到端触发留待 Shadow Trading 阶段验证。

---

## 三-B、P1 待修清单（09-15 外部审核，编号对齐）

> 来源：`Polymarket_P0_P1_Current_Audit.md`（伯森先生 09-15 提供）。
> **编号口径**：沿用审核文件的 P1 编号（`P1-026` 起；审核文件 5 个未编号项按顺序续编 `P1-029`~`P1-033`）。旧 `P1-001`~`P1-010`（上方「三」）是 09-10 那批已完成的 P1，编号区间不连续但互不冲突。

| 审核编号 | BUG_TRACKER 编号 | 问题 | 状态 | 关键点 |
|---------|----------------|------|------|--------|
| P1-026 | P1-026 | Multi-partial-fill 成交量覆盖 | 🟢 FIXED（09-14） | `apply_fill(delta)` 增量累计替代绝对覆盖 `update_fill`；`on_market_trade` 改 `st.apply_fill(trade.size)`；测试 30→70→100 |
| P1-027 | P1-027 | OrderStore 并发 fill lost update | 🟢 FIXED（09-16） | 新增 `apply_fill_by_exchange_order_id()` 单写锁内完成「定位+增量+写回」，`on_market_trade` 改走该方法；+2 测试（并发 10×10=100 无 lost update + 非法增量拒绝） |
| P1-028 | P1-028 | mark_rejected 与 fill 状态竞争 | 🟢 FIXED（09-16） | `mark_rejected` 改单写锁内原子状态转换 + 终态保护（Filled/Cancelled/Rejected/Expired 不覆盖）；+2 测试（Filled 不倒退 + 并发 fill/reject 无回归） |
| P1（未编号） | P1-029 | FillLedger 多笔 fill 被覆盖 | 🟢 FIXED（09-16） | 拆双索引：`fills`（fill 级历史，key=trade_id 空则 order_id）+ `orders`（订单级聚合 `OrderFill`，累计 size/加权平均价/fee/滑点）；`record` 不再覆盖式 insert，`list/stats/to_json` 改 fill 级，`get/settle/cancel` 保持订单级；+2 测试（多笔不覆盖 + 多笔结算独立盈亏） |
| P1（未编号） | P1-030 | StateSync partial-fill recovery 只恢复第一笔 | 🟢 FIXED（09-16） | 新增 `find_trades_for_order`（返回全部 trades），`build_recovery_plan` 每笔 partial fill 生成一个 `MarkOrderFilled`（`apply_fill` 增量 + `claim_trade` 按 trade_id 去重），不再 `.find()` 只取第一笔；+2 测试（find 全部 + 多笔生成多动作） |
| P1（未编号） | P1-031 | Recovery 后 reserve 未完整重建 | 🟢 FIXED（09-16） | `risk_engine.rs` 新增 `rebuild_reserve`（skip 上限校验，恢复以远端为权威）；`service.rs` 新增 `rebuild_open_order_reserves`（用 OrderStore 的 token_id + open order price × remaining_size 按 token 聚合回填 reserved，跳过 SELL）；`server.rs` 恢复后非致命接线；+4 测试（重建计入后续检查 + 忽略非法 + SELL 跳过） |
| P1（未编号） | P1-032 | Background StateSync 不自动降级 TradingGate | 🟢 FIXED（09-16） | 后台对账发现差异/恢复动作/对账失败时，先 `reconcile_complete` 降级 Ready→Reconciling（停新单，`ensure_ready` 拦截）；恢复后重新快照 + 复核对账，一致才回 Ready，仍不一致/复核失败保持 Reconciling（FAIL-CLOSED）；halt 闩锁不被覆盖；+2 测试（降级→恢复回 Ready 全闭环 + 持续差异保持 Reconciling） |
| P1（未编号） | P1-033 | Reserve Rollback Lifecycle | 🟢 FIXED（09-16） | ① `check_and_reserve` 集中度失败后回滚预留（仅 Buy）；② HTTP 200+`success=false` 释放预留 + mark_rejected；③ 网络超时/5xx 判为「未知结果」→ `is_unknown_outcome()` 保留预留 + 返回 Pending（保守，不低估暴露），确定失败（4xx/本地）才释放；+4 测试（集中度回滚 + is_unknown_outcome 分类×2）。指纹级精确确认留待后续 |
| P1（未编号） | P1-034 | `consume_trade_events` 把 `Lagged` 当致命错误 | 🟢 FIXED（09-16） | 旧实现 `while let Ok(...)` 遇 `broadcast::RecvError::Lagged(n)` 直接退出循环且无日志 → 成交链路静默永久停摆（现场仍在交易但成交不记账）。改为 `loop + match`：`Lagged` → `tracing::error!` 告警（含丢失条数，需 StateSync 兜底）+ continue；`Closed` → 告警后 return。保留「仅消费 Trade」+「幂等由下层单点」；+3 测试（Lagged 不致命 / Closed 才退出 / 非 Trade 不记账）。**已做回归验证：还原旧实现后 `test_consume_trade_events_lagged_not_fatal` 必 FAILED**，证明测试有鉴别力 |
| P1（未编号） | P1-035 | 空 `trade_id` 绕过 fill 幂等（P0-015 残余边界风险） | 🟡 FIXED / Verification Pending（09-17 Round04 独立审查：未升 VERIFIED） | 保持 `claim_trade()` 原语义不变；`poll_trades()` 循环入口对空/空白 `t.id` 告警并丢弃；StateSync `build_recovery_plan()` 对空/空白远端 trade ID 不生成 `MarkOrderFilled`；`apply_recovery()` 再加最终 fail-closed 防线，拒绝空/空白 `trade_id` 进入 `on_market_trade`。正常成交事件必须拥有非空唯一 trade_id。全量 Rust 测试 317 + 集成 18 + 文档 1 全绿；+3 针对性测试（`test_poll_trades_drops_empty_trade_id` / `test_build_recovery_plan_skips_empty_trade_id` / `test_apply_recovery_rejects_empty_trade_id`），**已做鉴别力验证：三处守卫置否后 3 测试必 FAILED**。**Round04 结论：代码+测试在，但不得由实施者自证 → 维持 FIXED，待独立方补三重证据后升 VERIFIED** |

**P1-035 定级待伯森先生裁决**：原文判断「是否构成 P0 取决于上游能否产生空 trade_id」。查证显示代码层面**允许**空 id 产生（serde(default) 且反序列化不失败），但 API 实际是否返回无 id trade 属外部事实。鉴于后果严重（仓位无限累加）而修复成本极低（几行 fail-closed），倾向按 P0 处理；若确认 API 保证非空 id，则降为 P1 防御性校验。

**推荐修复顺序（审核给）**：
1. **OrderStore/Fill**：P1-027 → P1-028 → P1-029
2. **StateSync**：P1-030 → P1-031 → P1-032
3. **Reserve Lifecycle**：P1-033

**当前进度**：**P0-015 VERIFIED（09-16，伯森先生复核确认）**；P0 其余项已清零（09-15）；P1-026 FIXED 待伯森先生确认关闭；P1-027~033 FIXED（09-16，P1 待修批次完成）；P1-034（`consume_trade_events` Lagged 缺陷）FIXED（09-16）；**P1-035（空 trade_id 绕过幂等）🔴 待修 + 待定级**。

---

## 四、与 8/25 旧清单对照（变化追踪）

| 类型 | 项 | 说明 |
|------|-----|------|
| ✅ 对应 | P0-001/002/003/004/005/006 | 与旧 P0-01/02/03/05 等一一对应 |
| 🔻 降级 | P0-06、P0-12、P0-13、P0-14 → P1-005/006/007 | post_check、pending orders、后台循环从 P0 降到 P1 |
| ➕ 新增 | P1-001/002/003/004/008/009 | 6 项旧清单未覆盖 |
| ✅ 已验证 | 旧 P0-07（TradingGate 状态机）、P0-11（TradingGate 进 READY） | 代码闭环 + 补 7 测试全过 → **VERIFIED** |
| ✅ 已确认 | 旧 P0-10（full_reconcile 接入） | 已接入启动流程（server.rs:138-141），见 P0-007 |
| ✅ 已确认 | 旧 P0-04（FastExecutor 真实执行路径） | 已实现于 FastExecutor 15 子模块（185 tests），place_order 全链路代码闭环；真实 CLOB 下单端到端留待实盘/Shadow Trading |

---

## 五、关键风险提示 🔴

1. ~~TradingGate 相关项疑似整体消失~~ → **✅ 已验证完成**：旧 P0-07（状态机）+ P0-11（进 READY）代码闭环，补 7 个测试全过（总计 229 tests），达 VERIFIED。GPT 新清单未列是因其已判定完成，非遗漏。
2. **P0 降级到 P1 理由已记录**：post_check 成交闭环、pending orders、后台对账循环从 P0 降到 P1，理由 = 逻辑已实现且有单测，但真实成交触发依赖 L2 `/data/trades` 轮询，sandbox 无外网无法端到端验证 → 推迟到 Shadow Trading 阶段。⚠️ 资金安全相关项降级仍待 Human 显式确认归档（记录见 `MASTER_TASKS.md`「P0 → P1 降级记录」）。
3. **「已修」≠「已验证」**：P0 全部 7 项（001~007）已 VERIFIED（09-09）；P1 9 项已完成（09-07）。

---

## 六、下一步

1. 向 GPT 确认两点：P0 降级 P1 的理由、P0-04（FastExecutor 真实执行路径）去向。（TradingGate 两项已确认 VERIFIED，无需再问。）
2. 编号体系已变（P0-01 → P0-001），`MASTER_TASKS.md` 需对齐重排（待确认后执行）。
3. 授权后启动 Round 4 验证：对 16 项逐项核 git diff + 测试 + 调用链，产出 `audits/round_04/` 报告。
4. **P1 项（P1-001~009）明日进行**；P0-004 遗留「claim 占位失败清理」记 P1 一并处理。

---

## 二-G、Round 05 独立终审裁决与 Round 06 双 AI（Gemini × GPT）交叉核验清单（2026-10-14）

> **审查基线**：commit `7c75285` / `a2dde81` + 全仓代码包交叉比对。
> **状态口径**：由非实施者（独立审查 AI）完成三重证据核验，正式更新 `VERIFIED` 状态，并锁定下一阶段（Round 06）同步修复清单。
> **编号说明**：本节内容原稿标为「二-F」，因文档已存在 2026-10-13 的「二-F」章节，为**统一编号不冲突**，本节落为 **二-G**（内容未作删改）。
> **统一编号规则**：沿用既有 `<级别>-<域>-<三位序号>` 约定（如 `P0-RISK-001` / `P0-EXEC-004`）。本轮新增域：**BUYNO**（策略 / 聚合器 / Kelly 仓位）、**GAMMA**（上游市场发现）、**FORECAST**（上游气象映射）。
> ⚠️ **重要**：`B1` / `B2` **不得单独作为 Finding ID**——它们在本文档中已表示 Round 05 设计批次 B1/B2（B1＝预留释放、B2＝in-doubt 对账）。本轮两项属 B1/B2 的**二审追加发现**，已规范化为 `P1-RISK-015` / `P1-CONSIST-016`，源标签仅作别名保留。

### 1. Round 05 正式晋升 `VERIFIED` 清单（已闭环，🔒 禁止回退）

| 编号 | 模块 | 独立审查裁决 | 核验结论摘要 |
|------|------|-------------|-------------|
| **R5-001 + A7** | `fast/risk_engine.rs` | ✅ **VERIFIED** | 集中度检查分子（`market_committed`）与分母（`committed_total`）均统一为 `filled + reserved`，`Sell` 分支 `already_reserved` 判定与回滚保护严密，测试覆盖完整。 |
| **R5-002 + A8** | `multi_source.py` / `config.py` | ✅ **VERIFIED** | 指数饱和 `edge = 1 - exp(-|dev|/σ)`（采用滚动残差 σ）、USDC 层 `cap` 后除以 `cost` 转 shares、`AGG_*` 单一资本权威注入及 T1–T12 + A8 突变验证全部通过。 |
| **R5-003** | `weather_data_ingestion/cleaning.py` | ✅ **VERIFIED** | `copy.deepcopy` 兑现不可变输入契约，单测通过。 |
| **R5-004** | `strategy_engine/ensemble_fusion.py` | ✅ **VERIFIED** | 归一化后抬升最小权重并按比例扣回超量，边界退化处理正确。 |
| **R5-007 / R5-008** | `strategy_engine/backtest/metrics.py` | ✅ **VERIFIED** | 真实时间戳反推年化因子 `(last-first)/(n-1)` 及同 token FIFO 持仓时间配对均已落地并验证。 |
| **A2（P1-007）** | `bin/server.rs` / `grpc/service.rs` | ✅ **VERIFIED** | `gate_after_reconcile` 在预留重建失败或 `unresolved > 0` 时强制保持非 `Ready`（Fail-Closed）。 |
| **A4（P1-012）** | `fast/state_sync.rs` | ✅ **VERIFIED** | `parse_json_f64` 拒绝 `NaN/Inf/非法值`，异常恢复成交进入 `unresolved_orders`。 |
| **A5（P1-013）** | `clob/signing/hmac_sig.rs` | ✅ **VERIFIED** | 已移除 raw ASCII fallback，空或非法 Base64 secret 严格 Fail-Closed。 |
| **A6（P1-014）** | `fast/mempool.rs` | ✅ **VERIFIED** | 全局容量仅统计 active 订单，终态订单正确释放容量。 |

> 🔒 **以上 9 项禁止回退**（no-reopen）。任何后续改动若触碰这些文件/行为，须重新走三重证据核验。
> 📌 **本节同步覆盖「二-F」相关条目状态**：R5-001/002/003/004/007/008 由「已有实现、待测试验收」升为 **VERIFIED**；A2/A4/A5/A6 由「FIXED 待验证」升为 **VERIFIED**。

---

### 2. Round 06 待修清单（双 AI 交叉核验：核心层 + 上游同步解决）

| 统一编号 | 级别 | 问题域 | 问题描述与根因定位 | 状态 | 归属任务组 |
|---------|------|--------|-------------------|------|-----------|
| **P0-BUYNO-001** | **P0** | 策略 / 聚合器 / Kelly 仓位 | **BUY_NO 概率、市价二次取反与限价错位（双 AI 共识）**：<br>① `multi_source.py`（及偏差策略）在 `BUY_NO` 时传入 `target_price = P(YES)`，但 `market_price` 传入了 `no_price (1 - P_mkt(YES))`；<br>② `position_sizer.py:_kelly` 在 `BUY_NO` 分支对 `price` 再算一次 `1 - price`（二次取反还原成 YES 价），且未将获胜概率转为 `p_win = 1 - p`，导致真实看空信号算出 `f* < 0`、`quantity = 0`；<br>③ 下游 `AggregatedOrder` 将 `P(YES)` 直接作为 `BUY_NO` 的下单限价发往 Rust，导致挂单价与 USDC 敞口计算失真。 | ⬜ TODO | **R6-G1** |
| **P1-RISK-UNIT-001（B3）** | **P1** | Python 风控闸门 | **RiskGate 份额（Shares）与名义价值（USDC）单位混淆**：<br>`risk_gate.py` 的 `_check_position_limit` 与 `_check_global_exposure` 直接将 `order.quantity`（shares）与 `max_position_per_market`（USDC）相加比对；且 `RiskState.position_notionals` 缺少按方向成本计算的统一辅助方法。 | ⬜ TODO | **R6-G1** |
| **P0-GAMMA-001** | **P0** | 上游市场发现（`market_discovery`） | **Gamma API 字段 JSON 字符串解析失败与静默 Mock 回退**：<br>`market_discovery/models.py` 假设 `outcomes` 与 `clobTokenIds` 为原生列表，但官方 API 常返回序列化 JSON 字符串（如 `'["Yes", "No"]'`），且 V2 市场可能使用 `positionIds`；`gamma_client.py` 在解析失败或网络异常时**静默回退到 Mock 假市场**，实盘存在**误交易假 token 风险**。 | ⬜ TODO | **R6-G2** |
| **P0-FORECAST-001** | **P0** | 上游气象映射（`forecast_utils.py`） | **非降水指标返回物理量而非 [0, 1] 概率**：<br>`forecast_utils.py` 对温度返回摄氏度均值、对风速返回 m/s，后续策略却将其当作 `0~1` 概率与 `market.implied_probability` 相减计算偏差。必须根据市场阈值规则转换为概率，或对未配置阈值的非概率指标**显式跳过（Fail-Closed）**。 | ⬜ TODO | **R6-G2** |
| **P1-RISK-015**<br><sub>（源标签 `P1-B1-LATEFILL`）</sub> | **P1** | Rust 成交与撤单预留（`service.rs` / `order_state.rs`） | **撤单后迟到成交导致预留二次释放与终态倒退（B1 二审发现）**：<br>订单经 `settle_cancel` 转为 `Cancelled` 并释放全部 `remaining_size * limit_price` 预留后，若收到在途迟到 `TradeEvent`，`on_market_trade` **未检查订单是否已处于终态或剩余可释放额度**，再次按 `trade.size * limit_price` 释放预留（**偷扣同 token 其他挂单预留**），且将 `Cancelled` **倒退回** `PartialFilled/Filled`。 | ⬜ TODO | **R6-G3** |
| **P1-CONSIST-016**<br><sub>（源标签 `P1-B2-INDOUBT`）</sub> | **P1** | Rust 状态同步（`state_sync.rs` / `server.rs`） | **已立即成交的 In-Doubt 订单永久锁死 TradingGate & 指纹时间戳过严（B2/A3 二审发现）**：<br>① 下单超时处于 `Received`（无 exchange_id）的订单若**已在交易所立即全部成交**，不会出现在 `/data/orders`（open orders）中，`resolve_in_doubt_orders` 仅查 open orders 导致该单**永远 `unresolved`**，`TradingGate` **永久卡死在 `Reconciling`**；<br>② `fingerprint_match` 要求 `created_at` **秒级严格相等**，跨秒延迟即匹配失败；<br>③ `A3` 分页 `next_cursor` 直接拼接**未做 URL 编码**。 | ⬜ TODO | **R6-G3** |

**Round 06 任务组归口**

| 任务组 | 范围 | 含 |
|--------|------|-----|
| **R6-G1** | Python 策略 / 风控核心层 | P0-BUYNO-001（P0）、P1-RISK-UNIT-001（P1） |
| **R6-G2** | 上游数据链路 | P0-GAMMA-001（P0）、P0-FORECAST-001（P0） |
| **R6-G3** | Rust 执行层后置缺陷 | P1-RISK-015、P1-CONSIST-016 |

> **Round 06 新增 3 项 P0**（P0-BUYNO-001 / P0-GAMMA-001 / P0-FORECAST-001）→ 总体状态维持 🔴 **LIVE = NO-GO**。
> ⚠️ `P1-RISK-UNIT-001` 为**再登记**（非新建），沿用既有编号，详见「二-E」B3 与「二-F」§三。

---

### 3. 过时审查项防返工声明（Already Verified — 🔒 DO NOT RE-OPEN）

以下由外部早期快照审查提及的问题，在当前分支**均已修复并验证闭环**，实施者（OpenCode）**严禁重复修改**：

1. **Docker 钱包密码文件读取**：已由 `P0-002`（`server.rs:load_wallet_password`）闭环，已优先读取 `WALLET_PASSWORD_FILE`。
2. **下单前 OrderStore 原子占位去重**：已由 `P0-004` / `P1-010`（`OrderStore::claim_or_retry` 单写锁原子占位）闭环。
3. **Rust 下单预留与实时成交记账接线**：已由 `P0-008`（`check_and_reserve`）与 `P0-013/014/015`（`on_market_trade -> on_fill` + `claim_trade` 幂等）闭环。
4. **Python pytest 挂起与 fixture 缺失**：已由 `ENV-001` 与 `TST-001`（`conftest.py` + session loop）闭环，当前基线 **126 passed**。
