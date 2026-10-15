# 交易数据库设计 — PostgreSQL Schema（预备工程）

- **状态**：**Draft v3.1 — 收尾定稿（v3 裁决 + v3.1 多账户隔离唯一键与 SQL 边界补丁）**；仍仅设计 + 独立测试，**未接入任何生产代码**
- **作者**：旺财（WorkBuddy）
- **日期**：2026-10-14（v1）/ 2026-10-15（v2 整改 / v3 定稿 / v3.1 收尾补丁）
- **授权依据**：伯森先生《WorkBuddy 数据库预备工程任务》（2026-10-14）＋《第二轮审查整改任务》（2026-10-15）＋《v3 定稿指令》（2026-10-15，Gemini 修订）＋《v3.1 收尾补丁指令》（2026-10-15）
- **证据基线**：commit `6722efd`（分支 `p0-blockers-2026-09-13`）
- **关联**：`docs/design/settlement_pnl_risk_wiring.md`（G1-001 v7）、`docs/design/DCR-001-B2-durable-persistence.md`、`MASTER_TASKS.md`（P0-RISK-002 / P1-RISK-UNIT-001）

> **v3 定稿摘要**（裁决与盲区修正详见 §9.8）：
> - **冻结裁决**：DB-HA-1 = Fail-Open（交易连续性优先）；DB-HA-2 = **PostgreSQL 唯一验收**（放弃 SQLite）；DB-HA-3 = `signals` 加内部代理主键 `signal_pk`（不碰 Python 源码，128-bit UUID 记技术债）；DB-HA-4 = 核心交易表引入 `account_id`。
> - **4 项底层盲区修正**：① spill `fsync` 移出 Rust 热路径（线程边界）；② `orders`/`positions` 加 `last_source_seq` 水位线 + 乱序 UPSERT 守卫；③ `forecast_runs` 唯一键规避 PG `NULL` 陷阱（`NOT NULL DEFAULT ''`）；④ `settlements.outcome` 支持 `0 / 0.5 / 1` 争议结算。
> - **本轮交付**：最终版 `db/migrations/0001_init.up/down.sql` + 补齐 V-7 并发幂等 / 乱序防御 / D1 并发 / outcome=0.5 测试（真实 PG 17.11）。

> **v3.1 收尾补丁摘要**（详见 §9.9）：多账户隔离唯一键 2 处（`fills` 唯一键加 `account_id`；`settlements`/`accounting_effects`/`pnl_records` 复合主键与外键）+ SQL 边界 3 处（`last_source_seq` 改 `NOT NULL DEFAULT 0` 并简化守卫、`fk_signals_market` 列级 `SET NULL (market_ref)`、`orders` 数量守恒约束）+ 测试补强 4 处（`_concurrent` 断言、D1 双表事务、多账户隔离、signal_id FAIL-CLOSED）。

## 标注约定（全文适用）

| 标注 | 含义 |
|------|------|
| 【已确认】 | 可直接追溯到现有代码/文档的事实（附 file:line 或文档名） |
| 【新增设计】 | 现有代码中没有对应物，本轮新提出的设计 |
| 【待验证】 | 需要 POC / Human 确认 / 依赖未闭环 P0·P1 的事项 |

> ⚠️ **诚实声明**：本设计产生的所有表当前**无任何生产写入路径**。本文档不声称任何「已完成的交易持久化功能」。

---

## 1. 审查结论摘要（第一步产出）

### 1.1 现有数据模型盘点

| 领域 | 现有权威模型 | 位置 | 持久化现状 |
|------|-------------|------|-----------|
| 订单 | `OrderState` / `OrderStore`（8 态状态机 + 原子 claim） | `fast/order_state.rs:30-501` | 纯内存，重启即失忆【已确认】 |
| 成交 | `FillRecord` / `OrderFill` / `FillLedger`（fill 级 + 订单级聚合双索引） | `fast/fill_records.rs:74-503` | 纯内存【已确认】 |
| 持仓 | `PositionLimits.PositionEntry {qty, notional, reserved}` + `RemotePosition` | `fast/risk_engine.rs:219-224`、`fast/state_sync.rs:37-53` | 纯内存【已确认】 |
| 风控 | `RiskConfig` / `DailyLossLimit` / `BreachLevel` / `RiskCheckResult` | `fast/risk_engine.rs:80-127,597-702`、`proto/polymarket.proto:57-61` | 纯内存；`post_check` 生产零调用（P0-RISK-001 未闭环）【已确认】 |
| 结算 | `SettlementEvent` + G1-001 v7 `SettlementRecord`/`AccountingEffect`（**仅设计，未实现**） | `fast/market_events.rs:110-115`、`settlement_pnl_risk_wiring.md §5.2` | 无；UMA POC BLOCKED（HA-007）【待验证】 |
| 信号 | `TradingSignal`（含 signal_id/策略/定价/置信度/生命周期） | `strategy_engine/models.py:130-207` | 仅写 `data/signals/*.json` 文件【已确认】 |
| 预报 | `WeatherForecast` / `ForecastPeriod` / `SourceMeta` | `weather_data_ingestion/models.py:102-214` | 仅写 `data/raw/*.json` 文件【已确认】 |
| 市场 | `MarketInfo` + `MarketRegistry`（JSON 注册表） | `market_discovery/models.py:64-86`、`registry.py` | `data/market_registry.json`【已确认】 |
| 对账 | `ReconciliationReport` / `RecoveryPlan` / `InDoubtOrder` | `fast/state_sync.rs:175-265,1124-1150` | 纯内存【已确认】 |

### 1.2 关键缺口（数据库要解决什么）

1. **全部交易状态纯内存**：`OrderStore`/`FillLedger`/`RiskEngine` 持仓均无落盘，重启即失忆（`order_state.rs:9` 注释自述「生产需持久化」；DCR-001 方案 A 明确「不支持跨重启」）【已确认】
2. **结算时间未注册即放行**：P0-RISK-002 未闭环——`TimeWindowGuard.settlements` 无注册来源，未知即放行【已确认，见 `update_settlement_times_rpc.md`】
3. **UMA settlement identity 未验证**：`settlement_id` 派生算法 POC BLOCKED（G1-001 v7 §4，HA-007）【待验证】
4. **signal → order 关联缺失**：`AggregatedOrder` 只带 `contributing_strategies`（策略名列表），**不带 signal_id**（`aggregator/models.py:94-96`）【已确认】→ 关联表设计见 §4.5，标注【新增设计】
5. **数量单位语义未闭环**：P1-RISK-UNIT-001（RiskGate shares/USDC 单位）未修复，schema 中 `quantity`/`size` 一律按现有代码口径存 **shares**，金额一律 **USDC**，但单位语义本身【待验证】

---

## 2. 设计决策（全局）

### D-1 平台中立与 Polymarket 特有字段分离【新增设计】
- 每张业务表带 `platform TEXT NOT NULL DEFAULT 'polymarket'`。
- 通用字段（方向/价格/数量/状态/时间）为列；Polymarket 特有标识（`condition_id`、`yes/no_token_id`、`tick_size`、`neg_risk`、Gamma 原始字段）收敛到 `markets.platform_meta JSONB` + 独立 `market_tokens` 表。
- `token_id` 在 Polymarket 是 ERC1155 token id；抽象为「可交易 instrument id」，未来平台填自己的 instrument id。

### D-2 精度与时间戳【新增设计】
| 类型 | 表示 | 依据 |
|------|------|------|
| 金额（USDC） | `NUMERIC(20,6)` | USDC 6 位小数；现有 f64 金额（fee/slippage/pnl/notional） |
| 价格 / 概率 | `NUMERIC(10,6)` | Polymarket 价格∈[0,1]，tick 0.001~0.01；概率 0~1 |
| 数量（shares） | `NUMERIC(20,6)` | 现有 f64 size/qty |
| 时间戳 | `TIMESTAMPTZ`（UTC） | 现有代码用 unix 秒 u64（`fill_records.rs:36-41`）或 ISO 8601 字符串（proto），入库统一转换；**禁用 timestamp without tz** |
| 浮点入库存取 | f64 → NUMERIC 在写入层一次性转换 | G1-001 v7 §6.4 canonical 比较要求定点字符串，NUMERIC 天然满足 |

> 现有代码全链路 f64 计算**不改**（禁止事项 1/2）；NUMERIC 只是持久层表示，不反向改变交易语义。

### D-3 ID 体系（要求 1/2）【新增设计】
- **内部 ID**：各表 `BIGINT GENERATED ALWAYS AS IDENTITY` 主键（`order_id`、`fill_id` 等），仅库内关联用，**不暴露给交易链路**。
- **平台原生 ID**：`client_order_id`（= `AggregatedOrder.order_id`，Python 生成，`aggregator/models.py:80`）、`exchange_order_id`（CLOB 返回，`OrderResult.exchange_order_id`）、`trade_id`（CLOB 成交 id，P0-015 幂等键，`fill_records.rs:77`）、`settlement_id`（【待验证】POC-3）。
- **关联链**：`signals.signal_id → signal_order_links → orders.client_order_id → fills.trade_id → settlements.settlement_id`。

### D-4 幂等约束（要求 3）— 与现有内存幂等一一对应
| 现有内存机制 | 位置 | 数据库对应 |
|------|------|-----------|
| `OrderStore.claim` 原子占位（P0-004） | `order_state.rs:253-267` | `UNIQUE(platform, client_order_id)` |
| `bind_remote_order` 一对一防护（B2） | `order_state.rs:479-501` | `UNIQUE(platform, exchange_order_id) WHERE exchange_order_id IS NOT NULL` |
| `FillLedger.claim_trade`（P0-015） | `fill_records.rs:334-340` | `UNIQUE(platform, trade_id) WHERE trade_id <> ''` |
| G1-001 I2：每 settlement 恰一笔 effect | v7 §5.6 | `accounting_effects.settlement_id PRIMARY KEY` |
| 注册表按 condition_id 去重 | `registry.py` | `UNIQUE(platform, market_ref)` |
| 预报重复拉取 | —（无，文件覆盖） | `UNIQUE(source, location, issue_time)` |

> 空 `trade_id` 不启用去重是现有 FAIL-CLOSED 行为（`fill_records.rs:335-337` + AGENTS.md「空 trade_id 三处拦截」）；DB 侧用部分唯一索引复刻该语义，空 trade_id 行允许存在但不构成幂等键【已确认】。

### D-5 写入路径（要求 9/10）【新增设计】
- **同步下单热路径零 DB I/O**：持久化走「进程内事件队列（tokio mpsc / Python asyncio.Queue）→ 专职 DB writer 协程」。
- **有界队列 + 溢出落盘**：队列满/DB 不可用时事件追加到本地 spill 文件（append-only JSONL），恢复后回放；**绝不允许 DB 故障阻塞或失败下单**（持久化 fail-open，风控仍 fail-closed）。
- **重试**：指数退避，单事件最大重试次数后进 `dead_letter`（落地为 `system_events` 告警 + spill 保留）。
- **重复事件**：消费端幂等（D-4 唯一约束 + `ON CONFLICT DO NOTHING/UPDATE`），重放安全。
- **v3 修订（底层盲区①：fsync 移出热路径）**：spill 的 `fsync`（落盘持久化）**绝不允许出现在 Rust 下单热路径**——下单线程只做非阻塞 `write`/`append`，`fsync` 由专职 writer 线程/协程在事件入队后异步执行，跨线程边界用有界 channel（`crossbeam-channel` / `tokio mpsc`）。「落盘即安全」的确认由 writer 侧完成，热路径**不等待 fsync**（与 DB-HA-1 Fail-Open 一致：持久化尽最大努力，交易连续性优先，缺口靠对账补齐）。
- **进程重启**：Rust 内存态仍按现状冷启动（DCR-001 方案 A）；DB 是**审计/分析副本**，恢复对账以远端 CLOB 为权威（现有 `StateSync`/`RecoveryPlan`），DB 数据经 `reconciliation_runs` 与远端比对，不回灌交易状态机。
- ⚠️ **与 DCR-001 的关系**：方案 A 冻结的是「B2 不新增持久化文件、不支持跨重启恢复」。本数据库是**新的独立持久层**，若未来要让 DB 参与重启恢复（方案 B），需另立 DCR 修订案 + Human 批准【待验证】。

> ⚠️ **v2 修订（DB-DESIGN-001）**：D-5 的「异步写入」**不等于**「可靠持久化」，存在四类真实缺口，已单独登记为 C-12 / V-6 / R-6，并在 §9.1 给出「事件 ID + spill 先行 + 恢复回放」的补强方案。核心结论：**在补齐这些缺口之前，DB 不得被宣传为「可靠的交易记录持久化」**，且「持久化故障期间是否继续实盘」必须由 Human 裁决（DB-HA-1）。

### D-5b 与 G1-001 v7 的兼容性【已确认 + 待验证】
- v7 的 D1「SQLite 单事务」语义（`settlement_pnl_risk_wiring.md §5.4-5.6`）在 PostgreSQL 下**等价成立**：`BEGIN → INSERT accounting_effects → UPDATE settlements → COMMIT`，COMMIT 即唯一 durable boundary，Case C 同样被原子性排除。
- HA-006（持久化选型，Human 倾向 SQLite，未冻结）【待验证】：本设计按 PostgreSQL 出 Schema，但 `settlements`/`accounting_effects` 的**列与状态机与 v7 §5.2 逐字段一致**，降级到 SQLite 仅需方言微调（IDENTITY→AUTOINCREMENT、TIMESTAMPTZ→TEXT）。
- `settlement_id` 在 POC-3 验证前，`settlements` 表仅作 Schema 预备，**禁止生产写入**（v7 §4.5 前置依赖链）【待验证】。

---

## 3. Schema 总览

| # | 表 | 角色 | 来源 |
|---|----|------|------|
| 1 | `markets` | 市场目录（通用字段 + platform_meta） | `MarketInfo`【已确认】 |
| 2 | `market_tokens` | 市场 ↔ 可交易 instrument（yes/no token） | `MarketInfo.yes/no_token_id`【已确认】+ 表拆分【新增设计】 |
| 3 | `forecast_runs` | 气象预报拉取批次 | `WeatherForecast`/`SourceMeta`【已确认】 |
| 4 | `signals` | 策略信号（含版本与决策上下文） | `TradingSignal`【已确认】 |
| 5 | `signal_order_links` | 信号 ↔ 订单多对多 | 【新增设计】（现有代码缺口，§1.2-4） |
| 6 | `orders` | 订单当前状态（8 态） | `OrderState`/`AggregatedOrder`【已确认】 |
| 7 | `order_events` | 订单状态变迁审计 | 【新增设计】（状态机迁移的审计落地） |
| 8 | `fills` | 成交明细（fill 级） | `FillRecord`/`TradeEvent`【已确认】 |
| 9 | `positions` | 持仓快照（本地 + 远端两套来源） | `PositionEntry`/`RemotePosition`【已确认】 |
| 10 | `settlements` | 结算记录（v7 状态机） | G1-001 v7 §5.2【已确认-设计】 |
| 11 | `accounting_effects` | 结算记账效果（I2 恰一笔） | G1-001 v7 §5.6 方案 A【已确认-设计】 |
| 12 | `pnl_records` | 盈亏流水（fill/订单/日快照） | `compute_pnl`/`FillStats`【已确认】+ 表设计【新增设计】 |
| 13 | `risk_events` | 风控事件（拦截/熔断/预言机偏差） | `RiskCheckResult`/`BreachLevel`/`OracleStatus`【已确认】 |
| 14 | `system_events` | 系统事件（对账/连接/守护/DB writer 自身） | `ReconciliationReport`/`ConnectionState`【已确认】 |
| 15 | `reconciliation_runs` | 对账批次头 | `ReconciliationReport`【已确认】 |
| 16 | `schema_migrations` | 迁移版本 | 【新增设计】 |

---

## 4. 逐表设计

> 列类型遵循 D-2。每列的来源追溯见姊妹篇 `trading_database_field_mapping.md`，此处只给关键字段与约束。

### 4.1 `markets`
| 列 | 类型 | 说明 |
|----|------|------|
| market_id | BIGINT IDENTITY PK | 内部 ID |
| platform | TEXT NOT NULL DEFAULT 'polymarket' | |
| market_ref | TEXT NOT NULL | 平台原生市场标识（Polymarket = `condition_id`） |
| question | TEXT NOT NULL DEFAULT '' | 市场问题 |
| event_type / location / category | TEXT | rain/temperature/snow/wind/humidity |
| start_time / close_time / settlement_time | TIMESTAMPTZ NULL | `end_date_utc`→close_time；settlement_time 为 TimeWindowGuard 注册源（P0-RISK-002 解药）【待验证：注册链路未实现】 |
| active / closed | BOOLEAN | |
| volume / liquidity | NUMERIC(20,6) | |
| platform_meta | JSONB NOT NULL DEFAULT '{}' | tick_size、neg_risk、Gamma 原始字段 |
| created_at / updated_at | TIMESTAMPTZ NOT NULL DEFAULT now() | 审计字段 |

约束：`UNIQUE(platform, market_ref)`；索引 `(event_type, location)`、`(close_time) WHERE NOT closed`。

### 4.2 `market_tokens`
| 列 | 类型 | 说明 |
|----|------|------|
| platform | TEXT NOT NULL | |
| token_id | TEXT NOT NULL | 可交易 instrument id |
| market_id | BIGINT NOT NULL FK → markets | |
| outcome | TEXT NOT NULL | yes / no / 其他（未来非标二元市场） |

约束：`PRIMARY KEY(platform, token_id)`；`UNIQUE(market_id, outcome)`。

### 4.3 `forecast_runs`
| 列 | 类型 | 说明 |
|----|------|------|
| forecast_run_id | BIGINT IDENTITY PK | |
| source | TEXT NOT NULL | noaa/caiyun/ecmwf/jma（`SourceMeta.source`） |
| location / lat / lon | TEXT / NUMERIC(9,5) | |
| issue_time | TIMESTAMPTZ NOT NULL | 预报发布时间（`SourceMeta.issue_time`） |
| fetch_time | TIMESTAMPTZ NOT NULL | 拉取时间（`SourceMeta.fetch_time`） |
| model_version | TEXT NOT NULL DEFAULT '' | 预报模型版本【新增设计：现有代码无此字段】 |
| product | TEXT NOT NULL DEFAULT '' | hourly / daily / minutely【新增设计：C-9】 |
| payload | JSONB NOT NULL | periods 全量（保留原始决策输入，要求 5） |
| source_file | TEXT NULL | 现文件落地路径（迁移期追溯） |
| ingest_status | TEXT NOT NULL DEFAULT 'ok' | ok / degraded / failed |
| created_at | TIMESTAMPTZ | |

约束：`UNIQUE(source, location, issue_time, model_version, product)`（重复拉取幂等）；索引 `(location, issue_time DESC)`。

> ⚠️ **v2 修订（DB-DESIGN-005，C-9）→ v3 定稿**：`UNIQUE(source, location, issue_time)` 会把「同一 source+location+issue_time、但**不同模型版本或不同产品**（hourly/daily/minutely）的预报」误判为重复，`ON CONFLICT DO NOTHING` 会**静默丢历史**。**v3 冻结**：复合唯一键 `(source, location, issue_time, model_version, product)`，其中 `model_version` / `product` 一律 `NOT NULL DEFAULT ''`（规避 PG `NULL` 唯一键陷阱——PG 中 `NULL` 与任何值不等，含 NULL 的列会让唯一约束形同虚设，见 §9.8 盲区③）。`ON CONFLICT` 目标列随之扩展为五列。

### 4.4 `signals`
| 列 | 类型 | 说明 |
|----|------|------|
| signal_pk | BIGINT IDENTITY PK | **内部代理主键**（DB-HA-3 冻结：不碰 Python 源码，`signal_id` 保留为业务键） |
| signal_id | TEXT NOT NULL UNIQUE | `TradingSignal.signal_id`（uuid hex12 = 48-bit，128-bit UUID 记技术债） |
| forecast_run_id | BIGINT NULL FK → forecast_runs | 【新增设计：现有代码无关联】 |
| platform | TEXT NOT NULL DEFAULT 'polymarket' | |
| market_ref | TEXT NULL | 关联 markets（经 condition_id） |
| token_id / yes_token_id / no_token_id | TEXT | |
| strategy | TEXT NOT NULL | 策略名 |
| strategy_version | TEXT NULL | 【新增设计：现有代码无版本；建议 = 策略模块 git sha / 配置版本】 |
| direction / event_type / location | TEXT | BUY_YES / BUY_NO |
| target_price / market_price | NUMERIC(10,6) | |
| quantity | NUMERIC(20,6) | shares |
| confidence / deviation / sigma_level | NUMERIC(10,6) | |
| generated_at / expires_at | TIMESTAMPTZ | |
| context | JSONB NOT NULL DEFAULT '{}' | 决策关键上下文：market snapshot、source_probs、source_weights、融合权重（要求 5）【新增设计：落库内容】 |
| metadata | JSONB | `TradingSignal.metadata` |
| created_at | TIMESTAMPTZ | |

索引：`(platform, token_id, generated_at DESC)`、`(strategy, generated_at DESC)`、`(expires_at) WHERE expires_at IS NOT NULL`。

> ⚠️ **v2 修订（DB-DESIGN-003，C-7）→ v3 定稿（DB-HA-3 冻结）**：`signal_id = uuid.uuid4().hex[:12]` 实为 **48-bit**（12 hex 字符），非完整 UUID（128-bit），海量信号下有非零碰撞概率。**裁决 = DB 加内部代理主键 `signal_pk`（BIGINT IDENTITY），`signal_id` 降级为 `TEXT NOT NULL UNIQUE` 业务键，不修改 Python 源码**（128-bit UUID 记技术债，见 §9.8）。⚠️ 碰撞语义：`signal_id` 仍是 `UNIQUE`，一旦 48-bit 碰撞，写入层应**显式 FAIL-CLOSED 报错**（禁止 `ON CONFLICT DO NOTHING` 静默吞信号），交由 Human 处置——此口径同样记入 §9.8。

### 4.5 `signal_order_links`【新增设计】
| 列 | 类型 | 说明 |
|----|------|------|
| signal_id | TEXT NOT NULL FK → signals | |
| order_id | BIGINT NOT NULL FK → orders | 内部 ID |
| role | TEXT NOT NULL DEFAULT 'contributing' | contributing / voided |

约束：`PRIMARY KEY(signal_id, order_id)`。
> ⚠️ 数据缺口：现有 `AggregatedOrder` 不带 signal_id 列表（§1.2-4），接入时需扩展（属未来集成，本轮不改代码）。

### 4.6 `orders`
| 列 | 类型 | 说明 |
|----|------|------|
| order_id | BIGINT IDENTITY PK | 内部 ID |
| platform | TEXT NOT NULL DEFAULT 'polymarket' | |
| account_id | TEXT NOT NULL DEFAULT 'default' | 账户维度（DB-HA-4 冻结）；单账户默认 `'default'` |
| client_order_id | TEXT NOT NULL | `OrderState.client_order_id` |
| exchange_order_id | TEXT NULL | CLOB 分配 |
| market_id | BIGINT NULL FK → markets | |
| token_id | TEXT NOT NULL | 执行 token（方向对应 outcome token） |
| side | TEXT NOT NULL | BUY / SELL（Rust 执行语义） |
| direction | TEXT NOT NULL | BUY_YES / BUY_NO（策略语义） |
| limit_price | NUMERIC(10,6) | `OrderState.limit_price` |
| target_price / market_price | NUMERIC(10,6) | `AggregatedOrder` 定价上下文 |
| requested_size / filled_size / remaining_size | NUMERIC(20,6) | shares |
| status | TEXT NOT NULL | received/submitted/live/partial_filled/filled/cancelled/rejected/expired（与 `OrderStateStatus` 逐值一致） |
| confidence | NUMERIC(10,6) | |
| contributing_strategies | JSONB | 策略名列表 |
| input_signal_count | INT | |
| block_reason / risk_checks | TEXT / JSONB | 风控拦截审计 |
| order_type / tick_size / neg_risk | TEXT / TEXT / BOOLEAN NULL | GTC…；Polymarket 特有但下单必需 |
| paper_trading | BOOLEAN NOT NULL DEFAULT false | |
| strategy_version / metadata / context | TEXT / JSONB / JSONB | |
| last_source_seq | BIGINT NOT NULL DEFAULT 0 | **事件源单调水位线**（盲区②；v3.1 改 `NOT NULL DEFAULT 0`，消除 NULL 覆盖漏洞） |
| placed_at / created_at / updated_at | TIMESTAMPTZ | placed_at = 下单时间（`FillRecord.placed_at` 口径） |

约束：
- `UNIQUE(platform, client_order_id)`（D-4；`client_order_id` 全局唯一，多账户下仍成立，无需含 `account_id`）
- `UNIQUE(platform, exchange_order_id) WHERE exchange_order_id IS NOT NULL`（D-4）
- `CHECK (filled_size >= 0 AND filled_size <= requested_size)`（复刻 `update_fill` clamp，`order_state.rs:116`）
- `CHECK (filled_size + remaining_size <= requested_size)`（**v3.1 数量守恒**：filled + remaining 不得超 requested）
- `CHECK (status IN (...8 态...))`
- 索引：`(platform, token_id, status)`、活动单部分索引 `(platform, token_id) WHERE status IN ('received','submitted','live','partial_filled')`、`(exchange_order_id)`。

> ⚠️ **v3 修订（底层盲区②：乱序 UPSERT 水位线）→ v3.1 加固**：事件可能乱序到达（网络重放/恢复回放），旧事件不得覆盖新状态。`last_source_seq` 为事件源单调序号，`NOT NULL DEFAULT 0`（初始无历史即 0，无 NULL 分支），写入层守卫**简化为纯比较**：
> ```sql
> INSERT ... ON CONFLICT (platform, client_order_id) DO UPDATE
>   SET ... WHERE orders.last_source_seq < EXCLUDED.last_source_seq;
> ```
> 水位线语义：仅当 incoming `>` 现值才应用；`<=` 现值的迟到/重复事件 no-op（含初始 0 放行首个 seq>0 事件）。`last_source_seq` 与审计事件 `event_id`（§4.7）同源，均来自事件源单调序列。

### 4.7 `order_events`【新增设计】
| 列 | 类型 | 说明 |
|----|------|------|
| order_event_id | BIGINT IDENTITY PK | |
| event_id | TEXT NOT NULL | **稳定事件 ID**（C-8 冻结）：事件源单调序列或 `(order_id, trigger, seq)` |
| order_id | BIGINT NOT NULL FK → orders | |
| from_status / to_status | TEXT | from 可 NULL（首事件） |
| trigger | TEXT NOT NULL | place / submit_ack / live / trade / cancel / reject / expire / recovery_bind / recovery_rebuild |
| actor | TEXT NOT NULL DEFAULT 'rust' | rust / python / recovery |
| payload | JSONB | 事件原始数据（如下单响应、TradeEvent 摘要） |
| occurred_at | TIMESTAMPTZ NOT NULL | 纯观测时间戳，**不参与幂等** |

幂等：`UNIQUE(order_id, event_id)`（同一事件重放不产生第二行）。
> 用途：状态机变迁审计（要求 4「订单状态变化」），不替代 orders 当前态。

> ⚠️ **v2 修订（DB-DESIGN-003，C-8）→ v3 定稿**：`occurred_at` **不得**作为去重依据——同一毫秒重放、时钟回拨、重试都可能使时间戳不唯一。**v3 冻结**：新增稳定 `event_id`，幂等键 `UNIQUE(order_id, event_id)`（事件源单调序列，与 `orders.last_source_seq` 同源），`occurred_at` 降级为纯观测时间戳。**订单状态更新（orders.status）与审计事件写入（order_events）必须同事务原子提交**，避免「状态已变、审计缺失」的断裂。

### 4.8 `fills`
| 列 | 类型 | 说明 |
|----|------|------|
| fill_id | BIGINT IDENTITY PK | |
| platform | TEXT NOT NULL DEFAULT 'polymarket' | |
| account_id | TEXT NOT NULL DEFAULT 'default' | 账户维度（DB-HA-4 冻结） |
| order_id | BIGINT NOT NULL FK → orders | 内部 ID |
| client_order_id / exchange_order_id | TEXT | 冗余便于检索 |
| trade_id | TEXT NOT NULL DEFAULT '' | fill 级幂等键（可空串，语义同 `claim_trade`） |
| token_id / side | TEXT | |
| limit_price / filled_price | NUMERIC(10,6) | |
| size | NUMERIC(20,6) | shares |
| fee / slippage / slippage_pct | NUMERIC(20,6) / NUMERIC(20,6) / NUMERIC(10,4) | 定义同 `compute_slippage`（|成交-限价|/限价，恒 ≥0） |
| status | TEXT NOT NULL | filled / settled / cancelled（`FillStatus`） |
| placed_at / filled_at / settled_at | TIMESTAMPTZ | settled_at NULL=未结算；**注意现有 `mark_settled` 用 now_secs()，v7 §9 要求改为 settlement_ts——【待验证】属 G1-001 整改范围，本轮不改** |
| trader_side / taker_order_id / maker_order_id / owner / trade_status / tx_hash | TEXT | `TradeEvent` 归属字段（P0-013） |
| created_at | TIMESTAMPTZ | |

约束：
- `UNIQUE(platform, account_id, trade_id) WHERE trade_id <> ''`（D-4，P0-015；**v3.1 加 account_id**：多账户共享同一撮合 `trade_id`（自成交/共同撮合）不互斥）
- `CHECK (size > 0)`（`apply_fill` 非法增量拦截口径）
- 索引：`(order_id)`、`(platform, token_id, filled_at DESC)`、`(settled_at) WHERE settled_at IS NULL AND status = 'filled'`（未结算扫描）。

### 4.9 `positions`
| 列 | 类型 | 说明 |
|----|------|------|
| position_id | BIGINT IDENTITY PK | |
| platform | TEXT NOT NULL DEFAULT 'polymarket' | |
| account_id | TEXT NOT NULL DEFAULT 'default' | 账户维度（DB-HA-4 冻结） |
| token_id | TEXT NOT NULL | |
| qty / notional / reserved | NUMERIC(20,6) | `PositionEntry` 三件套；qty 允许负值（proto `PositionEntry` 注释「负数=空头」；Rust 侧 clamp≥0——以实际写入方为准【待验证】） |
| avg_price / current_value / cash_pnl | NUMERIC | `RemotePosition` 字段 |
| source | TEXT NOT NULL | local_engine / remote_snapshot |
| last_source_seq | BIGINT NOT NULL DEFAULT 0 | **事件源单调水位线**（盲区②；v3.1 改 `NOT NULL DEFAULT 0`，同 `orders`） |
| snapshot_at | TIMESTAMPTZ NOT NULL | |
| updated_at | TIMESTAMPTZ | |

约束：`UNIQUE(platform, account_id, token_id, source)`。
> 定位：**投影表**，非权威。本地持仓权威 = RiskEngine 内存；远端权威 = CLOB。对账差异经 `reconciliation_runs` 呈现。
> ⚠️ UPSERT 同样带 `last_source_seq` 水位线守卫（旧快照不覆盖新快照），写法同 §4.6。

> ⚠️ **v2 修订（DB-DESIGN-005，C-11）→ v3 定稿（DB-HA-4 冻结）**：原 v2 判「单账户、不强行加 `account_id`」。**v3 裁决反转：核心交易表引入 `account_id`**（默认 `'default'`，单账户亦显式落库），`positions` 唯一键升级为 `UNIQUE(platform, account_id, token_id, source)`，未来多账户/多钱包不迁移。`market_tokens.PK(platform, token_id)` 与 `orders.UNIQUE(platform, client_order_id)` 因 token / client_order_id 本身全局唯一，多账户下仍成立，无需改（§9.8 记录此边界）。

### 4.10 `settlements`（对齐 G1-001 v7 §5.2，逐字段一致）
| 列 | 类型 | 说明 |
|----|------|------|
| settlement_id | TEXT NOT NULL（复合主键之一） | UMA 市场级事件 id；POC-3 未验证 → **禁止生产写入**【待验证】 |
| platform | TEXT NOT NULL DEFAULT 'polymarket' | |
| account_id | TEXT NOT NULL DEFAULT 'default' | 账户维度（DB-HA-4 冻结）；**v3.1 与 settlement_id 构成复合主键** |
| token_id | TEXT NOT NULL | |
| outcome | NUMERIC(6,4) | 0 / 0.5 / 1（`0.5` = UMA 争议「50/50」结算，盲区④） |
| settlement_ts | TIMESTAMPTZ NOT NULL | 权威结算时间（v7 §9：唯一来源 = settlement event） |
| pnl_delta | NUMERIC(20,6) NULL | 记账后回填 |
| status | TEXT NOT NULL | PENDING / ACCOUNTING / ACCOUNTED / RETRYABLE / FAILED（v7 §5.3） |
| accounting_effect_id | TEXT NULL | effect 已生效证明 |
| received_at / processed_at | TIMESTAMPTZ | 仅观测，不得决定归属日（v7 §9/§10） |
| uma_tx_hash / block_number / identifier / requester | TEXT / BIGINT / TEXT / TEXT | UMA 事件字段（`PriceSettledEvent`） |
| created_at / updated_at | TIMESTAMPTZ | |

主键：`PRIMARY KEY (account_id, settlement_id)`（**v3.1**：UMA 结算事件为市场级，但 `pnl_delta` 账户级——两账户持有同一市场头寸时须各自入账，故主键含 `account_id`）。
索引：`(status) WHERE status <> 'ACCOUNTED'`（恢复扫描）、`(platform, token_id)`。

> ⚠️ **v2 修订（DB-DESIGN-004，C-10）→ v3 定稿**：
> 1. **outcome 取值范围**：`CHECK (outcome >= 0 AND outcome <= 1)`；**v3 明确支持 `outcome = 0.5`**（UMA 争议结算的「50/50」裁决，非 0 非 1；negRisk 下每个 token 仍各自 0/0.5/1）。
> 2. **合法状态迁移**：冻结状态机（见 §9.4 迁移表），并加 `CHECK`：`pnl_delta IS NULL OR status = 'ACCOUNTED'`（未 ACCOUNTED 不得有 pnl_delta），`accounting_effect_id IS NULL OR status = 'ACCOUNTED'`。状态迁移本身由应用层 + 单事务 enforce，DB 侧用 CHECK 兜底非法终态组合。

### 4.11 `accounting_effects`（v7 §5.6 方案 A：effects 保存完整 identity）
| 列 | 类型 | 说明 |
|----|------|------|
| settlement_id | TEXT NOT NULL（复合主键之一） | 与 settlements 对齐 |
| account_id | TEXT NOT NULL DEFAULT 'default' | 账户维度（DB-HA-4 冻结） |
| token_id / outcome | TEXT / NUMERIC(6,4) | 完整 identity（canonical 比较用；outcome 支持 0/0.5/1） |
| pnl_delta | NUMERIC(20,6) NOT NULL | NUMERIC 定点 = v7 §6.4 canonical 字符串要求 |
| settlement_ts | TIMESTAMPTZ NOT NULL | 归属日分桶依据（v7 §10） |
| applied_at | TIMESTAMPTZ NOT NULL DEFAULT now() | |

主键/外键：`PRIMARY KEY (account_id, settlement_id)` + `FOREIGN KEY (account_id, settlement_id) REFERENCES settlements (account_id, settlement_id) ON DELETE RESTRICT`（**v3.1**：I2 由复合主键保证——每账户每 settlement 恰一笔 effect）。

> 不变量（入库即约束）：I1 `daily_pnl(day) = Σ pnl_delta GROUP BY (settlement_ts AT TIME ZONE 'UTC')::date` 可重建（**须显式 UTC**，`::date` 对 timestamptz 非 IMMUTABLE）；I2 由复合 PK 保证；I3 重放只读。

### 4.12 `pnl_records`
| 列 | 类型 | 说明 |
|----|------|------|
| pnl_record_id | BIGINT IDENTITY PK | |
| platform | TEXT NOT NULL DEFAULT 'polymarket' | |
| account_id | TEXT NOT NULL DEFAULT 'default' | 账户维度（DB-HA-4 冻结） |
| pnl_type | TEXT NOT NULL | realized_fill / realized_order / daily_snapshot / unrealized_snapshot |
| amount | NUMERIC(20,6) NOT NULL | USDC；**口径 = 现有 `compute_pnl`（net，含 -fee），冻结不改**（v7 §3.3） |
| currency | TEXT NOT NULL DEFAULT 'USDC' | |
| fill_id / order_id | BIGINT NULL FK | 归因 |
| settlement_id | TEXT NULL | **v3.1** 复合外键 `(account_id, settlement_id)` → settlements，`ON DELETE SET NULL (settlement_id)` |
| token_id | TEXT | |
| accounting_day | DATE NOT NULL | = settlement_ts 的 UTC 日（v7 §10 迟到结算归属） |
| recorded_at | TIMESTAMPTZ | |

幂等：`UNIQUE(platform, account_id, pnl_type, fill_id) WHERE fill_id IS NOT NULL`、`UNIQUE(platform, account_id, pnl_type, order_id) WHERE order_id IS NOT NULL AND pnl_type='realized_order'`、`UNIQUE(platform, account_id, pnl_type, accounting_day) WHERE pnl_type='daily_snapshot'`（含 `account_id`，多账户隔离，DB-HA-4）。

### 4.13 `risk_events`
| 列 | 类型 | 说明 |
|----|------|------|
| risk_event_id | BIGINT IDENTITY PK | |
| platform | TEXT NOT NULL DEFAULT 'polymarket' | |
| event_type | TEXT NOT NULL | rule_block / circuit_breaker / gate_state / oracle_deviation / manual_halt |
| rule | TEXT NULL | position_limit / settlement_window / daily_loss / global_exposure / concentration / gas_fee / oracle_deviation（`RiskCheckResult.rule`） |
| breach_level | TEXT NULL | normal / warning / restricted / halted（`BreachLevel`） |
| order_id / client_order_id / token_id | 关联 | 可空（全局事件无订单） |
| passed | BOOLEAN NULL | |
| reason | TEXT | |
| measured_value / threshold_value / daily_pnl | NUMERIC | 触发时上下文 |
| detail | JSONB | `OracleStatus` / risk_checks 全量 |
| occurred_at | TIMESTAMPTZ NOT NULL | |

索引：`(occurred_at)`、`(event_type, occurred_at)`、`(token_id)`。

### 4.14 `system_events` + 4.15 `reconciliation_runs`
`reconciliation_runs`（批次头）：run_id PK、started_at/finished_at、in_sync、positions_checked、orders_checked、critical_diffs、report JSONB（`ReconciliationReport` 全量）。
`system_events`：event_id PK、platform NULL、component（daemon/state_sync/market_events/uma_monitor/grpc/db_writer/migration）、event_type（startup/shutdown/reconnect/healthcheck/error/dead_letter/migration）、severity（info/warning/critical）、recon_run_id NULL FK、payload JSONB、occurred_at。
索引：`(component, event_type, occurred_at)`、`(severity) WHERE severity='critical'`。

### 4.16 `schema_migrations`
`version INT PRIMARY KEY`、`name TEXT NOT NULL`、`checksum TEXT NOT NULL`、`applied_at TIMESTAMPTZ DEFAULT now()`。

---

## 5. 迁移与回滚

- 目录 `db/migrations/`：`0001_init.up.sql`（全部建表/索引/约束）+ `0001_init.down.sql`（按依赖逆序 DROP）。
- 版本记录：migration 末尾 `INSERT INTO schema_migrations`（up）/ `DELETE`（down）。
- **不连接真实库**；验证仅在一次性嵌入式测试库（见 `db/tests/`）。
- 后续迁移按 `NNNN_name.up/down.sql` 递增，禁止修改已合入的旧 migration。

## 6. 已确认问题清单（发现，不在本轮修复）

| # | 问题 | 证据 | 归属 |
|---|------|------|------|
| C-1 | `mark_settled` 用 `now_secs()` 而非权威 settlement_ts | `fill_records.rs:151-153` vs v7 §9 | G1-001（P0-RISK-001 链） |
| C-2 | 结算时间无注册来源、未知即放行 | P0-RISK-002（未闭环） | Round 06 之后待排期 |
| C-3 | signal→order 关联字段缺失 | `aggregator/models.py:80-105` 无 signal_id 列表 | 未来集成（需改 proto/聚合器，本轮禁止） |
| C-4 | DCR-001 方案 A「不支持跨重启」与新持久层的定位冲突 | `DCR-001-B2-durable-persistence.md` | 若 DB 参与恢复需 DCR 修订 + Human 批准 |
| C-5 | HA-006 未冻结（SQLite vs PostgreSQL） | v7 §16 | Human 待决 |
| C-6 | 数量单位（shares vs USDC）语义未闭环 | P1-RISK-UNIT-001 | R6-G1 |
| C-7 | `signal_id` 仅 48-bit（`uuid4().hex[:12]`），作主键有碰撞风险，碰撞时静默丢信号 | `strategy_engine/models.py:137` | DB-DESIGN-003（§9.3） |
| C-8 | `order_events` 幂等键依赖 `occurred_at`（时间戳不可靠去重） | 设计 §4.7 v1 | DB-DESIGN-003（§9.3） |
| C-9 | `forecast_runs` 唯一键缺 `model_version`/`product` 维度，可能误删不同模型/产品的历史 | 设计 §4.3 v1 | DB-DESIGN-005（§9.5） |
| C-10 | `settlements` 缺 outcome 范围约束与合法状态迁移约束 | 设计 §4.10 v1 | DB-DESIGN-004（§9.4） |
| C-11 | Schema 隐含单账户假设，无 `account_id` 维度，多账户下唯一键冲突 | 设计 §4.9 v1 | DB-DESIGN-005（§9.5） |
| C-12 | 异步持久化内存队列 crash 丢失窗口未定义，spill 崩溃/磁盘满未处理 | 设计 D-5 v1 | DB-DESIGN-001（§9.1） |

## 7. 待验证事项

| # | 事项 | 解除条件 |
|---|------|---------|
| V-1 | `settlement_id` 派生（POC-3）+ UMA outcome 解码（POC-1/2） | HA-007 环境（RPC/fixture） |
| V-2 | positions.qty 是否允许负值（Rust clamp≥0 vs proto 注释） | 接入实现时以实际写入方为准并回写本文档 |
| V-3 | `strategy_version` / `model_version` 取值规范（git sha？配置版本？） | 伯森先生命名约定 |
| V-4 | Polymarket token_id 全局唯一性（跨市场不重复）——按 CLOB 文档成立，未独立验证 | 真实 Gamma/CLOB 数据验证 |
| V-5 | DB 参与重启恢复（方案 B 化） | DCR 修订 + Human 批准 |
| V-6 | spill 磁盘满 / 写失败 / 进程崩溃的端到端恢复测试（事件不丢、不重） | 实现 DbEventBus 后补（§9.1） |
| V-7 | 并发幂等测试：多连接同时 INSERT 同一 `trade_id` / `client_order_id` / `settlement_id`，断言恰一笔 | 独立 PG 测试（§9.2） |
| V-8 | PG ↔ SQLite 方言差异清单是否真的「仅微调」（trigger / 部分索引 / 生成列 / 隔离级别） | 若 HA-006 转 SQLite，逐项核验（§9.2） |

## 8. 风险清单

| # | 风险 | 缓解 |
|---|------|------|
| R-1 | f64 → NUMERIC 转换在边界值（NaN/Inf）报错 | 写入层沿用现有 `is_finite` 校验口径，非法值进 `system_events` 告警，不阻塞下单 |
| R-2 | 溢出 spill 文件无限增长（DB 长期不可用） | 有界 + 滚动切分 + 磁盘水位告警；交易链路不受影响（D-5） |
| R-3 | 双写不一致（内存权威 vs DB 副本） | DB 定位为审计副本；`reconciliation_runs` 定期比对远端权威 |
| R-4 | settlement 表被提前写入（POC 未闭环） | 设计文档 + 代码评审双重门禁；schema 本身无写入方 |
| R-5 | 嵌入式测试库与生产 PG 版本差异 | migration 仅用 PG14+ 稳定特性；CI 固定版本 |
| R-6 | fail-open 期间产生的审计数据缺口（DB 不可用但继续交易 → 缺口靠对账补，不能靠内存） | spill 先行 + 恢复后对账补齐 + 缺口告警（§9.1）；是否允许 fail-open 由 DB-HA-1 裁决 |

---

## 9. 第二轮审查整改记录（2026-10-15，DB-DESIGN-001 ~ 005）

> 逐项「原设计 / 风险 / 建议方案 / 仍需人工决定」。编号沿用 C-* / V-* / R-*，新增 DB-HA-*（待人工决定）。**本节为设计整改，不涉及任何生产代码 / 迁移 / 集成。**

### 9.1 DB-DESIGN-001：异步持久化可靠性（C-12 / V-6 / R-6）

- **原设计**：D-5 用「进程内事件队列 → 专职 writer → 溢出 spill 回放」，并笼统宣称「重放安全、不阻塞下单」。
- **风险**：
  1. **内存队列丢失窗口**：事件入队后、writer 落盘前若进程 crash，队列内未 drain 事件**永久丢失**（队列是内存态，无 WAL）。
  2. **spill 写失败 / 磁盘满**：spill 追加失败或磁盘满时无兜底，事件静默丢失。
  3. **恢复补写边界**：DB 恢复后「回放 spill」与「实时流」的先后顺序、去重、以及断库期间远端新发生的事件如何补齐，未定义。
  4. **跨重启幂等**：现有幂等只靠业务唯一键（trade_id 等）；`order_events`/`risk_events`/`system_events` 无天然幂等键，重放会重复（见 C-8）。
  5. **fail-open 边界**：「DB 故障仍继续实盘」意味着审计数据产生缺口，与「审计副本」定位冲突——这是**决策点，不能由实施者默认**。
- **建议方案（分层，不冻结选型）**：
  1. **事件级稳定 ID + 单调序号**：每个持久化事件带 `event_id`（`<source>:<monotonic_seq>` 或 UUIDv7）与 `source_seq`；跨重启幂等以 `event_id` 为准，业务唯一键仅作业务层去重。
  2. **WAL/spill 先行**：事件先 `append + fsync` 到本地 spill（顺序日志），**落盘成功后才 ack 给生产者**（「落盘即安全」，把丢失窗口从「入队到落库」缩到「入队到落盘」）；writer 从 spill 异步消费落库。
  3. **恢复协议**：启动先重放 spill（按 `source_seq` 顺序、`ON CONFLICT(event_id)` 幂等），再对接实时流；断库期间远端事件由既有 `StateSync`/`RecoveryPlan` 对账补齐，`reconciliation_runs` 记录缺口。
  4. **磁盘满 / spill 写失败**：FAIL-CLOSED 告警 + 停新事件入队（仅停持久化，不停交易——见 DB-HA-1），`system_events(severity=critical)` 记录。
- **仍需人工决定（DB-HA-1）**：**持久化故障期间是否允许继续实盘**？
  - 选项 A（推荐，符合「交易连续性优先」）：允许继续实盘，缺口靠对账补齐，`system_events` 标记审计缺口。
  - 选项 B（保守，符合 FAIL-CLOSED 哲学）：持久化不可用即停新单，直到恢复。
  - 两者都**不得**把「异步写入」当作「可靠持久化」对外宣称；无论选哪个，D-5 须补上本节方案后才可进入 I-2 实现。

### 9.2 DB-DESIGN-002：数据库测试环境（V-7 / V-8）

- **原设计**：D-5b 声称「降级 SQLite 仅需方言微调（IDENTITY→AUTOINCREMENT、TIMESTAMPTZ→TEXT）」，过于轻率。
- **风险**：SQLite 与 PG 在**部分唯一索引、`GENERATED ... AS IDENTITY`、并发写隔离、`ON CONFLICT` 语义、`AT TIME ZONE`、生成列**等存在实质差异；若最终选 SQLite，现有 PG 测试结果**不能**直接视为 SQLite 验收。
- **建议方案**：
  1. **当前验收 = 真实 PostgreSQL**：已完成的 31 例（`db/tests/`）跑在**真实 PG 17.11**（zonky 内嵌二进制），非 SQLite 模拟——满足「不得用 SQLite 代替 PG 验收」。
  2. **待补并发幂等测试（V-7）**：多连接并发 `INSERT` 同一 `trade_id` / `client_order_id` / `settlement_id`，断言唯一约束下**恰一笔成功**、其余 `ON CONFLICT` 不产生脏数据。
  3. **待补事务测试**：D1 单事务在并发下（两连接同时 ACCOUNTING 同一 settlement）的行为。
  4. **V-8**：若 HA-006 最终转 SQLite，须逐项核验 §9.2 差异清单并重写/重跑测试，**不得沿用 PG 结论**。
- **仍需人工决定（DB-HA-2）**：数据库选型冻结（= G1-001 的 HA-006，Human 倾向 SQLite 但未冻结）。在冻结前，**Migration 最终版不生成**（见 §9.7）。
- **环境状态声明**：本机无 Docker、无系统 PG；测试用一次性内嵌 PG。若要求 CI/正式 PG 实例验收，需 Human 提供环境（否则标记 BLOCKED，不降级为 SQLite 验证）。

### 9.3 DB-DESIGN-003：事件与信号 ID（C-7 / C-8）

- **原设计**：`signal_id` 直接用 `uuid4().hex[:12]`；`order_events` 用 `UNIQUE(order_id, to_status, trigger, occurred_at)` 去重；状态更新与审计事件未显式绑定。
- **风险**：见 C-7（signal_id 碰撞静默丢信号）、C-8（时间戳不可靠去重 + 状态/审计原子性缺失）。
- **建议方案**：
  1. **signal_id（C-7）**：Python 侧改用完整 `uuid4().hex`（32 hex，128-bit）；或 DB 加内部代理键 + `UNIQUE(signal_id)` 且写入层碰撞时**显式 FAIL-CLOSED 报错**（禁止 `ON CONFLICT DO NOTHING` 静默吞）。此改动属 Python 策略模型，**本轮不改**，仅登记。
  2. **order_events（C-8）**：新增 `event_id`（稳定），幂等键 `UNIQUE(order_id, event_id)`（推荐）或全局 `UNIQUE(event_id)`；`occurred_at` 降级为纯时间戳。
  3. **一致性**：`orders.status` 更新与 `order_events` 插入**同一事务**（原子）；或采用事件溯源——`orders` 为 `order_events` 的投影（重放最后状态），当前态与审计永不脱节。
- **仍需人工决定（DB-HA-3）**：signal_id 采用「改 Python 完整 uuid」还是「DB 代理键 + 冲突报错」？

### 9.4 DB-DESIGN-004：结算记账不变量（C-10）

- **原设计**：`settlements` 无 outcome 约束、无状态迁移约束；`accounting_effects`/`pnl_records` 约束已具备但迁移边界未明写。
- **风险**：非法 outcome（>1）、非法状态组合（如 PENDING 却有 pnl_delta）、重复结算的边界语义不完整。
- **建议方案**：
  1. **outcome 约束**：`CHECK (outcome >= 0 AND outcome <= 1)`。
  2. **状态迁移（冻结）**：
     ```
     PENDING → ACCOUNTING → ACCOUNTED        （正常）
     PENDING → RETRYABLE → ACCOUNTING → …    （可重试）
     ACCOUNTING → RETRYABLE                   （失败可重试）
     任意 → FAILED                             （人工处置终态）
     ACCOUNTED / FAILED 为终态，不可再迁移
     ```
  3. **记账一致性 CHECK**：`pnl_delta IS NULL OR status = 'ACCOUNTED'`、`accounting_effect_id IS NULL OR status = 'ACCOUNTED'`；`accounting_effects` 只能随 `settlements.status` 在同一事务写入（D1 单事务，§5.6 语义不变）。
  4. **重复结算**：同 `settlement_id` 重复事件 → `ON CONFLICT` no-op；canonical 不同 → reject（v7 §6.4 已冻结，不重复展开）。
  5. **UTC 归属**：`accounting_day` 一律 = `(settlement_ts AT TIME ZONE 'UTC')::date`，禁止应用层按本地时区计算。
- **状态保留声明（强制）**：UMA/settlement POC 仍 BLOCKED（HA-007）；**本节的设计完善 ≠ 结算功能完成**。`settlement_id` 未经 POC-3 前，`settlements`/`accounting_effects` 仍**禁止生产写入**。

### 9.5 DB-DESIGN-005：天气预测数据与多平台扩展（C-9 / C-11）

- **原设计**：`forecast_runs UNIQUE(source, location, issue_time)`；全 Schema 无账户维度。
- **风险**：
  1. `forecast_runs` 唯一键误删：同一 source+location+issue_time 但不同 `model_version`/产品（hourly/daily/minutely）被 `ON CONFLICT DO NOTHING` 静默丢弃（C-9）。
  2. 多账户下 `positions`/`orders` 唯一键冲突（C-11）。
- **建议方案**：
  1. **forecast_runs（C-9）**：**推荐**去掉强唯一键，改普通索引 `(source, location, issue_time, model_version, product)` + 应用层去重（保留全部历史，误删风险最低）；若仍要幂等去重，则唯一键扩展为 `(source, location, issue_time, model_version, product)`，并新增 `product TEXT` 列（hourly/daily/minutely）。
  2. **账户维度（C-11）**：本轮**明确单账户适用边界**，不强行加 `account_id`（YAGNI）。多账户需求出现时，再引入 `account_id` 并重设 `positions`/`orders`/`fills` 唯一键。`market_tokens.PK(platform, token_id)` 与 `orders.UNIQUE(platform, client_order_id)` 因 token / client_order_id 全局唯一，多账户下仍成立。
- **仍需人工决定（DB-HA-4）**：是否现在就为多账户预留 `account_id`（提前抽象 vs 用时再加）？

### 9.6 验证计划（本轮不执行，待独立审查后）

| 阶段 | 动作 | 输出 |
|------|------|------|
| 文档审查 | 独立审查者核对 §9 整改是否覆盖 DB-DESIGN-001~005 | 审查意见 |
| 源码验证 | 核对 C-7（`models.py:137`）、C-1（`fill_records.rs:151`）等 file:line | 证据复核 |
| SQL 验证 | 在真实 PG 上跑 §9.2 补强测试（并发幂等 V-7 / 事务） | 测试报告 |
| 实际集成 | **仅 I-0 迁移执行器**（后续单独授权） | — |

### 9.7 交付边界（本轮）

- 本轮**仅提交**：本设计文档 v2（+ 顶部变更摘要）。
- **不生成最终 Migration**、**不改 `db/migrations/`、`db/tests/`**——待独立审查通过 + HA-006/DB-HA-1/DB-HA-3/DB-HA-4 裁决后，再据 §9 修订生成 `0002_*.sql`（增量）或重写 `0001`。
- **不修改**任何 Rust/Python 交易代码。

### 9.8 v3 定稿裁决记录（2026-10-15，伯森先生 v3 定稿指令，Gemini 修订）

> 本节为 §9.1~9.5 的**最终裁决**，覆盖其「仍需人工决定」项。裁决后，前文相应「选项 A/B」表述以本节为准。

**一、冻结裁决（DB-HA-1 ~ 4）**

| 编号 | 裁决 | 依据/落点 |
|------|------|-----------|
| DB-HA-1 | **Fail-Open**（交易连续性优先） | 持久化故障期间允许继续实盘，缺口靠对账补齐 + `system_events(severity=critical)` 标记审计缺口（§9.1 选项 A）；「异步写入 ≠ 可靠持久化」承诺不变 |
| DB-HA-2 | **PostgreSQL 唯一验收** | 放弃 SQLite 验收路径；唯一验收 = 真实 PostgreSQL（本轮 PG 17.11）；§9.2 / V-8 的 SQLite 降级方案作废 |
| DB-HA-3 | `signals` 加**内部代理主键 `signal_pk`** | **不修改 Python 源码**（128-bit UUID 记技术债）；`signal_id` 降级为 `TEXT NOT NULL UNIQUE` 业务键；48-bit 碰撞时写入层**显式 FAIL-CLOSED 报错**（禁止 `ON CONFLICT DO NOTHING` 静默吞） |
| DB-HA-4 | 核心交易表引入 **`account_id`** | `orders`/`fills`/`positions`/`settlements`/`accounting_effects`/`pnl_records` 加 `account_id TEXT NOT NULL DEFAULT 'default'`；`positions`/`pnl_records` 唯一键含 `account_id`；`market_tokens.PK` 与 `orders.UNIQUE(platform, client_order_id)` 因 token / client_order_id 全局唯一不改 |

**二、边界冲突修正（不碰 Python 源码）**

- DB-HA-3 采用「DB 内部代理键」方案，**放弃**「改 Python 完整 uuid」选项，全程零 Rust/Python 源码改动。
- `strategy_version` / `model_version` 取值规范（V-3）仍待伯森先生命名约定，但 DB 侧已用 `NOT NULL DEFAULT ''` 兜底，不阻塞建表。

**三、4 项底层工程盲区修正**

| # | 盲区 | 修正 |
|---|------|------|
| ① | spill `fsync` 阻塞下单热路径 | `fsync` 移出热路径，由专职 writer 线程异步执行（§2 D-5、§9.1） |
| ② | 乱序事件覆盖新状态 | `orders`/`positions` 加 `last_source_seq` 水位线 + `ON CONFLICT ... WHERE last_source_seq IS NULL OR < EXCLUDED` 守卫（§4.6/§4.9） |
| ③ | PG `NULL` 唯一键陷阱 | `forecast_runs.model_version`/`product` 一律 `NOT NULL DEFAULT ''`，复合唯一键 `(source, location, issue_time, model_version, product)` 切实生效（§4.3） |
| ④ | UMA 争议结算 outcome | `settlements`/`accounting_effects` 的 `outcome` 支持 `0 / 0.5 / 1`（`0.5` = 50/50 争议），CHECK 保持 `0 <= outcome <= 1`（§4.10/§4.11） |

**四、本轮交付物**

1. 最终版 `db/migrations/0001_init.up.sql` / `down.sql`（重写，纳入上述全部裁决）。
2. 补齐测试：V-7 多连接并发幂等、乱序 UPSERT 水位线防御、D1 结算单事务并发、outcome=0.5 边界（真实 PG 17.11）。
3. 测试验证报告（`db/tests/README.md` 更新）。

**五、状态保留（不变）**

- `settlement_id` 未经 POC-3 验证前，`settlements`/`accounting_effects` **禁止生产写入**（HA-007 BLOCKED）。
- 本设计仍**未接入任何生产代码**；`db/migrations/`、`db/tests/` 仅用于独立测试库。

### 9.9 v3.1 收尾补丁记录（2026-10-15，伯森先生 v3.1 终审意见）

> 逐行核对 DDL 与测试源码后，发现 2 处多账户隔离唯一键漏洞 + 3 处 SQL 边界隐患 + 3 处测试缺陷，全部收尾修复。**仍零 Rust/Python 源码改动。**

**一、多账户隔离唯一键修复（2 处）**

| # | 问题 | 修复 |
|---|------|------|
| 1 | `fills.uq_fills_platform_trade` 仅 `(platform, trade_id)`，两账户自成交/共享 `trade_id` 时第二笔被拦截 | 改为 `UNIQUE (platform, account_id, trade_id) WHERE trade_id <> ''` |
| 2 | `settlements`/`accounting_effects` 以单列 `settlement_id` 为主键，UMA 事件市场级但 `pnl_delta` 账户级，两账户无法各自结算 | `settlements` 主键改复合 `(account_id, settlement_id)`；`accounting_effects` 复合主键 + 复合外键；`pnl_records` 改复合外键 `(account_id, settlement_id)` |

**二、SQL 边界加固（3 处）**

| # | 问题 | 修复 |
|---|------|------|
| 1 | `last_source_seq` 可 `NULL`，`WHERE ... IS NULL OR ...` 使初始态可被任意乱序旧事件覆盖 | `orders`/`positions` 改 `BIGINT NOT NULL DEFAULT 0`，守卫简化为 `WHERE <table>.last_source_seq < EXCLUDED.last_source_seq` |
| 2 | `signals.fk_signals_market` 复合外键 `ON DELETE SET NULL` 会连带置空 `NOT NULL` 的 `platform` 而报错 | 用 PG15 列级语法 `ON DELETE SET NULL (market_ref)` |
| 3 | `orders` 缺数量守恒 | 补 `CHECK (filled_size + remaining_size <= requested_size)` |

> ⚠️ 因引入 PG15 列级 `SET NULL (col)` 语法，Migration 头注释由「PostgreSQL 14+」升为「**PostgreSQL 15+**」（实际验收仍为 PG 17.11）。

**三、测试补强（4 处）**

1. `_concurrent` 原先吞异常不校验 → 全部并发用例补 `assert not errors`。
2. D1 并发由「autocommit 单表插入」重写为**显式单事务双表**（`INSERT accounting_effects` + `UPDATE settlements` + `COMMIT`），断言恰一笔 effect 且 `status='ACCOUNTED'`。
3. 新增 `TestV31MultiAccount`：`wallet_a`/`wallet_b` 可同时写入同 `trade_id`（fills）、同 `settlement_id`（settlements & accounting_effects）、同 `token_id`（positions）互不冲突。
4. `seed_signal` 移除 `ON CONFLICT DO NOTHING`（与 DB-HA-3 禁止静默吞冲突），新增 `test_signal_id_collision_fail_closed` 断言重复 `signal_id` 抛 `UniqueViolation`。

**四、状态保留（不变）**

- `settlement_id` 未经 POC-3 验证前，`settlements`/`accounting_effects` **禁止生产写入**（HA-007 BLOCKED）。
- 仍**未接入任何生产代码**。

---

> **下一步**：字段映射（`trading_database_field_mapping.md`）→ Migration（`db/migrations/`）→ 独立测试（`db/tests/`）→ 接入计划（`trading_database_integration_plan.md`）。**集成实施需另行授权。**
