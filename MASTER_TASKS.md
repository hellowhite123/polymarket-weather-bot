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

### 阶段四（09-23 伯森先生授权开工）：Round 05 代码整改（9 项）

> 授权来源：伯森先生 09-23 逐条整改指令（P0×1 + P1×4 + P2×4）。
> 红线不变：**不得改交易语义 / 改 RiskConfig 阈值含义 / 改聚合器或 gRPC 契约 / 大规模重构 / 触碰 cold_wallet 与签名算法标准实现**。
> 通用 Allowed：仅改下列各条指定文件 + 对应测试。通用 Forbidden：上述红线全部适用。
> 通用验收锚点：`cargo clippy --all-targets` 0 error；`cargo test` 全绿；`python -m pytest -q` 全绿。

#### R5-001（P0）集中度检查 TOCTOU — 分母漏计 pending 预留

- **Status**: ✅ **CONFIRMED**（09-18 独立审查，见 `docs/CONSENSUS_ROUND05_R5-001_R5-009.md`）；实现（09-23 分母统一）+ Sell 分支修正与测试重做（09-18）均通过独立审查
- **Owner**: OpenCode
- **Allowed Files**: `polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`
- **Forbidden**: `strategy_engine/`、`RiskConfig` 阈值含义、`ConcentrationChecker` 判定语义
- **改法**: `PositionLimits` 新增 `committed_total()`（`notional + reserved` 全市场求和）；`check_concentration` 分母改用 committed 口径，统一「要么全含 reserved、要么都事后加」，禁止混用（`check_and_reserve` 已预留 → 不重复加 notional；`pre_check` 未预留 → 补本单 notional）。
- **Required Tests**: 新增「预留下单 A 后下单 B，集中度校验必须计入 A 的 pending 额度」回归测试；既有 `test_check_and_reserve_concentration_failure_rolls_back` 保持全绿。
- **Acceptance**: clippy 无新增告警；新增测试通过；cargo test 全绿。
- **Sell 分支修正（09-18，伯森先生指令「P0 必须先修」）**: 上项引入 `check_concentration(..., already_reserved)` 后，`check_and_reserve` 对 **Sell 也硬编码 `true`**（Buy 才预留）→ `future_total` 漏算 Sell 本单 notional，与 `pre_check(.., false)` 口径不一致，集中度门禁/比率被低估。改法：`already_reserved = matches!(side, Side::Buy)`。🔧 FIXED（待独立判 VERIFIED）。
- **测试重做（09-18，独立审查要求「测试必须只因 concentration 失败」）**:
  - `test_sell_concentration_rejected_without_position_limit_breach`（Test A）：构造 `max_position=5000 / committed=9900 / Sell 200` → **Position Limit PASS + Global PASS**，仅 |committed| 加本单跨过 `2×max` 门禁后 `new_market/future_total = 3800/10100 ≈ 37.6% > 30%` → 拒；并断言错误信息含 `集中度限制`、**不含** `仓位硬限制`/`全局暴露限制`（旧测试因 over-limit 构造无法证明失败原因）。
  - `test_sell_concentration_failure_does_not_release_existing_reserve`（Test B）：已有 Buy 预留 500，Sell 集中度失败后 `committed_total` **不变**（回滚仅 Buy 分支，不误释放）。
  - Buy rollback 保持（Test C：`test_check_and_reserve_concentration_failure_rolls_back`）。
  - **Test D（mutation）**：`already_reserved = matches!(side, Buy)` → Test A/B PASS；临时改 `true` → **A/B 均 FAILED**（已验证）；恢复后全绿。
- **完成标准核对**：[1]逻辑正确 ✅ [2]Sell 仅因 concentration 失败 ✅ [3]mutation 捕获旧 Bug ✅ [4]Buy rollback ✅ [5]Sell 不误释放 ✅ [6]Rust 全量 `338 + 4 + 18 + 1` 全绿 ✅。
  - [7] `cargo fmt --check`：**R5-001 授权文件 `risk_engine.rs` rustfmt 干净**；但**全库** `cargo fmt --check` 失败于**预存**文件（`server.rs`/`fill_records.rs`/`market_events.rs`/`mempool.rs`/`nonce_manager.rs`/`order_state.rs`/`state_sync.rs`/`service.rs`/`tests/*`）——非 R5-001 引入，按「仅改授权范围」**不擅自全库 rustfmt**。
  - [8] `cargo clippy -D warnings`：失败于 **20 个预存残留**（8× `non_snake_case` serde JSON 字段名 `apiKeys`/`orderType`/`orderID`/`errorMsg`/`makingAmount`/`takingAmount` 等、`very complex type`、`Err`-variant 过大）——**R5-006 已记录为 JSON 契约不可改名**；修它属越权/破坏契约。
  - → [7]/[8] **受预存基线阻断**，需单独授权（全库 fmt 或显式 `#[allow]` 白名单）方能满足；**本任务不擅自处理**。

#### R5-002（P1）建议仓位与资本脱节 — MultiSource `_calculate_quantity`

- **Status**: 🔧 **FIXED（09-18，待独立审查）** — V3 授权实施完成，commit `a2dde81`；报告 `docs/R5-002_IMPLEMENTATION_REPORT.md`
  - 公式：`edge=1−exp(−|market_dev|/σ)`（σ=`DeviationTracker.stats()` **残差σ**，非 z）；`advisory_notional=base_capital×kelly_fraction×edge`；**USDC cap** 后 `/cost` 得 shares；无效 σ/cost/参数→0
  - 资本：`AggregatorParams.from_env()`（`AGG_*` 唯一读取点）→ 装配注入 `MultiSourceStrategyParams`（策略内无 `os.getenv`）
  - 测试：T1–T12；`pytest` **123 passed**；mutation 捕获（线性 edge → 5 测试 FAILED）
  - **未修改** `risk_gate.py`/`risk_engine.rs`/`PositionSizer`/`Aggregator` final sizing/proto/Rust
  - **E4**（Python/Rust `max_position_per_market` 双权威 + Python risk_gate 单位非 USDC）按 V3 §1 **剥离**，建议另立 P1 Finding
- **Owner**: OpenCode
- **Allowed Files**: `strategy_engine/strategies/multi_source.py`、`strategy_engine/config.py`（仅新增 MultiSource 参数字段）、`test_multi_source.py`
- **Forbidden**: 改 `max_position_per_market` 硬顶语义；改其他策略；改聚合器 Kelly 实现
- **改法**: `MultiSourceStrategyParams` 增加 `kelly_fraction`（默认 0.25）、`base_capital`（默认 1000.0，与 `.env` AGG_* 对齐）；`_calculate_quantity` 改为 `kelly_fraction × base_capital × 价差优势系数`，`cap = min(结果, max_position_per_market)`。
- **Required Tests**: `test_multi_source.py` 断言金额受资本约束、同 dev_sigma 下不随 market_dev 线性飙升。
- **Acceptance**: pytest 全绿。

#### R5-003（P1）clean_forecast 原地修改输入

- **Status**: 🔧 FIXED（09-23：方案 A `copy.deepcopy` 兑现不可变契约；新增 `test_cleaning.py` 2 用例）
- **Owner**: OpenCode
- **Allowed Files**: `weather_data_ingestion/cleaning.py`、`test_cleaning.py`（或新增用例）
- **Forbidden**: 改清洗数值语义
- **改法**: 方案 A（推荐）——`clean_forecast` 先 `copy.deepcopy(forecast)` 再清洗并返回新对象，兑现「不修改原始对象」契约；核对 `pipeline.py`/回测/`strategy_engine` 调用点语义一致。
- **Required Tests**: 断言 `clean_forecast(f) is not f` 且原对象字段不变。
- **Acceptance**: pytest 全绿。

#### R5-004（P1）AdaptiveWeighter 最小权重语义

- **Status**: 🔧 FIXED（09-23：归一化后再抬升 + 超量按比例扣回；+2 测试含不可行退化）
- **Owner**: OpenCode
- **Allowed Files**: `strategy_engine/ensemble_fusion.py`、`test_ensemble_fusion.py`
- **Forbidden**: 改 Brier/衰减公式
- **改法**: 先常规归一化得 `w_i`，再 `max(w_i, min_weight)`，二次归一化到 1；若多下限互斥致总和 > 1，按比例缩减超下限源的超量保证和 = 1。
- **Required Tests**: 3 源、`min_weight=0.1`，断言任一源权重 ≥ 0.1 且总和 = 1；覆盖边界。
- **Acceptance**: pytest 全绿。

#### R5-005（P1）私钥/密码未零化

- **Status**: 🔧 FIXED（09-23：`zeroize = "1"`；`decrypt` 返回 `Zeroizing<String>`，`encrypt`/`decrypt` 的 pk_bytes/derived_key/plaintext 均 `Zeroizing`；wallet 13 测试全绿）
- **Owner**: OpenCode
- **Allowed Files**: `polymarket-rust/polymarket-rust/Cargo.toml`（加 `zeroize = "1"`）、`src/wallet/keystore.rs`、`src/wallet/manager.rs`、`src/fast/hot_wallet.rs`（仅零化缓冲，不改签名算法）
- **Forbidden**: 改 PBKDF2 迭代/AES-256-GCM 算法、改 cold_wallet、改签名标准实现
- **改法**: `decrypt` 返回 `Zeroizing<Vec<u8>>`（或签名路径内以 `Zeroizing<Vec<u8>>` 承载私钥字节）；`encrypt` 的 `pk_bytes`/`derived_key` 用 `Zeroizing`；密码参数可保持 `&str`。
- **Required Tests**: cargo test 全绿；代码审查确认签名流程私钥不在非 Zeroizing 缓冲长期存活。
- **Acceptance**: cargo test 全绿。

#### R5-006（P2）clippy 门禁修复

- **Status**: 🔧 FIXED（09-23：`cargo clippy --all-targets` **0 error**；警告 82→23。`approx_constant` 用 PI 断言消除；`field_reassign_with_default` 全改结构体初始化；2 处 `await_holding_lock`（uma_monitor/daemon）改为 await 前释放锁；清理 unused/needless_borrow/casts 等。**残留 23 警告**：8 处 serde API 字段名 `non_snake_case`（JSON 契约，不可改名）、never-read/dead-code、complex type、too many args、is_empty 建议——均非资金类，记录在案）
- **Owner**: OpenCode
- **Allowed Files**: `polymarket-rust/polymarket-rust/src/**`（仅 clippy 整改）
- **Forbidden**: 借 clippy 之机改业务语义/阈值
- **改法**: `client.rs` 测试字面量改用 `std::f64::consts::PI` 消除 2 个 `approx_constant` error；`field_reassign_with_default` 改 `Struct { field, ..Default::default() }`；`uma_monitor.rs` 2 处 `await_holding_lock` 在 await 前 drop guard；清理 unused。
- **Required Tests**: `cargo clippy --all-targets` 0 error；`cargo test` 全绿。
- **Acceptance**: clippy 0 error；残留警告记录。
- **撤回（09-18，伯森先生指令）**: `daemon.rs` 启动预检 `rpc.check_health_all().await;` **撤回到 `let _ = rpc.check_health_all();`**（不 await），除非另行明确授权。该处不属 clippy 必修项；撤回后 `cargo test` 仍 `337+4+18+1` 全绿。若后续 clippy 因 `let_underscore_future` 报错，须先取得授权再处理。

#### R5-007（P2）权益曲线年化因子硬编码

- **Status**: 🔧 FIXED（09-23：`_resolve_periods_per_year` 由 `total_hours / len(equity_curve)` 反推；+1 测试验证 Sharpe 按采样粒度 √缩放）
- **Owner**: OpenCode
- **Allowed Files**: `strategy_engine/backtest/metrics.py`、`test_backtest.py`
- **Forbidden**: 改收益率序列定义
- **改法**: `calculate()` 依据 `total_hours / len(equity_curve)` 反推实际采样间隔换算 `periods_per_year`，或加参数由调用方传入。
- **Required Tests**: 若断言 Sharpe 需随新口径核对期望值。
- **复审纠正（09-18，伯森先生）**: 确认 `total_hours` 生产定义 = **回测窗口首尾跨度**（`engine.py`：`events[0].deadline-48h → events[-1].deadline+1h`），**非** equity 曲线首尾；且 n 个点只有 **n-1** 个间隔。旧口径 `total_hours / len(equity_curve)` 分母错误（混用窗口/曲线 + 把 n 当间隔数），会低估间隔、放大年化。**纠正**：`_resolve_periods_per_year` 改为**优先由真实 `EquityPoint.timestamp` 反推** `interval=(last-first)/(n-1)`，时间戳不可用时回退 `total_hours/(n-1)`。测试改为 timestamp-based：`test_sharpe_annualization_follows_sampling`（1h vs 6h → √6）、`test_periods_per_year_uses_real_timestamps_not_total_hours`、`test_periods_per_year_fallback_uses_n_minus_1`；鉴别力验证：改回旧口径 3 测试必 FAILED。全量 pytest `111 passed`。
- **Acceptance**: pytest 全绿。

#### R5-008（P2）持仓时间/分组统计用 token_id 表匹配

- **Status**: 🔧 FIXED（09-23：改为按 token 维护 OPEN 队列 FIFO 配对，一一对应；+1 测试同 token 两次开平）
- **Owner**: OpenCode
- **Allowed Files**: `strategy_engine/backtest/metrics.py`、`strategy_engine/backtest/models.py`（如需加批次字段）、`test_backtest.py`
- **Forbidden**: 改交易记录语义
- **改法**: 为 `TradeRecord` 增加唯一 `batch_id`/`order_id`（OPEN 与对应 SETTLE 共享）按批次配对；或按时间序 FIFO 匹配同 token 的 OPEN↔CLOSE 保证一一对应。
- **Required Tests**: 同 token 多次开平场景下持仓时间/胜率统计正确。
- **Acceptance**: pytest 全绿。

#### R5-009（P2）假健康检查

- **Status**: ⛔ **BLOCKED — ENVIRONMENT**（09-18 独立审查：设计通过，等待真实 Docker Runtime Evidence；本环境无 Docker）。代码侧已 FIXED（09-23 探测 + 09-18 方案1 entrypoint 保持 Up）；**不得再改代码**（除非新 Finding）
- **Owner**: OpenCode
- **Allowed Files**: `docker-compose.yml`（app 段 healthcheck）
- **Forbidden**: 改服务依赖/网络语义
- **改法**: 改为真实探测（HTTP/gRPC 就绪端点，或进程存活 + 心跳输出），参照 polymorph 段 `nc -z localhost 50051`。
- **Required Tests**: `docker compose config` 合法；app 失活时 healthcheck 转 unhealthy。
- **Acceptance**: compose 配置合法。
- **补充复审（09-18，伯森先生）**: 实机验收要求 `docker compose config/up` + 停 `market_discovery` 后 3 次 healthcheck 转 `unhealthy`。**本环境无 Docker**（无 `docker.exe` / Docker Desktop / WSL 发行版）→ **真实 Docker 验收 BLOCKED**。静态复核：healthcheck 谓词**正确**（`p` 为每个进程的 argv 列表，`b'-m' in p and b'market_discovery' in p` 成立；不误匹配自身）；`polymarket-rust/Dockerfile` 已装 `netcat-openbsd`。
- **行为修正（方案 1）**: 原 daemon 监督在子进程退出时 `exit 1` + `restart: unless-stopped` → 容器直接重启，healthcheck 来不及转 `unhealthy`。`docker-entrypoint.sh` daemon 分支改为**子进程退出后保持容器 Up**（仅记录一次告警），使 healthcheck 能累积 3 次转 `unhealthy`。`bash -n` 语法通过。**真实 Docker 验收仍需在有 Docker 的主机执行**（runbook 见对话/交付说明）。
- **运行手册（09-18）**: 新增 `docs/DEPLOY_HEALTHCHECK_RUNBOOK.md`——明确「Up ≠ Healthy」、healthcheck 检查真实 `market_discovery` 进程且不误匹配其他 python 模块、`retries=3` 由 Docker 负责、**unhealthy 不自动重启**（单机 Compose 由人工 `docker compose restart app` 处置）、真实 Docker 5 步验收、信号处理静态核验。

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

- **Status**: 🚧 IN_PROGRESS（已授权 2026-09-17；CONSENSUS_ROUND_04 确认 P0-RISK-001/002/003 = CONFIRMED/P0）
  - G1-003 Gas 保护：🔧 **FIXED（09-17）**
  - G1-001 日亏熔断：⬜ TODO（依赖结算/PnL 链）— 方案 `docs/design/settlement_pnl_risk_wiring.md` **Draft v7，待审查**（统一 D1 SQLite 单事务模型：S4 COMMIT=effect 已生效唯一 durable boundary；Case A–E 区分 D1/D2、D1 无 Case C；durable conflict algorithm；§14.12 原子性测试。**P0-2 UMA POC BLOCKED → 实施阻断，需 HA-007**；HA-006 倾向 D1 SQLite 待冻结）
  - G1-002 结算窗口：⬜ TODO（需先出 `UpdateSettlementTimes` 接口方案待批）— 方案 `docs/design/update_settlement_times_rpc.md` **Draft v3，待审查**（二轮整改：Unknown/STALE gate 落 `place_order::settlement_gate`、双 freshness、replace_all 原子语义、增长保护、Human Acceptance 冻结）
  - 设计审查记录：`docs/design/CONSENSUS_DESIGN_ROUND_01.md`；审查清单：`docs/design/REVIEW_CHECKLIST_DESIGN.md`
- **Owner**: Work Buddy
- **Goal**: 让已实现但未接入生产路径的三项风控真正生效——① 日亏熔断（`post_check`→`report_pnl`）；② 结算前 2h 禁开 Buy；③ Gas 费保护。仅接线，**不改任何阈值/语义**。
- **G1-003 实现说明（09-17）**：`place_order` 在 `check_and_reserve` 前调用 `gas_gwei_for_risk()`——经 `FastExecutor::rpc_pool().rpc_call("eth_gasPrice")` 取价并解析为 gwei（`parse_gas_price_hex_to_gwei`，向下取整）。
  - 未挂载执行器/未配 RpcPool（mock/paper）→ `Ok(0)` 不阻塞（此类订单随后因「FastExecutor 未初始化」被拒）。
  - 已挂载但 RPC 调用/解析失败 → **FAIL-CLOSED**，`mark_rejected` + 返回 `Rejected`；不再把 gas 硬编码为 0 绕过上限检查。
  - 改动仅 `service.rs`（未改 `risk_engine.rs` 阈值/逻辑）。
- **G1-003 测试**（实际落地名，全部在 `service.rs` tests）:
  - `test_parse_gas_price_hex_to_gwei` / `test_parse_gas_price_invalid_returns_err`
  - `test_gas_protection_rejects_high_gwei`（501 gwei 拒、500 放行）
  - `test_place_order_fails_closed_when_gas_fetch_fails`（RPC 不可达 → 拒单，错误含「Gas 费获取失败」）
  - **鉴别力验证**：将失败分支改回放行、解析器改恒 0 后，后 2 个测试必 FAILED。
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/grpc/service.rs`
  - `polymarket-rust/polymarket-rust/src/bin/server.rs`
  - `polymarket-rust/polymarket-rust/src/fast/executor.rs`（仅取 gas price 调用，若需要）
- **Forbidden**:
  - `strategy_engine/`
  - `polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`（**阈值与判定逻辑不改**；如确需新增 getter 须先申请）
- **Required Tests**:
  - `test_daily_loss_halts_after_threshold_integration`（结算/盈亏回填后达阈值 → `check_gates` 拒单）— 待 G1-001
  - `test_settlement_window_blocks_buy_when_registered`（注册结算时间后，2h 内 Buy 被拒、Sell 放行）— 待 G1-002
  - `test_unknown_settlement_fail_closed`（未注册结算时间 → 按约定拒绝并告警）— 待 G1-002
  - ~~`test_gas_protection_rejects_high_gwei`~~ ✅ 已完成
- **Acceptance**: 三项在真实 `place_order` 调用链上可触发并拒绝；`get_risk_state` 的熔断标志与 `RiskEngine` 权威状态一致（002 的 fail-closed 策略需伯森先生先确认「未注册结算时间」的处置口径）。**当前验证**：Rust 330 + 4 + 18 + 1 全绿。

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

### G4 预留生命周期（P1-RISK-006/007；008/009 需设计）

- **Status**: 🚧 IN_PROGRESS（2026-09-17 伯森先生授权）
  - P1-RISK-006 取消订单释放预留 + OrderStore 原子取消：🔧 FIXED（09-17）
  - P1-RISK-007 后台对账后重建 open 预留：🔧 FIXED（09-17）
  - P1-RISK-008 成交价优于限价时的预留残余：⬜ **需设计/显式授权**（须改 `risk_engine.rs` 预留记账 = Risk Model 红线）— 方案 `docs/design/reserve_release_accounting.md`（Draft v1，待审）
  - P1-EXEC-009 未知结果订单被对账输入集排除：⬜ **需设计/显式授权**（对账语义变更）— 方案 `docs/design/indoubt_order_reconciliation.md`（Draft v1，待审）
- **Owner**: Work Buddy
- **Goal**: 消除预留（reserve）泄漏：① 取消订单时释放该单剩余预留并标记 OrderStore；② 后台对账触发仓位重建后重建远端 open 订单预留（避免暴露漏计）。
- **实现说明（09-17）**：
  - `order_state.rs`：`OrderState` 新增 `limit_price`；`OrderStore` 新增 `set_limit_price` / 原子 `mark_cancelled`（终态保护）/ `mark_cancelled_by_exchange_order_id` / `active_orders`。
  - `service.rs`：下单 claim 后记录 `limit_price`；新增 `settle_cancel`（按 `remaining_size × limit_price` 释放预留 + 原子取消）；`cancel_order`/`cancel_all` 成功后调用（cancel_all 同时修正 `cancelled_count`）；`apply_recovery::MarkOrderNonFilled` 改用原子取消；`rebuild_open_order_reserves` 增加 `_filtered` 变体（只重建指定 token，规避 `rebuild_reserve` 累加语义的双计）。
  - `server.rs`：后台 30s 对账中，若有 `RebuildPosition` 动作 → `fetch_open_orders` + 只对这些 token `rebuild_open_order_reserves_filtered`。
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/grpc/service.rs`
  - `polymarket-rust/polymarket-rust/src/fast/order_state.rs`
  - `polymarket-rust/polymarket-rust/src/bin/server.rs`
- **Forbidden**: `strategy_engine/`、`fast/risk_engine.rs`（**阈值/记账逻辑不改**）、`fast/fill_records.rs`、交易语义
- **Required Tests**（实际落地名）:
  - `test_cancel_order_releases_reserve`、`test_cancel_all_releases_reserves`
  - `test_mark_cancelled_atomic_terminal_protection`、`test_mark_cancelled_by_exchange_order_id`、`test_active_orders_excludes_terminal`
- **验证**: 全量 Rust `335 + 4 + 18 + 1` 全绿；**鉴别力验证**——禁用预留释放后取消类测试必 FAILED；去掉终态保护后 `test_mark_cancelled_atomic_terminal_protection` 必 FAILED。
- **Acceptance**: 取消成功后该单预留被释放、OrderStore 转 Cancelled；后台恢复后只对仓位重建 token 重建预留。**待独立审查员判 VERIFIED**。
- **未纳入本批（原因）**: P1-RISK-008 需要「按 order_id 的预留台账」才能精确释放残余，属 Risk Model 记账变更；P1-EXEC-009 需要重新定义 `Received` 订单的对账语义。两者须先出设计并经授权。

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
- **遗留**: 原「`test_grpc.py` async 挂起根因」已由 **TST-001** 闭环（session 级 event loop，无需改 `grpc_bridge`）。

---

### TST-001 test_grpc.py async 用例 pytest 挂起根因修复

- **Status**: 🔧 FIXED（2026-09-17）
- **Owner**: Work Buddy
- **Goal**: 修复 `test_grpc.py` 的 9 个 async 用例在 pytest 下挂起的**根因**，使其可被 pytest 正常收集执行，随后移除 ENV-001 中的 `__test__ = False` 规避标记。
- **根因（已定位）**: pytest-asyncio 默认**每测试一个 event loop**；`MockExecutionServer`（`grpc.aio.server`）/ `GrpcExecutionClient`（`grpc.aio` channel）跨多个 event loop 反复启停时，异步资源在 loop 关闭处挂起。用 `pytest-timeout` 定位：单独跑任一 async 用例均通过，但整套顺序跑到最后一个用例时超时（`event_list = self._selector.select(timeout)` 处阻塞）——即**累积的 per-test loop 资源**所致。脚本 runner（`asyncio.run(main)` 单 loop）天然无此问题。
- **修复（最小、无业务逻辑改动）**: `pytest.ini` 设 `asyncio_default_test_loop_scope = session`（+ `asyncio_default_fixture_loop_scope = session`），让全部 async 用例共享一个 session 级 loop（等价脚本 runner 的单 loop）；并加 `timeout = 120` 安全网。
- **Allowed Files**:
  - `pytest.ini`、`requirements-dev.txt`（新增 `pytest-timeout`）
  - `test_grpc.py`（移除 `__test__ = False` 标记）
  - `strategy_engine/grpc_bridge/client.py`、`mock_server.py`（**最终无需改动**）
- **Forbidden**: 业务逻辑（converter 转换语义、proto 契约、下单业务规则）一律不改
- **Required Tests**（实际结果）:
  - `python -m pytest -q test_grpc.py` → **13 passed / 2.0s（0 hang / 0 error）**
  - `python -m pytest -q`（全量）→ **103 passed / 7.3s**（原 94，test_grpc 的 9 个 async 用例已纳入）
  - `python test_grpc.py` 脚本 runner → 全绿
- **Acceptance**: 已达成；`__test__ = False` 已移除；根因与修复记录于本条目 / commit message / `pytest.ini` 注释；无业务行为变化。**待独立审查员判 VERIFIED**。

---

### 后续候选（P1/P2，待 G1~G3 后再排期）

> 明细见 `audits/round_04/audit_opencode_round_04.md` 第四、五节。建议分组：
> G4 预留生命周期（P1-RISK-006/007/008、P1-EXEC-009）；G5 对账数据完整性（P1-CONSIST-011、P1-DATA-012）；
> G6 认证防御（P1-SEC-013）；G7 OrderStore 记账一致性（P1-CONSIST-010）；
> G8 Mempool/Nonce 潜伏缺陷（P1-EXEC-014 及 P2 潜伏项，**接入生产前必须修**）。
> 另有流程项 **ENV-001**：Python `pytest` 门禁实际不可用，建议补 `pytest.ini`/`conftest.py` 与 dev 依赖清单。
