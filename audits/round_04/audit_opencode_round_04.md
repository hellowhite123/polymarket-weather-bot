# audit_opencode_round_04.md

- **审查方**：OpenCode（`opencode-go/deepseek-v4.1-flash`）
- **日期**：2026-09-17
- **审计输入（代码快照）**：分支 `p0-blockers-2026-09-13`，commit `21fdcf8`（P1-035 测试补齐后）
- **审查类型**：只读代码审查（范围 A：交易关键路径），未修改任何源码
- **依据**：`docs/CODE_REVIEW_STANDARD.md`（五维 + §4 Finding 格式 + 六态状态机）、`AGENTS.md`
- **角色声明**：OpenCode 在当前项目为**工程实施者**。本报告所有 Finding 均为 `TODO`/`IN_PROGRESS` 候选，**不构成 VERIFIED**；VERIFIED 须由非实施者（独立审查 AI / Human）基于三重证据判定。

## 方法与取证口径

1. 四个只读审查代理并行分片审阅：`service.rs`/`order_state.rs`、`risk_engine.rs`/`fill_records.rs`、`state_sync.rs`/`market_events.rs`、`nonce_manager.rs`/`mempool.rs`/`executor.rs`。
2. 主审查方对**所有 P0 与高置信 P1** 逐条用 grep + 源码精读独立复核（不盲信代理结论）。
3. 证据标签：
   - `[主审已复核]` = 本次审查中由主审查方直接读取源码确认；
   - `[代理报告]` = 来自分片代理、主审查方未逐行复核（置信度以降级处理）。

## 一、总览

| 级别 | 数量 | 说明 |
|------|------|------|
| P0 | 5 | 风控三闸未接线 + 实时成交链不生效 + Maker 方向归一化缺失 |
| P1 | 9 | 预留泄漏/对账不完整/记账极性/分页/认证回退等 |
| P2 | 12 | 可观测性、语义漂移、潜伏缺陷 |
| 环境/流程 | 2 | Python pytest 门禁实际不可用；`.workbuddy` pytest 依赖缺失 |

> 判据引用 `CODE_REVIEW_STANDARD.md` §2：**「模块存在但未接入生产执行路径（存在 ≠ 生效）」直接命中 P0**。

## 二、Finding 汇总表

| ID | 级别 | 类别 | 标题 | 关键位置 | 置信度 | 取证 |
|----|------|------|------|----------|--------|------|
| P0-RISK-001 | P0 | Risk | 日亏熔断从未触发（`post_check` 生产零调用） | `risk_engine.rs:1049,1063`；`service.rs` | HIGH | 主审已复核 |
| P0-RISK-002 | P0 | Risk | 结算窗口（2h 禁开 Buy）未接线 | `risk_engine.rs:443-483,1115` | HIGH | 主审已复核 |
| P0-RISK-003 | P0 | Risk | Gas 费保护被硬编码 `0` 绕过 | `service.rs:718`；`risk_engine.rs:752` | HIGH | 主审已复核 |
| P0-EXEC-004 | P0 | Execution | 实时成交链不生效：无 L2 认证 + token 列表恒空 + 错误吞没 | `market_events.rs:268-277,438`；`server.rs:303-335` | HIGH | 主审已复核 |
| P0-EXEC-005 | P0 | Correctness | Maker 成交未按 `trader_side` 归一化，可能反向记账 | `service.rs:347-377`；`market_events.rs:345-358` | MEDIUM | 主审已复核 |
| P1-RISK-006 | P1 | Risk | 取消订单不释放预留 → 容量泄漏 | `service.rs:915-960`；`risk_engine.rs:311` | HIGH | 主审已复核 |
| P1-RISK-007 | P1 | Risk | 后台对账不重建 open 订单预留 → 暴露漏计（fail-open） | `server.rs:247-259`；`risk_engine.rs:372` | HIGH | 主审已复核 |
| P1-RISK-008 | P1 | Risk | 成交价优于限价时预留残余不释放 | `risk_engine.rs:332-338` | HIGH | 代理报告 |
| P1-EXEC-009 | P1 | Execution | 未知结果订单停在 `Received`，被对账输入集排除 | `service.rs:841-857`；`order_state.rs:362-366` | HIGH | 代理报告 |
| P1-CONSIST-010 | P1 | Correctness | 下单响应绝对值 + 成交事件增量双路写 OrderStore | `service.rs:793-812` vs `398-399` | HIGH | 代理报告 |
| P1-CONSIST-011 | P1 | Data | StateSync `/data/*` 不翻页，第一页外数据被当权威 | `state_sync.rs:116-123,437-527` | HIGH | 代理报告 |
| P1-DATA-012 | P1 | Data | 恢复路径 `parse_json_f64` 静默转 `0.0`（与实时路径策略不一致） | `state_sync.rs:846-853` | HIGH | 代理报告 |
| P1-SEC-013 | P1 | Security | HMAC secret 解码失败静默回退原始字节（掩盖 401 根因） | `hmac_sig.rs:35-37` | HIGH | 代理报告 |
| P1-EXEC-014 | P1 | Execution | Mempool 全局容量按历史 `orders.len()` 计数（潜伏，见详述） | `mempool.rs:519,549,626-632` | HIGH | 主审已复核 |
| P2（12 项） | P2 | — | 见第四节 | — | — | 混合 |

## 三、P0 详述（§4 Finding 格式）

### P0-RISK-001 日亏熔断从未触发

- **File**: `polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`
- **Line**: 1049-1063（`post_check` → `report_pnl`）；`src/grpc/service.rs`
- **Finding**: 唯一调用 `DailyLossLimit::report_pnl` 的点是 `RiskEngine::post_check`（`risk_engine.rs:1063`），但 `post_check` 在**生产代码中零调用**（grep 全仓仅 `service.rs:1594` 与 `risk_engine.rs` 内部测试）。生产成交路径 `on_fill`（`risk_engine.rs:1088-1097`）不更新 PnL。gRPC 层 `risk_state.daily_circuit_breaker`/`manual_halt` 也**无任何写入点**（`service.rs:202,226,674` 只定义/读取）。
- **Evidence**:
  ```
  risk_engine.rs:1063   let level = self.daily_loss.report_pnl(pnl);   // 唯一生产调用
  (grep post_check: 仅 service.rs:1594 / risk_engine.rs 测试)
  ```
- **Risk**: `DailyLossLimit` 永远停在 `Normal`，`check_gates`（986-1005）的 `Restricted/Halted` 永不触发，**日内亏损无上限**。
- **Proposed Fix**: 在结算/成交盈亏回填路径调用 `report_pnl`；把 `RiskEngine` 的 `BreachLevel` 同步到 `RiskStateInternal`；补端到端测试。
- **Required Test**: `test_daily_loss_halts_after_threshold_integration`
- **Confidence**: HIGH
- **Status**: TODO

### P0-RISK-002 结算窗口未接线且未知即放行

- **File**: `.../src/fast/risk_engine.rs`
- **Line**: 443-483（`check`）、1115-1121（setter）
- **Finding**: `set_settlement`/`set_settlements` **生产零调用**（仅定义 + 测试）；`service.rs` 的 `settlement_times` 恒为 `Vec::new()`（TODO）。`check()` 对未注册结算时间的 token 直接 `Ok(())`。
- **Risk**: 「结算前 2h 禁开 Buy」规则完全未生效；且设计上缺数据即放行，违反 FAIL-CLOSED。
- **Proposed Fix**: 从 MarketDiscovery 注入 `endTime`；未注册结算时间的 token 走可配置拒绝策略。
- **Required Test**: `test_settlement_window_blocks_buy_when_registered` / `test_unknown_settlement_fail_closed`
- **Confidence**: HIGH
- **Status**: TODO

### P0-RISK-003 Gas 费保护被硬编码 0 绕过

- **File**: `.../src/grpc/service.rs`
- **Line**: 718
- **Finding**: `re.check_and_reserve(&order.token_id, side, notional, order.quantity, 0)` —— gas 参数恒为 `0`，`risk_engine.rs:752-762` 的 `gas_gwei > max(500)` 恒为 false。
- **Risk**: Gas 拥堵至数千 gwei 仍放行，手续费失控。
- **Proposed Fix**: 由 rpc_pool/链上取实时 gas 传入；gas 未知应 fail-closed。
- **Required Test**: `test_gas_protection_rejects_high_gwei`
- **Confidence**: HIGH
- **Status**: TODO

### P0-EXEC-004 实时成交链不生效

- **File**: `.../src/fast/market_events.rs`、`.../src/bin/server.rs`
- **Line**: `market_events.rs:268-277`（无认证头）、`438`（`if let Ok` 吞错）；`server.rs:303-335`（`token_ids` 恒空、默认禁用）
- **Finding**: (1) `poll_trades` 请求 `/data/trades` 不带任何 L2 认证头；(2) 即使 `MARKET_EVENTS_ENABLED=1`，轮询循环传入的 `token_ids` 是硬编码空 `Vec`（`server.rs:328`），`poller.tick(&[])` 不产生任何事件；(3) `tick` 用 `if let Ok(...)` 静默吞掉 `poll_trades` 错误。
- **Risk**: 「成交 → `on_market_trade` → RiskEngine/FillLedger」实时记账链永不产出事件，账面仓位不随成交更新；错误无告警。
- **Proposed Fix**: 注入 wallet+ApiKeyCreds 并对 `/data/trades` 签名；从注册表填充 token 列表；`tick` 对 Err/非 2xx 显式 `error!` 并触发对账降级。
- **Required Test**: `test_poll_trades_attaches_l2_headers` / `test_tick_logs_poll_failure`
- **Confidence**: HIGH（代码事实）；「`/data/trades` 需 L2」为官方文档外部事实
- **Status**: TODO

### P0-EXEC-005 Maker 成交方向未归一化（定级待确认）

- **File**: `.../src/grpc/service.rs`、`.../src/fast/market_events.rs`
- **Line**: `service.rs:347-377`（只用 `trade.side`）；`market_events.rs:345-358`（`trader_side` 原样透传，消费端不读）
- **Finding**: `attribute_order` 会返回 MAKER 订单（本方挂单也会成交），但 `on_market_trade` 记账只用 `trade.side`，从不读取 `trade.trader_side`。若 Polymarket 的 `side` 是 taker 视角，则本方作为 maker 时买卖方向相反。
- **Risk**: 仓位/`notional`/FillLedger 方向错误，风控基于错误仓位决策。
- **Proposed Fix**: `trader_side == "MAKER"` 时对 `side` 取反；恢复路径同步处理。
- **Required Test**: `test_on_market_trade_maker_side_inverted`
- **Confidence**: MEDIUM（代码遗漏确定；`side` 视角需对照官方 data-api 文档）
- **Status**: TODO（**建议第三方按官方文档复核后再定 P0/P1**）

## 四、P1 摘要（详见各 Finding 证据）

| ID | 要点 | 建议 |
|----|------|------|
| P1-RISK-006 | `cancel_order`/`cancel_all` 成功后不 `release_reserve`、不更新 OrderStore；`release_reserve` 仅在下单失败 3 分支调用 | 取消成功回调按 remaining×price 释放预留 + 原子 `mark_cancelled` |
| P1-RISK-007 | 后台 30s 对账 `apply_recovery` 可 `sync_position` 清空 reserved，但循环内不调 `rebuild_open_order_reserves`（仅启动 `server.rs:178` 调一次） | 后台恢复后同样重建 open 预留 |
| P1-RISK-008 | `on_fill` 按 `filled_notional` 冲抵 `limit_price×qty` 预留，价格改善/部分成交后残留永不释放 | 按 order_id 整单释放；或终态清零残余 |
| P1-EXEC-009 | 未知结果订单停留 `Received`，而 `pending_exchange_order_ids()` 排除 `Received`，对账看不到 → 无法回收预留 | 对账输入纳入 `Received` 或给未知结果订单可对账占位 |
| P1-CONSIST-010 | 下单响应 `update_fill(绝对)` 与成交事件 `apply_fill(增量)` 双路写 OrderStore → 提前 Filled / lost update | 取消响应路径写 filled_size，统一由成交事件增量 |
| P1-CONSIST-011 | `state_sync` 的 `PaginatedResponse` 无 `next_cursor`、从不翻页，第一页外数据（trades/orders/positions）被当权威全集 | 实现游标翻页；未确认完整前不得判 NonFilled / 清零仓位 |
| P1-DATA-012 | `parse_json_f64` 对非法/缺失静默 `0.0`（实时路径 `parse_f64` 已要求 Err），恢复可生成 0 价 0 量成交 | 恢复路径复用 `parse_f64`，非法入 `unresolved` |
| P1-SEC-013 | `hmac_sig.rs` secret `URL_SAFE.decode` 失败时回退原始 ASCII 字节 → 用错密钥仍产出“合法”签名 | 解码失败返回 Err，fail-closed |
| P1-EXEC-014 | Mempool 全局容量用 `orders.len()`（历史累计）；`update_status` 转终态不删条目，`sweep` 又跳过 `Confirmed/Cancelled` | 改用活跃 pending 计数；终态条目从 map 移除。**注**：生产未调用 `MempoolMonitor::confirm`，故目前为潜伏缺陷，一旦接线即 P0 |

## 五、P2 摘要

- **可观测性/语义**：`apply_recovery` 返回 `plan.actions.len()` 而非实际应用数（`service.rs:472`，主审已复核）；`cancel_all` 恒返回 `cancelled_count: 0`；幂等返回对 `Rejected/Cancelled` 仍 `success=true` 且 `Filled` 价恒 0；`get_risk_state` 的熔断字段与 RiskEngine 权威状态脱节。
- **数据/记账**：`FillLedger::cancel` 会把已 Filled 记录标 Cancelled 致部分成交永不结算；订单级 `settle` 不回填 fill.pnl；`OrderFill.total_slippage_pct` 累加百分比而非加权平均；`compute_pnl` 未知 side 静默返回 0 且缺 finite 校验。
- **防御纵深**：`on_fill`/`post_check` 缺 finite 校验（NaN 可致上限比较恒 false）；`release_reserve` 静默 clamp 掩盖超额/重复释放。
- **Nonce（当前未接入下单链，全为潜伏）**：`confirm` 无条件推进 watermark（乱序污染）；`tick` 放弃 stuck 时不 `mark_failed`（Pending 永久泄漏）；`reset` 用 `store` 可回退游标致重复 nonce；`bump_nonce` 未强制 RBF 增幅。`next_nonce/mark_pending/register_tx/confirm/mark_failed` 生产零调用（主审已复核）。
- **Mempool 其余**：`stats()` 递归取同一读锁可条件死锁；per-token 限频为固定窗口可放行 ~2×；`on_submitted` 用 `tmp_{orderID}` 空 id 时计数漂移；`token_window < 1s` 时 `as_secs()` 截断为 0 致全拒。
- **Executor（潜伏）**：命中预签名时不校验 `size`（可提交错误数量）；`FastExecutor` 内部 `fill_ledger` 记录把所有 `success` 单记为全额 Filled（当前 `server.rs` 未 `.with_fill_ledger`，休眠，接线即双路重复记账）；`resolve_meta` 缓存未命中写死 `neg_risk=false` 且永久缓存。
- **StateSync 其余**：远端 open order 缺 `id` 被 `filter_map` 静默丢弃可致误判；`references_order` 忽略 `maker_orders` 数组；`fetch_trades` 用 `user=` 参数疑与 CLOB 端点约定不符；去重缓存超 500 直接 `clear()`。
- **OrderStore**：所有访问点 `RwLock::unwrap()`，锁中毒即 panic；纯内存实现，重启失忆（文档已承认 R2）。

## 六、环境与流程 Finding（非代码缺陷）

### ENV-001（P1，流程）Python `pytest` 门禁实际不可用（主审已复核）
- **现象**：venv（`C:\Users\32389\.workbuddy\binaries\python\envs\default`）**未安装 pytest**；`AGENTS.md:29` 声称 `pytest` 为验证门禁，但 `requirements.txt` 未声明该依赖。
- **复位后实测**：装 `pytest` 后 `pytest -q` = **93 passed / 9 failed / 1 error**；装 `pytest-asyncio` + `-o asyncio_mode=auto` 后排除 `test_grpc.py` = **89 passed / 1 error**。
- **根因**：`test_grpc.py`（9 个 async 用例）在 pytest 下挂起（gRPC async 生命周期），但用其自带 `main()`（`python test_grpc.py`）**全绿**；`test_mock.py::test_formatter` 依赖不存在的 fixture `noaa_fc/caiyun_fc`（脚本式 runner 设计）。
- **影响**：文档承诺的「pytest 全绿」无法作为可信门禁；多份测试为脚本式（`main()` + PASS 计数）与 pytest 混用。
- **建议**：补 `pytest.ini`（`asyncio_mode=auto`）+ `conftest.py`（fixtures）或将脚本式测试标注 `__test__ = False`；把 `pytest`/`pytest-asyncio` 写入 `requirements-dev.txt`；修正 `AGENTS.md` 门禁描述。

### ENV-002（P2，环境）
- 本次为跑基线临时安装了 `pytest==9.1.1`、`pytest-asyncio`、`anyio` 到项目 venv（未落盘配置文件）。如需可复现，请补 dev 依赖清单。

## 七、主审验证结论（供 CONSENSUS 参考）

- **Rust 基线**：`cargo test --target x86_64-pc-windows-msvc` = **317 passed**（P1-035 +3）**+ 4 + 18 + 1 doc，全绿**。
- **Python 基线**：见 ENV-001。
- **已核实为正确**：`check_and_reserve` 单写锁内原子（P0-008 成立）；`validate_order_inputs`/`RiskConfig::validate` fail-fast；`TradingGate::ensure_ready` fail-closed 且在 `place_order` 最前端；`Lagged` 已按 P1-034 正确处理；`on_market_trade` 是唯一 `RiskEngine::on_fill` 调用点。
- **未发现**：锁顺序死锁、持锁跨 await（`risk_engine`/`fill_records`/`state_sync` 均为 std 锁且无锁内 await）。

## 八、建议进入 `MASTER_TASKS.md` 的任务（待独立审查员确认后授权）

| 任务组 | 覆盖 Finding | 备注 |
|--------|--------------|------|
| G1 风控闸门接线 | P0-RISK-001/002/003 | 交易语义/Risk Model 阈值不变，仅接线；需 Human 授权 |
| G2 成交链可用性 | P0-EXEC-004 | 含 L2 认证注入 + token 列表来源 |
| G3 Maker 方向归一化 | P0-EXEC-005 | 先由第三方按官方文档确认 `side` 视角 |
| G4 预留生命周期 | P1-RISK-006/007/008、P1-EXEC-009 | 与既有 P1-031/033 延续 |
| G5 对账数据完整性 | P1-CONSIST-011、P1-DATA-012 | 分页 + 解析 fail-closed |
| G6 认证防御 | P1-SEC-013 | HMAC 解码 fail-closed |
| G7 OrderStore 记账一致性 | P1-CONSIST-010 | 响应/事件双路记账 |
| G8 Mempool/Nonce 潜伏缺陷 | P1-EXEC-014、P2 潜伏项 | 接入生产前必须修 |

> ⚠️ **OpenCode 为实施者，不得自行宣告 VERIFIED。** 本报告仅为审查输入；请伯森先生与独立审查 AI 逐条判定 `CONFIRMED/DISPUTED/DEFERRED/REJECTED` 并写入 `CONSENSUS_ROUND_04.md` 后，方可拆分为 `MASTER_TASKS.md` 条目授权实施。
