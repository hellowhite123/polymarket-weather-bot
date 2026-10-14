# MASTER_TASKS.md

**唯一工程修改任务来源。** 实施者（Work Buddy）默认只以本文件作为代码修改授权依据。

- 未经本文件明确授权的任务，实施者不得自行改动交易语义 / Risk Model / Strategy / 资金控制逻辑。
- 每个任务一次只做一个 Task Group，完成后走 `docs/CODE_REVIEW_STANDARD.md` 的验收门禁。
- 状态机：`TODO → IN_PROGRESS → FIXED → VERIFIED → CLOSED`（VERIFIED 由非实施者判定）。

---

## ⛔ 当前总闸：Round 05 分类整改（2026-10-13）＋ Round 06 授权整改（2026-10-14）

> **审查基线**：`99a89f84caa4643779607a0fd13531a8e689c6d6`（10-13）｜ `7c75285` / `a2dde81`（10-14 终审）
> **审查方式**：静态代码审查 + 生产调用链追踪 + 测试与部署配置核查
> **总体状态**：🔴 **`LIVE NO-GO`**（不因分类而改变）
>
> 🆕 **2026-10-14 更新**：
> - **Round 05 已正式晋升 `VERIFIED`（9 项，🔒 禁止回退）**：R5-001+A7、R5-002+A8、R5-003、R5-004、R5-007/R5-008、A2(P1-007)、A4(P1-012)、A5(P1-013)、A6(P1-014)。→ 本文件「分类整改 Task Group · 已有实现、待测试验收（优先级 9）」**已由独立审查闭环**。
> - **Round 06 已授权开工**（冯·诺伊曼 10-14 指令）→ 见「活跃任务 → **阶段六**」：R6-G1 / R6-G2 / R6-G3，含新增 **3 项 P0**（P0-BUYNO-001、P0-GAMMA-001、P0-FORECAST-001）＋ 2 项 P1（P1-RISK-015、P1-CONSIST-016）。
> - 编号与 `BUG_TRACKER.md` **§二-G** 双向对齐（P1-B1-LATEFILL→P1-RISK-015；P1-B2-INDOUBT→P1-CONSIST-016）。
>
> **分类标准（本节口径）**
>
> | 分类 | 定义 | 是否可以关闭 |
> |---|---|---|
> | 已确认问题 | 源码或生产调用链已提供足够证据，证明存在具体缺陷或保护链路缺失 | **不能直接关闭**，须整改并通过验收 |
> | 待验证问题 | 发现可疑代码路径或潜在风险，但尚缺并发测试、运行时证据或完整调用链证明 | 暂不能关闭 |
> | 部分实现 / 尚未关闭 | 部分代码已实现，但存在已知限制、缺少必要功能或尚未完成独立验收 | 不能标记 VERIFIED |
> | 已实现、待验收 | 源码已具备预期实现，但本次没有成功执行相应测试 | 待验收后再决定是否关闭 |
>
> ⚠️ **「问题已确认」与「修复已确认」是两个不同状态。** 已确认的问题仍是 OPEN，直至修复证据和验收结果满足要求。
>
> **优先级表**
>
> | 优先级 | Finding | 当前分类 | 行动 |
> |---|---|---|---|
> | 1 | P0-RISK-001 | 已确认问题 | 补齐 PnL 生产更新与日亏熔断链 |
> | 2 | P0-RISK-002 | 已确认问题 | 补齐结算时间注册及 fail-closed |
> | 3 | P1-RISK-UNIT-001 | 已确认问题 | 统一 shares/USDC 和阻断风控权威（归入 **R6-G1**） |
> | 4 | P1-010 / B4 | 待验证并发后果 | 消除非原子读改写并补并发测试 |
> | 5 | B1 / P1-008 | 部分实现 | 完成预留释放回归测试 |
> | 6 | B2 / P1-009 | 部分实现 | 明确持久化恢复要求并补测试 |
> | 7 | B5 / R5-005 | 待验证 | 完成密钥生命周期审计 |
> | 8 | R5-009 | BLOCKED | 完成 Docker Runtime 验收 |
> | 9 | ~~R5-001/002/003/004/007/008~~ | ✅ **已 VERIFIED（10-14）** | ~~补跑完整测试门禁~~ → 已闭环，禁止回退 |
> | **R6-1** | **P0-BUYNO-001** | Round 06 新增 P0 | **R6-G1**：BUY_NO 概率/市价二次取反 + 限价错位 |
> | **R6-2** | **P0-GAMMA-001** | Round 06 新增 P0 | **R6-G2**：Gamma 字段 JSON 字符串解析 + 静默 Mock 回退 |
> | **R6-3** | **P0-FORECAST-001** | Round 06 新增 P0 | **R6-G2**：非降水指标返回物理量而非概率 |
> | R6-4 | P1-RISK-015 | Round 06 新增 P1 | **R6-G3**：撤单后迟到成交预留二次释放 + 终态倒退 |
> | R6-5 | P1-CONSIST-016 | Round 06 新增 P1 | **R6-G3**：已成交 in-doubt 锁死闸门 + 指纹容差 + cursor 编码 |
>
> **最终裁决**：Round 05 继续 `LIVE NO-GO`。在 P0 全部整改并由独立审查者确认、必要 P1 按既定标准完成验收、Rust/Python 测试门禁通过之前，**不建议进入实盘**。

---

## 活跃任务

### 【分类整改 Task Group】Round 05 已确认问题（P0×2 + P1×1）— 优先级 1~3

> 来源：Round 05 审查问题分类与整改清单 §二「已确认问题」（2026-10-13，基线 `99a89f8`）。
> 红线不变：不得改交易语义 / Risk Model 阈值 / Strategy / 资金控制逻辑；本组**仅做接线与 fail-closed**。

#### P0-RISK-001 日亏熔断没有接入生产盈亏更新链

- **Status**: ⬜ **OPEN / TODO**（分类：已确认问题）
- **Owner**: 待派（实施者）
- **确认依据**: `src/fast/risk_engine.rs:1159–1185` `RiskEngine::post_check` 更新持仓、日亏和回撤；`src/grpc/service.rs:416–425` `settle_fill`/`settle_token` 当前**只调用 `FillLedger` 结算**；生产源码搜索**未发现 `post_check` 的生产调用**（找到的外部调用位于测试代码）。
- **结论**: 日亏熔断实现存在，但**无证据证明真实成交或结算路径会更新日亏状态** → 不能认定为已接入生产。
- **整改要求**:
  1. 明确权威 PnL 数据来源；
  2. 将盈亏更新接入真实成交/结算生命周期；
  3. 保证重复事件不会重复计入盈亏；
  4. 验证熔断触发后交易执行路径**确实拒绝新订单**；
  5. 明确熔断状态的重启恢复与复位规则。
- **Allowed Files**: `polymarket-rust/polymarket-rust/src/grpc/service.rs`、`polymarket-rust/polymarket-rust/src/bin/server.rs`（如需）；`risk_engine.rs` 仅限新增 getter / 接线，**阈值与判定逻辑不改**（如确需改动须先申请）
- **Forbidden**: `strategy_engine/`、RiskConfig 阈值含义、交易语义、资金控制逻辑
- **Required Tests**: PnL 生产调用链测试 + 幂等测试 + 熔断触发后拒单测试
- **关闭条件**: 生产调用链、幂等测试、熔断触发测试及**独立审查**全部通过。
- **关联**: 设计稿 `docs/design/settlement_pnl_risk_wiring.md`（Draft v7，待审查；P0-2 UMA POC BLOCKED → 需 HA-007）

#### P0-RISK-002 结算时间未接入生产注册链

- **Status**: ⬜ **OPEN / TODO**（分类：已确认问题）
- **Owner**: 待派（实施者）
- **确认依据**: `src/fast/risk_engine.rs:529–541` 结算时间需通过 `set_settlement`/`set_settlements` 注册；`risk_engine.rs:547–569` **未注册的 `token_id` 会直接通过结算窗口检查**；`src/grpc/service.rs:234–252` `RiskStateInternal::to_proto()` 当前返回**空的 `settlement_times`**，并注明市场数据获取逻辑尚未实现；**生产初始化路径未发现 `set_settlements` 调用**。
- **结论**: 结算窗口保护**缺少可证明的生产数据注册链**；未注册市场可能绕过该检查。
- **整改要求**:
  1. 接入权威结算时间来源；
  2. 在**启动和市场更新时**注册结算时间；
  3. **缺失、过期或无效的结算时间不得默认放行开仓**（fail-closed）；
  4. 补充临近结算、缺失数据、更新失败等测试。
- **Allowed Files**: `polymarket-rust/polymarket-rust/src/grpc/service.rs`、`polymarket-rust/polymarket-rust/src/bin/server.rs`、`polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`（仅注册接口/接线）
- **Forbidden**: `strategy_engine/`、RiskConfig 阈值含义、交易语义
- **Required Tests**: `test_settlement_window_blocks_buy_when_registered` + `test_unknown_settlement_fail_closed`（未注册/过期 → 按约定拒绝并告警）+ 更新失败测试
- **关闭条件**: 生产注册链和 fail-closed 行为**均有测试与独立审查证据**。
- **关联**: 设计稿 `docs/design/update_settlement_times_rpc.md`（Draft v3，待审查）；「未注册结算时间」的处置口径需伯森先生先确认。

#### P1-RISK-UNIT-001 Python 风控混用 shares 与 USDC

- **Status**: ⬜ **OPEN**（分类：已确认问题）｜**前置：proto 契约变更须 Human 授权**
- **Owner**: 待派（实施者）
- **确认依据**: `strategy_engine/aggregator/risk_gate.py:131–142` 单市场检查将 `order.quantity` 加到持仓数量后与风险限额比较；`risk_gate.py:151–160` 全局暴露检查同样；`strategy_engine/aggregator/models.py:165–202` `positions` 表数量、`position_notionals` 表 USDC 名义价值，**名义价值缺失时可能退回数量之和**；`src/fast/risk_engine.rs:243–319` Rust 风控以 USDC 名义价值执行检查与预留；`proto/polymarket.proto:99–120` `RiskState` **当前没有 `position_notionals` 字段**；`strategy_engine/grpc_bridge/converter.py:92–111`、`152–170` 转换链**没有传输或恢复名义价值字段**。
- **结论**: Python 风控存在单位混用，新增订单数量与 USDC 暴露相加，**可能造成错误拒单或错误放行**。
- **整改要求**:
  1. 按**冻结的 B3 设计**明确 **Rust 为最终阻断风控权威**；
  2. Python advisory 检查**不得将 shares 当作 USDC**；
  3. 补齐名义价值数据的权威来源及传输链；
  4. **未经授权不得擅自修改 Proto 契约或 RiskConfig 阈值**；
  5. 对齐 Python/Rust 配置来源及有效限额。
- **Allowed Files**: 待授权确认后填写（涉及 `strategy_engine/aggregator/risk_gate.py`、`models.py`、`grpc_bridge/converter.py`、`proto/polymarket.proto`——**proto 变更须先取得 Human 明确授权**）
- **Forbidden**: 未授权修改 proto 契约、RiskConfig 阈值、聚合器与 gRPC 契约
- **关闭条件**: 单位一致性、接口传输、生产调用链及边界测试**全部通过**。
- **关联**: `docs/design/ROUND05_B_DESIGN_DRAFTS.md`（B3）；`docs/R5B_REVIEW_FINDINGS_2026-10-12.md` §1
- ⚠️ **R5-002 与 P1-RISK-UNIT-001 必须继续作为不同 Finding 追踪，不应合并。**

---

### 【分类整改 Task Group】Round 05 待验证问题（优先级 4、7、8）

#### P1-010 / B4 下单响应可能覆盖并发成交状态

- **Status**: 🟡 **OPEN，待并发测试**（分类：待验证问题——已发现具体风险路径，最终并发后果仍待验证）
- **Owner**: 待派（实施者）
- **证据**: `src/grpc/service.rs:1023–1042` 下单响应路径执行「读取订单副本 → 修改副本 → `upsert` 写回」；`service.rs:512–517` 成交事件路径调用 `apply_fill_by_exchange_order_id`；`src/fast/order_state.rs:208–225` 成交事件更新在**单次写锁**中执行。
- **风险**: 若成交事件在下单响应读取旧订单后、写回前发生，**旧副本可能覆盖新的成交状态**。
- **已确认事实**: 两条路径存在**非原子读改写**与**原子事件更新**的差异。
- **尚未确认事实**: 真实并发交错是否稳定触发状态覆盖，以及是否进一步造成 `RiskEngine`/`FillLedger`/订单状态不一致。
  - ⚠️ **补充复核（`docs/R5B_REVIEW_FINDINGS_2026-10-12.md` §3）**：所述「成交事件先更新后被 upsert 覆盖」的机制**不可达**——窗口期内订单尚无 `exchange_order_id`，事件路径按 exchange id 定位必然命中不了。真实可达隐患是：**丢成交 / 覆盖 B2 绑定 / 潜在重复计数**。
- **整改要求**:
  1. 将订单状态更新改为**原子状态转换**；
  2. **禁止旧订单副本整体覆盖**更新后的状态；
  3. 明确**累计成交快照与增量成交事件的合并规则**；
  4. 补充下单响应、成交、撤单交错的**并发测试**。
- **Allowed Files**: `polymarket-rust/polymarket-rust/src/grpc/service.rs`、`polymarket-rust/polymarket-rust/src/fast/order_state.rs`（新增原子方法，语义参照既有 `apply_fill_by_exchange_order_id`）
- **Forbidden**: `strategy_engine/`、`risk_engine.rs` 阈值、交易语义
- **关闭条件**: 修复后并发测试通过，并**证明订单状态、预留、成交账本保持一致**。

#### B5 / R5-005 热钱包密钥缓存的实际清理效果

- **Status**: 🟡 **待验证**（分类：已确认实现部分零化措施，但实际生命周期行为尚未完成验证）
- **Owner**: 待派（实施者/审查者）
- **证据**: `src/fast/wallet/keystore.rs:94–131` 加密路径使用 `Zeroizing`；`keystore.rs:146–170` 解密路径使用 `Zeroizing`；`src/fast/wallet/manager.rs:83–92` 解密字符串用于解析 `LocalWallet`；`src/fast/hot_wallet.rs:24–30`、`312–322` 热钱包缓存采用 TTL、驱逐和 best-effort 清理。
- **已知限制**: 清理解密明文**不等于**证明 `LocalWallet` 内部所有密钥副本都已清除；`Arc<LocalWallet>` 意味着**缓存驱逐不必然等于对象立即销毁**。
- **待验证内容**:
  1. TTL 到期后的缓存清理行为；
  2. 锁定、驱逐及最后一个引用释放的生命周期；
  3. 密钥材料是否存在**未使用零化容器的副本**；
  4. 是否有意外日志输出或长期缓存。
- **Allowed Files**: `polymarket-rust/polymarket-rust/src/fast/hot_wallet.rs`、`src/wallet/keystore.rs`、`src/wallet/manager.rs`（仅零化增强，**不改签名算法/PBKDF2/AES-GCM/cold_wallet**）
- **关闭条件**: 完成**生命周期审计**、相关测试及 best-effort 保证边界说明。

#### R5-009 Docker 健康检查运行时行为

- **Status**: ⛔ **BLOCKED**（分类：静态实现已存在，运行时结果待验证）｜**代码侧已冻结，不得再改（除非新 Finding）**
- **Owner**: 待派（有 Docker 主机的环境）
- **证据**: `docker-compose.yml:47–61` 已包含检查 `market_discovery` 进程的健康探针和失败重试配置。
- **待验证内容**:
  1. 正常启动时健康检查是否通过；
  2. 停止目标进程后是否在规定失败次数后转为 `unhealthy`；
  3. 容器是否按设计**保持运行**；
  4. 恢复进程后健康状态是否恢复；
  5. 进程匹配是否可能误判其他进程。
- **关闭条件**: 完成**真实 Docker Compose 运行测试并保存输出**。
- **验收手册**: `docs/DEPLOY_HEALTHCHECK_RUNBOOK.md` §5（5 步实机验收）

---

### 【分类整改 Task Group】Round 05 部分实现 / 尚未关闭（优先级 5、6）

> ⚠️ 本组**不能仅凭代码存在就标记 VERIFIED**。

#### B1 / P1-008 订单级预留释放

- **Status**: 🟡 **部分实现，待独立验收**（分类：部分实现 / 尚未关闭）
- **Owner**: 待派（独立审查者）
- **已实现的代码路径**:
  - `src/grpc/service.rs:452–480` 成交时按**订单限价**计算预留释放；
  - `service.rs:615–655` 撤单**先转换状态**，再释放预留；
  - `src/fast/risk_engine.rs:358–375` 区分**预留释放**与**实际成交暴露**；
  - `risk_engine.rs:1041–1051` 集中度检查采用 **committed 口径**。
- **尚未完成的验证**:
  1. 多笔同 token、不同限价订单；
  2. 部分成交和价格改善；
  3. 撤单与成交并发；
  4. **拒单状态转换失败时是否仍释放预留**；
  5. 未知身份订单的后续恢复。
- **状态判定**: **不能仅凭上述代码存在就标记 VERIFIED。**

#### B2 / P1-009 未知订单对账与跨重启恢复

- **Status**: 🟡 **部分实现**（分类：部分实现 / 尚未关闭）
- **Owner**: 待派（实施者 + 独立审查者）
- **已实现的代码路径**:
  - `src/fast/order_state.rs:457–501` 识别 in-doubt 订单并进行**唯一绑定保护**；
  - `src/fast/state_sync.rs:1110–1119` 歧义订单保持 unresolved，**不因 TTL 自动释放预留**；
  - `src/bin/server.rs:176–208` 启动时执行匹配；
  - `server.rs:249–260` 未解决订单**阻止 TradingGate 进入 READY**。
- **已知缺口**: `state_sync.rs:1119` 明确记载当前状态**只在内存中，重启后不会恢复**；持久化恢复测试**被延期**。
- **整改要求**:
  1. 明确**是否要求**未知订单跨进程重启持续恢复；
  2. 如果 Design V2 要求持久化，补齐**订单状态和预留状态的持久化**；
  3. 在恢复完成前**保持交易闸门关闭**；
  4. 补充**真实重启恢复测试**。
- **状态判定**: 唯一匹配与 fail-closed 部分已有实现，但**不能认定完整恢复能力已经通过**。
- 关联：`docs/R5B_REVIEW_FINDINGS_2026-10-12.md` §2（现有延期属 **DCR-001 方案 A** 已批准，Human 2026-10-04）；建议补文档注记说明「启动 in-doubt 约束依赖 R2-P2 持久化，当前为空转」。

---

### 【分类整改 Task Group】Round 05 已有实现、待测试验收（优先级 9）

> **本次静态检查中相关源码与测试文件存在；但由于 Rust 测试没有执行、Python 测试收集失败，本次不能独立确认回归测试通过。**
> 后续应**逐项运行对应测试，保留命令、退出码和输出**。

| 项 | 内容 | 状态 |
|----|------|------|
| R5-001 | 集中度检查 TOCTOU（分母漏计 pending 预留）+ Sell 分支修正 | 已有实现、待验收（09-18 独立审查曾判 ✅ CONFIRMED，仍需补跑门禁） |
| R5-002 | 建议仓位与资本脱节 — MultiSource `_calculate_quantity` | 已有实现、待验收（与 P1-RISK-UNIT-001 **分开追踪，不得合并**） |
| R5-003 | `clean_forecast` 原地修改输入 | 已有实现、待验收 |
| R5-004 | `AdaptiveWeighter` 最小权重语义 | 已有实现、待验收 |
| R5-007 | 权益曲线年化因子硬编码 | 已有实现、待验收 |
| R5-008 | 持仓时间/分组统计用 `token_id` 表匹配 | 已有实现、待验收 |

- **Required Tests**: 逐项运行对应 Rust/Python 测试，**保留命令 + 退出码 + 输出**。
- **关闭条件**: 完整测试门禁通过 + 独立审查确认。

---

### 【测试环境问题】TEST-ENV-002 Rust/Python 门禁未能执行（2026-10-13）

> 分类：**测试环境/依赖一致性问题**，不能直接认定为业务逻辑测试失败。

#### TEST-ENV-002-A Python Protobuf 版本不匹配

- **现象**: 当前运行时 `protobuf 6.33.6`，生成代码要求 `protobuf 7.35.1` → **三个测试模块在收集阶段报错**。
- **后续操作**:
  1. 核对 `requirements.txt` 与 `requirements-dev.txt`；
  2. 按**项目指定版本**重建干净环境；
  3. **不得通过未经授权的依赖升级绕过问题**；
  4. 重新运行完整 Python 测试。
- **Status**: ⬜ OPEN

#### TEST-ENV-002-B Rust 工具链缺失

- **现象**: 本次环境**没有安装 Cargo**，因此未运行 `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`。
- **后续操作**: **必须在指定的 Windows MSVC 环境补跑完整门禁。**
- **Status**: ⬜ OPEN
- ⚠️ 注意既有基线约束：全库 `cargo fmt --check` / `cargo clippy -D warnings` = **PRE-EXISTING BASELINE FAILURE**（非本次引入）；**禁止声称 clippy PASS**，**禁止借整改之名扩大范围清理**。

---

## 阶段一（09-13 授权开工）：Trade 一致性链 P0-013 / P0-014 / P0-015

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

### 阶段六（2026-10-14 授权开工）：Round 06 双 AI 共识缺陷与上下游全链路整改（R6-G1 ~ R6-G3）

> **授权来源**：冯·诺伊曼（2026-10-14 指令：剔除过时重复项，核心层与上游同步解决）。
> **通用红线（Forbidden）**：
> 1. 严禁返工已 `VERIFIED` 的历史任务（`P0-002` 密码读取、`P0-004` `claim_or_retry`、`R5-001` 集中度公式、`R5-002` MultiSource `_calculate_quantity` 公式、`ENV-001` pytest 配置）。
> 2. 严禁修改 `RiskConfig` 阈值定义、冷钱包与 EIP-712 签名标准实现、gRPC `polymarket.proto` 消息结构。
> 3. 实施者（OpenCode）完成后仅可将状态置为 `FIXED` 并附 `git diff` + 测试输出，**不得自行标记 `VERIFIED` 或 `CLOSED`**。
>
> **统一编号说明**（与 `BUG_TRACKER.md` §二-G 对齐，双向一致）：
>
> | 本文档「关联编号」 | 统一编号 | 说明 |
> |-------------------|---------|------|
> | `P0-BUYNO-001` | **P0-BUYNO-001** | 原名保留（已符合 `P0-<域>-<NNN>` 约定） |
> | `P1-RISK-UNIT-001 (B3)` | **P1-RISK-UNIT-001** | 再登记，沿用既有编号 |
> | `P0-GAMMA-001` | **P0-GAMMA-001** | 原名保留 |
> | `P0-FORECAST-001` | **P0-FORECAST-001** | 原名保留 |
> | `P1-B1-LATEFILL` | **P1-RISK-015** | ⚠️ `B1` 不得单独作 Finding ID（已表 Round 05 设计批次 B1）；本项为 B1 二审追加发现 |
> | `P1-B2-INDOUBT` | **P1-CONSIST-016** | ⚠️ 同理，`B2` 为 Round 05 设计批次 B2；本项为 B2 二审追加发现 |
> | `A3 (Gap G8)` | **A3 / P1-011** | A3 即 `阶段五` 的 A3 游标分页项；`G8` 为 Round 04 候选任务组的 Mempool/Nonce 潜伏缺陷分组 |
>
> 📌 状态同步口径：本节仅登记 `TODO` / `FIXED`；`VERIFIED` / `CLOSED` 由非实施者判定并回写 `BUG_TRACKER.md` §二-G。

---

#### R6-G1（P0/P1）Python `BUY_NO` 概率/定价链路修复与 `RiskGate` USDC 单位统一

- **Status**: ⬜ TODO（已授权）
- **Owner**: OpenCode
- **关联编号**: `P0-BUYNO-001`、`P1-RISK-UNIT-001 (B3)`
- **Allowed Files**:
  - `strategy_engine/strategies/multi_source.py`
  - `strategy_engine/strategies/forecast_deviation.py`
  - `strategy_engine/strategies/nowcast_correction.py`
  - `strategy_engine/strategies/historical_statistical.py`
  - `strategy_engine/aggregator/position_sizer.py`
  - `strategy_engine/aggregator/models.py`
  - `strategy_engine/aggregator/aggregator.py`
  - `strategy_engine/aggregator/risk_gate.py`
  - `strategy_engine/grpc_bridge/converter.py`
  - `test_aggregator.py`、`test_multi_source.py`
- **Forbidden**:
  - 不得修改 `multi_source.py:_calculate_quantity` 的 R5-002 饱和公式；
  - 不得修改 `proto/polymarket.proto`。
- **改法**:
  1. **内部概率与市场价口径统一为 YES 参考系**：策略输出 `TradingSignal` 时，`target_price` 统一表示模型预测的 $P(\text{YES})$，`market_price` 统一表示当前市场的 $P_{\text{mkt}}(\text{YES})$（即 `market.implied_probability` / `yes_price`），消除上游传 `no_price`、下游再算 `1 - price` 的二次取反 Bug。
  2. **修复 `PositionSizer._kelly` 的 `BUY_NO` 胜率**：在 `BUY_NO` 分支中，单位成本 `cost = 1.0 - price`，赔率 `b = price / (1.0 - price)`，获胜概率必须设为 `p_win = 1.0 - p`（即 $P(\text{NO})$），失败概率 `q_lose = p`。
  3. **区分公平概率与执行限价/成本**：在 `AggregatedOrder` 中提供 `unit_cost_usdc` 与 `execution_target_price` 属性（`BUY_YES` 时为 `market_price` / `target_price`；`BUY_NO` 时为 `1 - market_price` / `1 - target_price`）。在 `grpc_bridge/converter.py` 转 gRPC 下单请求时，使用对应方向 token 的真实目标限价。
  4. **统一 `RiskGate` 后检查为 USDC Notional**：在 `risk_gate.py` 的 `_check_position_limit` 和 `_check_global_exposure` 中，使用 `order_notional = order.quantity * order.unit_cost_usdc`（USDC），并验证价格合法性（`NaN/Inf/<=0` 时 Fail-Closed 拦截）；单市场持仓优先读取 `state.get_position_notional(token_id)`；`_check_daily_loss` 边界由 `<` 修正为 `<=`。
- **Required Tests**:
  - `test_kelly_buy_no_realistic`：给定 $P(\text{YES})=0.20$、市场 $P_{\text{mkt}}(\text{YES})=0.55$、`direction=BUY_NO`，断言 `f_star > 0` 且计算出正数 `quantity`。
  - `test_risk_gate_uses_usdc_notional_not_shares`：给定 `max_position_per_market=500 USDC`，已持仓 `200 USDC`，新增买入 `400 shares @ $0.50`（即 `200 USDC`），总暴露 `400 USDC <= 500 USDC` 应 **PASS**（旧逻辑 `200 + 400 = 600 > 500` 会误拒）。
  - `test_risk_gate_buy_no_notional_uses_no_cost`：验证 `BUY_NO` 时按 `quantity * (1 - yes_price)` 计算风控敞口。
- **Acceptance**: `python -m pytest -q` 全绿。

---

#### R6-G2（P0）上游市场发现 Gamma JSON 解析加固与气象指标概率化防线

- **Status**: ⬜ TODO（已授权）
- **Owner**: OpenCode
- **关联编号**: `P0-GAMMA-001`、`P0-FORECAST-001`
- **Allowed Files**:
  - `market_discovery/models.py`
  - `market_discovery/gamma_client.py`
  - `strategy_engine/strategies/forecast_utils.py`
  - `test_market_discovery.py`
- **Forbidden**: `polymarket-rust/`、策略核心公式。
- **改法**:
  1. **Gamma 字段兼容解析**：在 `market_discovery/models.py` 中支持 `outcomes`、`clobTokenIds`（及 V2 `positionIds` 回退）为 JSON 字符串（`json.loads`）或原生列表；严格按 `"YES"` / `"NO"` 文本标签对齐 token_id，禁止盲目假设下标 0 为 YES。
  2. **禁止实盘静默回退 Mock**：在 `gamma_client.py` 中增加 `allow_mock_fallback: bool` 控制（默认从环境变量读取，生产/实盘模式下为 `False`），当 API 请求或解析失败且禁止 fallback 时，显式抛出异常或返回空并打印 `ERROR` 告警，绝不静默生成假市场。
  3. **气象指标 `[0, 1]` 概率守卫**：在 `forecast_utils.py:extract_probability` 中，确保返回值严格处于 `[0.0, 1.0]` 概率空间；若提取的是温度/风速等未完成阈值概率化转换的原始物理量（或超出 `[0, 1]`），必须通过阈值正态 CDF/Sigmoid 映射为概率，若无阈值上下文则返回 `None` 跳过该市场（Fail-Closed），严禁把摄氏度/风速数值直接当作概率返回。
- **Required Tests**:
  - `test_gamma_parses_json_string_outcomes_and_tokens`：用官方 JSON 字符串格式 `'["Yes", "No"]'` 与 `'["tok_yes", "tok_no"]'` 验证解析成功。
  - `test_gamma_no_silent_mock_when_disabled`：验证 `allow_mock_fallback=False` 时 API 异常不产生 Mock 市场。
  - `test_extract_probability_always_in_unit_interval`：验证 `extract_probability` 对任意事件类型只返回 `None` 或 `[0.0, 1.0]` 内的浮点数。
- **Acceptance**: `python -m pytest -q` 全绿。

---

#### R6-G3（P1）Rust 执行层 `B1` 迟到成交防双释、`B2` 已成交 In-Doubt 解绑与 `A3` 游标编码

- **Status**: ⬜ TODO（已授权）
- **Owner**: OpenCode
- **关联编号**: `P1-B1-LATEFILL`（统一编号 **P1-RISK-015**）、`P1-B2-INDOUBT`（统一编号 **P1-CONSIST-016**）、`A3 (Gap G8)`（＝ A3 / P1-011）
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/order_state.rs`
  - `polymarket-rust/polymarket-rust/src/fast/state_sync.rs`
  - `polymarket-rust/polymarket-rust/src/grpc/service.rs`
  - `polymarket-rust/polymarket-rust/src/bin/server.rs`
- **Forbidden**: `strategy_engine/`、`RiskConfig` 阈值、签名与钱包模块。
- **改法**:
  1. **`B1` 防止取消后迟到成交二次释放预留**：
     - `OrderStore::apply_fill_by_exchange_order_id` 改为返回 `Option<(OrderState, f64)>`（即 `(更新后状态, 本次实际扣减的 remaining_size 增量 actual_delta)`）；
     - 若订单已处于 `Cancelled / Rejected / Expired / Filled` 终态，状态保持不变（不倒退）且 `actual_delta = 0.0`；
     - `service.rs::on_market_trade` 先调用该方法拿到 `actual_delta`，仅按 `actual_delta * st.limit_price` 计算 `reserve_release`，杜绝已取消订单再次释放预留偷扣其他挂单额度。
  2. **`B2` 支持已立即成交的 In-Doubt 订单通过 `/data/trades` 对账解绑 + 时间戳容差**：
     - 在 `state_sync.rs` 的 `fingerprint_match` 中，将 `created_at` 严格相等改为 `rc.abs_diff(local.created_at) <= 60`（60 秒容差）；
     - 若 `in_doubt` 订单在 `remote_open_orders` 中未命中，继续在本次对账已拉取的 `remote_trades` 中按 `client_order_id` 或唯一指纹匹配：若唯一命中成交记录，则将其绑定 `exchange_order_id` 并通过恢复计划触发成交入账与预留冲抵，不再永久滞留 `unresolved` 锁死 `TradingGate`。
  3. **`A3` 分页 Cursor URL 编码**：
     - 在 `state_sync.rs` 的 `fetch_positions`、`fetch_open_orders`、`fetch_trades` 中，对 `next_cursor` 做百分号编码（Percent-Encoding，处理 `=`、`+`、`/`），确保 HTTP 请求 URL 与 L2 HMAC 签名路径严格一致。
- **Required Tests**:
  - `test_b1_late_fill_after_cancel_does_not_double_release_reserve`：挂单 A（reserve 60）与挂单 B（reserve 40）同 token；A 先 `cancel` 释放 60（剩余总预留 40），随后收到 A 的迟到 `TradeEvent`，断言总预留仍为 40（不误扣 B 的预留），且 A 状态不倒退。
  - `test_b2_in_doubt_filled_immediately_resolved_via_trades`：构造 `in-doubt` 订单不在 `open_orders` 但出现在 `remote_trades`，断言成功解绑并释放预留，`unresolved == 0`。
  - `test_b2_fingerprint_allows_timestamp_drift_within_tolerance`：本地与远端 `created_at` 相差 5 秒仍唯一匹配成功。
  - `test_a3_pagination_cursor_url_encoded_in_l2_signature`：含 `=` 与 `/` 的 Base64 游标正确编码并完成翻页。
- **Acceptance**: `cargo test` 全绿；`cargo clippy --all-targets` 0 error。

> ⚠️ **门禁前置提醒**：本组含 Rust 测试，须在 Windows MSVC 环境执行——
> `PROTOC=C:/Users/32389/tools/protoc/bin/protoc.exe cargo test --target x86_64-pc-windows-msvc`。
> 另注意已知基线约束：全库 `cargo fmt --check` / `cargo clippy -D warnings` = **PRE-EXISTING BASELINE FAILURE**（非本次引入）；**禁止声称 clippy PASS**，**禁止借本组之名扩大范围清理 baseline lint**。

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
  - 🔁 **2026-10-13 复审再确认（分类：已确认问题）**：见本文件顶部「已确认问题」组——`risk_engine.rs:1159–1185` `post_check` **生产零调用**（`service.rs:416–425` 只调 `FillLedger`），日亏熔断不能认定已接入生产；`risk_engine.rs:547–569` **未注册 token_id 直接通过**结算窗口检查，`service.rs:234–252` `settlement_times` 恒空、生产无 `set_settlements`。两项维持 **OPEN**。
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

---

### 阶段五（Round 05 P1 整改，commit `7c75285`）：A1–A8 + B1 + B2 已实施 / B3–B5 待授权 / C1 BLOCKED

> 来源：`round 5.md`（Round 05 P1 独立二审）+ `docs/INSTRUCTION_TO_OPENCODE_ROUND05_P1_REMEDIATION.md`。
> 红线：不得改交易语义 / RiskConfig 阈值 / proto / cold wallet；**不得由实施者写 VERIFIED/CLOSED**。
> 基线→新：本轮改动未单独记录基线 commit（见 `git show 7c75285`）。

| 项 | 内容 | Allowed | 状态 |
|----|------|---------|------|
| A1 / P1-006 | 取消订单：**先转换成功再释放** reserve（client+exchange；`limit_price≤0` fail-closed） | `grpc/service.rs` | 🔧 FIXED（5 测试 + mutation 2 FAILED） |
| A2 / P1-007 | reconcile 预留重建失败 → **强制不进入 READY**（启动+后台 fail-closed） | `bin/server.rs` | 🔧 FIXED（代码；服务端集成测试待真实运行/Docker） |
| A3 / P1-011 | positions/orders/trades **游标分页**（推进/去重/页数上限/中间页错误 fail-closed） | `fast/state_sync.rs` | 🔧 FIXED（5 测试） |
| A4 / P1-012 | `parse_json_f64→Result`；非法恢复成交进 `unresolved` | `fast/state_sync.rs` | 🔧 FIXED（3 测试） |
| A5 / P1-013 | 删 HMAC raw ASCII fallback；空/非法 secret fail-closed | `clob/signing/hmac_sig.rs`、`clob/auth.rs` | 🔧 FIXED（官方 vector + 2 测试） |
| A6 / P1-014 | 全局容量只计 active；terminal 释放容量 | `fast/mempool.rs` | 🔧 FIXED（3 测试 + mutation 3 FAILED） |
| A7 / R5-001 分子 | 集中度**分子**改用 `committed(filled+reserved)` | `fast/risk_engine.rs` | 🔧 FIXED（1 测试 + mutation FAILED） |
| A8 / R5-002 | cap 在 **USDC notional** 层（补测试，不改公式） | `test_multi_source.py` | 🔧 FIXED（3 测试） |
| B1 / P1-008 | on_fill 预留释放改按**订单限价**（成交暴露按执行价；身份/限价未知**保守持有**待对账） | `fast/risk_engine.rs`、`grpc/service.rs` | 🔧 FIXED（8 测试 + mutation 7 FAILED） |
| B2 / P1-009 | in-doubt（Received 无 exchange id）对账候选：唯一匹配→Submitted；ambiguous/unresolved 保守持有预留（无 TTL 释放）；闸门非 READY；进程内幂等重放 no-op | `fast/order_state.rs`、`grpc/service.rs`、`bin/server.rs`、`fast/state_sync.rs` | 🔧 FIXED（15 测试 + mutation 2×3 FAILED；durable-restart 测试 DEFERRED per DCR-001 方案 A） |
| B3–B5 | P1-RISK-UNIT-001 / P1-010 / R5-005 | `docs/design/ROUND05_B_DESIGN_DRAFTS.md` | ⛔ WAITING_AUTHORIZATION（B3 需先确认 proto `RiskState` 配置字段变更） |
| C1 / R5-009 | Docker runtime 验收 | — | ⛔ BLOCKED（环境无 Docker） |

- 验证（B1 后）：`cargo test` 386 passed；`pytest` 126 passed；`cargo clippy --all-targets` 0 error。
- 验证（B2 后）：`cargo test` **379 + 4 + 18 + 1 = 402 passed**（另 1 ignored = B2 durable-restart DEFERRED）；`pytest` **126 passed（exit 0）**；`cargo clippy --all-targets` **0 error**（警告均为既有基线）；mutation：ambiguous 自动绑定 → 3 FAILED、gate 忽略 unresolved → 3 FAILED（均已还原）。
- **未自证 VERIFIED/CLOSED**；状态由独立审查者更新。
