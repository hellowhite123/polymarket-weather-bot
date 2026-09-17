# MASTER_TASKS.md

**唯一工程修改任务来源。** 实施者（Work Buddy）默认只以本文件作为代码修改授权依据。

- 未经本文件明确授权的任务，实施者不得自行改动交易语义 / Risk Model / Strategy / 资金控制逻辑。
- 每个任务一次只做一个 Task Group，完成后走 `docs/CODE_REVIEW_STANDARD.md` 的验收门禁。
- 状态机：`TODO → IN_PROGRESS → FIXED → VERIFIED → CLOSED`（VERIFIED 由非实施者判定）。

---

## 活跃任务

### 阶段一（09-13 授权开工）：Trade 一致性链 P0-013 / P0-014 / P0-015

> 三者耦合，须一起实现。修复顺序：归属 → 接线 → 幂等。

#### P0-013 Trade 归属 attribution

- **Status**: FIXED（09-13：`attribute_order` 归属 + 6 测试全绿）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/market_events.rs`
- **Forbidden**: `strategy_engine/`、`risk_engine.rs`、`executor.rs`、`grpc/service.rs`（本项不改）
- **Required Tests**: `poll_trades` 按 `trader_side` 正确归属（TAKER→taker_order_id / MAKER→maker_orders 内自己的 order）
- **Acceptance**: 不再无条件用 `taker_order_id` 当 `order_id`；`TradeEvent` 携带 `trade_id`/`trader_side`/`owner` 完整 metadata。

#### P0-014 MarketEventPoller → ExecutionService 接线

- **Status**: FIXED（09-13：`consume_trade_events` 接线 + 测试全绿；默认关 `MARKET_EVENTS_ENABLED=1`）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/bin/server.rs`
  - `polymarket-rust/polymarket-rust/src/grpc/service.rs`
- **Forbidden**: `strategy_engine/`、`risk_engine.rs`
- **Required Tests**: 事件 consumer 订阅 `MarketEvent::Trade` → `on_market_trade()` 串起来
- **Acceptance**: 生产级 consumer 存在；默认以环境开关禁用真实轮询（sandbox 无外网），启用后 `poll → Trade → on_market_trade → RiskEngine/OrderState/FillLedger` 闭环。

#### P0-015 Fill 幂等 / 双路重复记账

- **Status**: FIXED（09-13：`FillLedger.claim_trade` + `trade_id` 字段 + 幂等测试全绿）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/fill_records.rs`
  - `polymarket-rust/polymarket-rust/src/grpc/service.rs`
- **Forbidden**: `strategy_engine/`、`risk_engine.rs`（不改 RiskEngine 计算逻辑，仅加幂等去重）
- **Required Tests**: 同一 `trade_id` 重复投递只记账一次；`FillRecord` 以 `trade_id` 为 fill 级 identity，`order_id` 作关联字段。
- **Acceptance**: `on_market_trade` 入口 `atomic dedup(trade_id)`，已处理则忽略。

> ✅ **记账归属决策已定（09-13，伯森先生选方案 X）**：成交记账唯一走路径 B（`on_market_trade`）。路径 A（`place_order`）下单只更新 OrderStore 状态，已移除即时 `on_fill` + `FillLedger.record`。仓位预留由 P0-008 check-and-reserve 解决。

### 阶段二（09-13 授权开工）：资金并发安全 P0-008 ~ P0-012

> 与阶段一衔接：路径 A 不再即时记账后，仓位预留交给 P0-008 原子 check-and-reserve；P0-009 输入校验是风控入口第一道闸；P0-010/011 是 Nonce 并发安全；P0-012 是 Mempool 容量原子。

#### P0-008 Risk check-and-reserve 原子化

- **Status**: FIXED（09-13：`PositionLimits::check_and_reserve` 原子预留 + `release_reserve` 回退 + `post_check` 冲抵预留；`RiskEngine::check_and_reserve` 接入 `place_order`；4 测试全绿）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`
  - `polymarket-rust/polymarket-rust/src/grpc/service.rs`
- **Forbidden**: `strategy_engine/`、`executor.rs`、交易语义 / Risk Model 阈值
- **Required Tests**: `test_check_and_reserve_blocks_second_order` / `test_check_and_reserve_global_limit` / `test_on_fill_converts_reserve_to_filled` / `test_release_reserve_frees_capacity`
- **Acceptance**: 并发下单不会同时通过仓位上限（预检与预留在同一写锁内原子完成）；下单失败/无 FastExecutor 时释放预留；真实成交 `on_fill` 把预留冲抵为 filled。

#### P0-009 Risk 输入校验 NaN/Inf/负值

- **Status**: FIXED（09-13：`validate_order_inputs` 拒绝 notional/qty 的 NaN/Inf/≤0；`RiskConfig::validate` 启动 fail-fast；5 测试全绿）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`
- **Forbidden**: `strategy_engine/`、交易语义
- **Required Tests**: `test_reject_nan_notional` / `test_reject_inf_notional` / `test_reject_negative_notional` / `test_reject_zero_or_negative_qty` / `test_risk_config_validate_invalid`
- **Acceptance**: 风控入口对 NaN/Inf/负值/零数量一律拒单；非法 `RiskConfig` 启动即 panic（fail-fast）。

#### P0-010 Nonce 并发分配原子化

- **Status**: FIXED（09-13：`NonceTracker.next_local` 用 `AtomicU64::fetch_add` 原子递增游标替代「读 max + 写 insert」两步；并发测试 800 nonce 唯一）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/nonce_manager.rs`
- **Forbidden**: `strategy_engine/`、交易语义
- **Required Tests**: `test_nonce_concurrent_allocation_unique`
- **Acceptance**: 多线程并发 `next_nonce()` 各拿唯一 nonce，无重复。

#### P0-011 Nonce 确认语义状态机

- **Status**: FIXED（09-13：`NonceState` 枚举 Allocated/Pending + `HashMap<u64, NonceState>` 替代 `HashSet<u64>`；`mark_pending`/`nonce_state` 新增；分配游标与确认解耦）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/nonce_manager.rs`
- **Forbidden**: `strategy_engine/`、交易语义
- **Required Tests**: `test_nonce_state_machine_transitions` / `test_nonce_out_of_order_confirm_no_skip` / `test_nonce_sync_advances_allocator`
- **Acceptance**: 明确 Allocated→Pending→Confirmed/Failed 状态转换；乱序确认不导致 next_nonce 跳过 pending 或重复已确认。

#### P0-012 Mempool capacity TOCTOU

- **Status**: FIXED（09-13：`PendingOrderTracker::try_submit` 把 capacity check + reservation 合入同一写锁；`submit` 委托 `try_submit`；并发测试 8 线程仅 2 笔成功）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/mempool.rs`
- **Forbidden**: `strategy_engine/`、交易语义
- **Required Tests**: `test_try_submit_atomic_capacity`
- **Acceptance**: 并发 submit 不会突破 per-token/global capacity；锁顺序统一「先 orders 后 counts」避免死锁。

> ✅ **阶段二全部闭环（09-13）**。

### 阶段三（09-13 授权开工）：状态恢复 P0-007B

#### P0-007B full_reconcile 自动恢复

- **Status**: FIXED（09-13：`full_reconcile_with_recovery` + `build_recovery_plan` + `ExecutionService::apply_recovery` + `RiskEngine::sync_position`；15 测试全绿）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/state_sync.rs`
  - `polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`（仅加 `sync_position` 对账重建，不改 Risk Model 阈值）
  - `polymarket-rust/polymarket-rust/src/grpc/service.rs`
  - `polymarket-rust/polymarket-rust/src/bin/server.rs`
- **Forbidden**: `strategy_engine/`、交易语义 / Risk Model 阈值 / 资金控制逻辑
- **Required Tests**: `test_build_recovery_plan_*`（成交/非成交/仓位重建/unresolved）+ `test_classify_vanished_order_*` + `test_sync_position_*` + `test_apply_recovery_*`
- **Acceptance**: `full_reconcile` 从「只检测」升级为「检测 + 自动恢复」——消失订单通过 `/data/trades` 分类终态（Filled → 补记 fill；NonFilled → 标记终态），仓位差异以远端为权威重建；trades 拉取失败时消失订单进入 unresolved 保持 FAIL-CLOSED；启动 + 后台 30s 循环均应用恢复，恢复后复核对账驱动 TradingGate。

---

## 已闭环任务（历史）

### P0-02 Docker Secret 与环境变量一致性

- **Status**: ✅ DONE（09-09 修复 + 4 测试全绿 + commit a963231）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `Dockerfile`
  - `docker-compose.yml`
  - `docker-entrypoint.sh`
  - `polymarket-rust/polymarket-rust/src/wallet/keystore.rs`
  - `polymarket-rust/polymarket-rust/src/bin/server.rs`
- **Forbidden**: `strategy_engine/`
- **Required Tests**:
  - `secret_loaded_consistently_in_docker`
- **Acceptance**: Docker 侧 `WALLET_PASSWORD_FILE` 与 Rust 侧 `WALLET_PASSWORD` 读取链路统一，无不一致风险。

---

### P1-010 Order Dedup claim 占位失败清理（P0-004 遗留）

- **Status**: ✅ DONE（09-10 修复 + 4 测试全绿；代码改动待 Human review 后 commit）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/order_state.rs`
  - `polymarket-rust/polymarket-rust/src/grpc/service.rs`
- **Forbidden**: `strategy_engine/`、`polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`
- **Required Tests**:
  - `claim_retry_after_rejected`
  - `failed_place_order_marks_rejected`
- **Acceptance**: `claim()` 占位后任一下单失败路径（非 OPEN / 熔断 / RiskEngine 缺失 / 风控拦截 / fast path 失败 / 未初始化）将占位标记为 `Rejected`；幂等检查对 `Rejected/Expired/Cancelled` 终态允许重新占位下单，进行中/已成交仍去重。

---

## 旧清单对照（P0-01~P0-14 → 新编号，全部闭环）

> 8/25 第一版 P0 清单（14 项）已全部映射到 `BUG_TRACKER.md` 新编号（P0-001~007 + P1-001~010），全部闭环。下表为对应关系，防止旧任务被误重新执行。

| 旧编号 | 内容 | 新编号 | 状态 |
|--------|------|--------|------|
| P0-01 | FastExecutor API Key / Warmup | P0-001 | VERIFIED（09-08） |
| P0-02 | Docker Secret 一致性 | P0-002 | VERIFIED（09-09） |
| P0-03 | RiskEngine pre_check 接入 | P0-003 | VERIFIED |
| P0-04 | FastExecutor 真实执行路径 | 执行层 15 子模块（185 tests）+ P0-001 调用链 | 已实现；真实 CLOB 下单端到端留待实盘/Shadow Trading |
| P0-05 | 幂等保护 | P0-004 | VERIFIED |
| P0-06 | post_check 基础实现 | P1-005 / P1-006 | DONE |
| P0-07 | TradingGate 状态机 | 保持旧编号 | VERIFIED |
| P0-08 | StateSync 仓位接口 | P0-007 | VERIFIED |
| P0-09 | Position 对账逻辑 | P0-007 | VERIFIED |
| P0-10 | full_reconcile 接入启动 | P0-007 | VERIFIED |
| P0-11 | TradingGate 进入 READY | 保持旧编号 | VERIFIED |
| P0-12 | Pending Orders 状态同步 | P1-005 / P1-007 | DONE |
| P0-13 | 成交回报连接 post_check | P1-005 / P1-006 | DONE |
| P0-14 | 后台周期对账循环 | P1-007 | DONE |

---

## P0 → P1 降级记录

> 资金安全相关项降级按协议需 Human 显式确认。以下理由已记录，待伯森先生最终拍板归档。

| 项 | 降级理由 |
|----|---------|
| post_check 成交闭环（旧 P0-06/13 → P1-005/006） | 逻辑已实现并有单测，但真实成交触发依赖 L2 `/data/trades` 轮询，sandbox 无外网无法端到端验证 → 推迟到 Shadow Trading |
| pending orders 同步（旧 P0-12 → P1-005/007） | 状态同步已有单测，对账闭环已在启动 + 后台 30s 循环实现 |
| 后台对账循环（旧 P0-14 → P1-007） | 已实现每 30s full_reconcile + 差异告警；无资金直接暴露，属健壮性增强 |

> ⚠️ 以上 3 项涉及资金/仓位安全的降级，是否同意归档，请伯森先生明示。

---

## 候选任务（Round 04 审查产出）

> 来源：`audits/round_04/audit_opencode_round_04.md`（OpenCode，commit `21fdcf8`）。
> **状态说明**：Round 04 独立共识审查已完成（2026-09-17）：`P0-RISK-001/002/003`、`P0-EXEC-004`、`P0-EXEC-005` 均 **CONFIRMED / P0**；G1/G2/G3 与 ENV-001 **已授权**，`Status` 置为 `TODO`。一次只做一个 Task Group。
> ⚠️ **总体状态 LIVE = NO-GO**：任何 P0 未闭环前禁止实盘。CONSENSUS 原件：`audits/round_04/CONSENSUS_ROUND_04.md`。
> 红线不变：不得改交易语义 / Risk Model 阈值 / Strategy / 资金控制逻辑；本批**仅做接线与 fail-closed**。

### G1 风控三闸接线（P0-RISK-001/002/003）

- **Status**: TODO（**已授权 2026-09-17**；CONSENSUS_ROUND_04 确认 P0-RISK-001/002/003 = CONFIRMED/P0）
- **Owner**: Work Buddy
- **Goal**: 让已实现但未接入生产路径的三项风控真正生效——① 日亏熔断（`post_check`→`report_pnl`）；② 结算前 2h 禁开 Buy；③ Gas 费保护。仅接线，**不改任何阈值/语义**。
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/grpc/service.rs`
  - `polymarket-rust/polymarket-rust/src/bin/server.rs`
  - `polymarket-rust/polymarket-rust/src/fast/executor.rs`（仅取 gas price 调用，若需要）
- **Forbidden**:
  - `strategy_engine/`
  - `polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`（**阈值与判定逻辑不改**；如确需新增 getter 须先申请）
- **Required Tests**:
  - `test_daily_loss_halts_after_threshold_integration`（结算/盈亏回填后达阈值 → `check_gates` 拒单）
  - `test_settlement_window_blocks_buy_when_registered`（注册结算时间后，2h 内 Buy 被拒、Sell 放行）
  - `test_unknown_settlement_fail_closed`（未注册结算时间 → 按约定拒绝并告警）
  - `test_gas_protection_rejects_high_gwei`（gas 超上限 → 拒单；gas 未知 → fail-closed）
- **Acceptance**: 三项在真实 `place_order` 调用链上可触发并拒绝；`get_risk_state` 的熔断标志与 `RiskEngine` 权威状态一致（002 的 fail-closed 策略需伯森先生先确认「未注册结算时间」的处置口径）。

### G2 实时成交链可用性（P0-EXEC-004）

- **Status**: 🔧 FIXED（2026-09-17；CONSENSUS_ROUND_04 确认 P0-EXEC-004 = CONFIRMED/P0）
- **Owner**: Work Buddy
- **Goal**: 启用 `MARKET_EVENTS_ENABLED=1` 后，`poll_trades → Trade → on_market_trade` 真正闭环——补 L2 认证头、从注册表填充监控 token、`tick` 不再吞错。
- **实现说明（09-17）**：
  - **L2 认证**：`MarketEventPoller` 新增 `.with_l2_auth(wallet, creds)`；`poll_trades`（`/data/trades`）与 `poll_open_orders`（`/data/orders`）按 `{ts}{METHOD}{path}` 签名并注入 `POLY_*` 头，非 2xx 显式返回 `Err`。
  - **监控 token 来源**：`MarketEventPoller::load_token_ids_from_registry(path)` 读取 MarketDiscovery 注册表 JSON 的 `yes_token_id`/`no_token_id`；server 侧支持 `MARKET_EVENTS_TOKEN_IDS`（CSV）覆盖、`MARKET_EVENTS_REGISTRY` 指定路径。
  - **不再吞错 + fail-closed**：`tick` 返回 `TickOutcome { tokens, poll_errors, events }`，trades/book/orders 任一失败计数并 `tracing::error!/warn!`；server 轮询循环在 `outcome.should_degrade()` 时调用 `trading_gate.reconcile_complete(false,false)` 降级。
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/market_events.rs`
  - `polymarket-rust/polymarket-rust/src/bin/server.rs`
- **Forbidden**: `strategy_engine/`、`risk_engine.rs`、交易语义
- **Required Tests**（实际落地名）:
  - `test_poll_trades_attaches_l2_headers`（wiremock 断言 POLY_ADDRESS/SIGNATURE/TIMESTAMP/API_KEY/PASSPHRASE 齐备）
  - `test_tick_reports_errors_and_degrades_on_poll_failure`（仅 trades 500 → `poll_errors==1` + `should_degrade()`）
  - `test_tick_uses_configured_token_ids`（tick 按传入 token 列表请求 `/data/trades`）
  - `test_load_token_ids_from_registry`（注册表解析 + 缺失文件 Err）
- **验证**：全量 Rust `326 + 4 + 18 + 1` 全绿；**鉴别力验证**：分别关闭 L2 头挂载 / 错误计数 / token 遍历后，对应 3 测试必 FAILED，还原后转绿。
- **Acceptance**: 认证头齐备、错误可见且 fail-closed、token 列表来自配置/注册表。**待独立审查员判 VERIFIED**。

### G3 Maker 成交方向归一化（P0-EXEC-005）

- **Status**: 🔧 FIXED（2026-09-17；P0-EXEC-005 = CONFIRMED/P0。外部事实已确认：Polymarket Trade schema 同时含 `side`/`trader_side`/`maker_address`/`maker_orders`/`taker_order_id`，**不得仅凭顶层 `side` 归属本方成交方向**）
- **Owner**: Work Buddy
- **Goal**: `trader_side == "MAKER"` 时把成交 `side` 归一化到本方视角（BUY↔SELL），使 `RiskEngine`/`risk_state`/`FillLedger` 方向正确。
- **实现说明（09-17）**：归一化落在**事件产生处**而非入账处——实时路径 `market_events::own_side()`（`poll_trades` 构造 `TradeEvent.side` 时按 `trader_side` 取反）；恢复路径 `state_sync::own_recovery_side()`（按 `taker_order_id`/`maker_order_id` 判定本方角色后取反）。`on_market_trade` 仍是唯一入账点，`TradeEvent.side` 语义统一为「本方视角」。
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/market_events.rs`（新增 `own_side` + `poll_trades` 归一化）
  - `polymarket-rust/polymarket-rust/src/fast/state_sync.rs`（新增 `own_recovery_side` + `build_recovery_plan` 归一化）
  - （`service.rs` 无需改动——入账点未变）
- **Forbidden**: `strategy_engine/`、`risk_engine.rs`、交易语义（不改 token 路由决策 DEC-0001/0002）
- **Required Tests**（实际落地名）:
  - `test_own_side_maker_inverts` / `test_own_side_taker_unchanged`（纯函数）
  - `test_poll_trades_maker_side_inverted`（wiremock：MAKER + side=SELL → 事件 side=BUY，order_id 归属正确）
  - `test_build_recovery_plan_maker_side_inverted` / `test_build_recovery_plan_taker_side_unchanged`
- **验证**：全量 Rust `322 + 4 + 18 + 1` 全绿；**鉴别力验证**：关闭两处归一化后 3 个 maker 测试必 FAILED、taker 测试仍绿。
- **Acceptance**: Maker/Taker 两种角色下仓位方向与 `risk_state` 一致；回归全绿。**待独立审查员判 VERIFIED**。

### ENV-001 Python 测试门禁修复（流程/测试基础设施）

- **Status**: 🔧 FIXED（2026-09-17）
- **Owner**: Work Buddy
- **Goal**: 让 `AGENTS.md` 承诺的 Python 验证门禁**真实可用且可复现**：修复 pytest 依赖缺失、async 用例挂起、脚本式测试误收集、fixture 缺失四项，并让文档与命令一致。不改任何业务代码逻辑。
- **背景证据**: 见 `audits/round_04/audit_opencode_round_04.md` §六 ENV-001——venv 未装 pytest 且 `requirements.txt` 未声明；装上后 `test_grpc.py` 在 pytest 下挂起（自带 `main()` 脚本跑法全绿）；`test_mock.py::test_formatter` 缺 `noaa_fc/caiyun_fc` fixture。
- **实现说明（09-17）**：
  - 新增 `pytest.ini`（`asyncio_mode=auto` + 忽略脚本式 return 警告）、`conftest.py`（`noaa_fc`/`caiyun_fc` fixture，复用 `test_mock` 内联 mock）、`requirements-dev.txt`（pytest / pytest-asyncio / anyio）。
  - `test_grpc.py`：**修正项**——9 个 async 用例在 pytest 的 per-test event loop 下实测必挂起（90s 未返回），根因在 `strategy_engine/grpc_bridge` 的 gRPC async 生命周期，而该目录不在本任务 Allowed Files 内，**无法修复**。故对 9 个 async 用例加 `__test__ = False` 标记，从 pytest 收集排除；其 4 个同步转换器用例仍由 pytest 覆盖，全部用例由 `python test_grpc.py` 脚本 runner 验证（全绿）。
  - `.gitignore` 增加 `.pytest_cache/`；`AGENTS.md` 门禁命令改为 `python -m pytest -q`（基线 94 passed）+ `python test_grpc.py`。
- **Allowed Files**:
  - `pytest.ini`（新建）
  - `conftest.py`（新建）
  - `requirements-dev.txt`（新建）
  - `test_grpc.py`、`test_mock.py`（仅加 pytest 适配标记 / fixture，不改断言与业务）
  - `AGENTS.md`（仅修正「Python 验证门禁」命令描述）
  - `.gitignore`（如新增缓存/产物需忽略）
- **Forbidden**: `strategy_engine/`、`weather_data_ingestion/`、`market_discovery/`、`polymarket-rust/`（业务逻辑一律不改）
- **Required Tests**（实际结果）:
  - `python -m pytest -q` → **94 passed / 0 failed / 0 error，2.7s 完成（不再挂起）**
  - `python test_grpc.py`（脚本 runner，含 9 个 async 用例）→ 全部 PASS
- **Acceptance**: 已达成；`AGENTS.md` 门禁命令与实际一致。
- **遗留（未在本次授权范围）**: `test_grpc.py` async 用例在 pytest 下挂起的**根因修复**需改动 `strategy_engine/grpc_bridge`（client/mock_server 的 gRPC async 生命周期），建议单列后续任务。

---

### 后续候选（P1/P2，待 G1~G3 后再排期）

> 明细见 `audits/round_04/audit_opencode_round_04.md` 第四、五节。建议分组：
> G4 预留生命周期（P1-RISK-006/007/008、P1-EXEC-009）；G5 对账数据完整性（P1-CONSIST-011、P1-DATA-012）；
> G6 认证防御（P1-SEC-013）；G7 OrderStore 记账一致性（P1-CONSIST-010）；
> G8 Mempool/Nonce 潜伏缺陷（P1-EXEC-014 及 P2 潜伏项，**接入生产前必须修**）。
> 另有流程项 **ENV-001**：Python `pytest` 门禁实际不可用，建议补 `pytest.ini`/`conftest.py` 与 dev 依赖清单。
