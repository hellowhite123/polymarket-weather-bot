# 交易数据库接入计划 — Rust / Python（本轮仅计划，不实施）

- **状态**：Draft v1 — 供审查；**集成实施需伯森先生另行授权**
- **作者**：旺财（WorkBuddy）
- **日期**：2026-10-14
- **上游文档**：`trading_database_design.md`（Schema）、`trading_database_field_mapping.md`（映射）
- **红线**：接入不得改动 RiskConfig 阈值 / 交易语义 / gRPC 合约 / 下单关键路径；DB 写入一律异步。

---

## 1. 总原则

1. **DB 是审计/分析副本，不是交易状态权威**。权威排序：远端 CLOB（对账）> Rust 内存（运行时）> DB（回放/分析）。
2. **同步下单热路径零 DB I/O**（设计要求 9）。事件先入进程内有界队列，专职 writer 协程落库。
3. **持久化 fail-open，风控 fail-closed**。DB 故障只降级（spill + 告警），绝不阻塞或失败下单。
4. **消费端幂等**：所有写入用 `INSERT ... ON CONFLICT` 或唯一约束兜底，重放安全（设计要求 10）。

## 2. 事件模型（新增设计，不改 gRPC 合约）

```
┌─ Rust ────────────────────────────────────────────────┐
│ executor / order_state / market_events / risk_engine  │
│        │  (1) 发事件（非阻塞 try_send）                 │
│        ▼                                              │
│ DbEventBus (tokio mpsc, cap=4096, 溢出→spill JSONL)   │
│        │  (2) 批量 drain                               │
│        ▼                                              │
│ DbWriter task (tokio spawn) ──sqlx──→ PostgreSQL      │
│        │  失败：指数退避重试 N 次 → dead letter        │
│        ▼                                              │
│ spill/*.jsonl（DB 恢复后回放，回放走同一幂等写入）      │
└───────────────────────────────────────────────────────┘
┌─ Python ──────────────────────────────────────────────┐
│ pipeline / aggregator / market_discovery              │
│        │  asyncio.Queue → DbWriter 协程（asyncpg）     │
│        ▼                                              │
│ 写入：forecast_runs / signals / signal_order_links    │
└───────────────────────────────────────────────────────┘
```

| 事件 | 生产者 | 落表 |
|------|--------|------|
| MarketDiscovered / MarketUpdated | Python market_discovery | markets / market_tokens |
| ForecastIngested | Python weather_data_ingestion | forecast_runs |
| SignalGenerated | Python pipeline | signals |
| OrderPlaced / StatusChanged | Rust executor / OrderStore 钩子 | orders / order_events |
| FillRecorded | Rust `on_market_trade` / `apply_recovery` | fills |
| PositionSnapshot | Rust `post_check` / `sync_position` | positions |
| SettlementReceived / Accounted | Rust settlement 链路（**G1-001 闭环后才有生产者**） | settlements / accounting_effects |
| PnlRecorded | Rust settle 路径 | pnl_records |
| RiskBlocked / CircuitBreaker | Rust RiskEngine / Python risk_gate 镜像 | risk_events |
| ReconcileReport | Rust StateSync | reconciliation_runs / system_events |

## 3. 分阶段实施路线

| 阶段 | 内容 | 前置条件 | 验收 |
|------|------|---------|------|
| **I-0 迁移执行器** | 选定 runner（建议 Rust `sqlx migrate`；或独立 Python alembic 仅管 DDL），接入 `schema_migrations` 校验 | 无 | 空库→0001→down→再 up 幂等 |
| **I-1 只写影子库（Python 侧）** | weather_data_ingestion / market_discovery / pipeline 增加**可选** DB sink（开关默认 off） | 伯森先生授权 + DSN 环境变量 | 文件产物与 DB 内容一致（抽样比对） |
| **I-2 Rust 事件总线** | DbEventBus + DbWriter + spill；挂 `OrderStore`/`FillLedger` 只读钩子（**不改业务逻辑**，仅追加事件） | I-1 + 授权 | 压测：下单 P99 延迟增量 < 1ms（对比 `latency_bench`） |
| **I-3 风控/持仓事件** | risk_events / positions 快照（低频，秒级） | I-2 | 与 RiskState 抽样一致 |
| **I-4 结算链路** | settlements / accounting_effects / pnl_records | **G1-001（P0-RISK-001）+ POC-1/2/3 闭环后** | v7 §14 测试在 PG 上复跑 |
| **I-5 对账与回放** | reconciliation_runs 比对 DB vs 远端；spill 回放工具 | I-2 | 断库 10 分钟恢复后零丢失、零重复 |

> ⚠️ I-4 被 v7 §4.5 前置依赖链阻塞（POC BLOCKED，HA-007），本计划不提前实施。

## 4. 关键场景处理（设计要求 10）

### 4.1 持久化失败
- writer 捕获错误 → 退避重试（0.5s→2s→8s→30s，max 5 次）→ 进 `dead_letter`：事件转存 spill + `system_events(severity=critical)`。
- **不反压热路径**：队列满 → 直接写 spill（`try_send` 失败即落盘），不阻塞 `execute_fast`。

### 4.2 重复事件
- 所有写入路径幂等（D-4 唯一约束 + `ON CONFLICT`）。回放 spill 与实时流并发也安全（同一约束）。

### 4.3 进程重启
- Rust 内存态按现状冷启动（DCR-001 方案 A 不变）；远端对账恢复（现有 `StateSync`/`RecoveryPlan`）。
- DB writer 启动先回放 spill（按时间序），再接管实时事件。
- **DB 不回灌交易状态机**；若未来要（方案 B），先提 DCR 修订（对应设计文档 C-4）。

### 4.4 状态对账
- 周期任务：拉 CLOB open orders / positions / trades → 与 DB `orders/fills/positions` 比对 → 差异写 `reconciliation_runs.report` + `system_events`（critical 差异告警）。
- 与现有 `ReconciliationReport` 复用同一 JSON 结构。

## 5. 技术选型（建议，待批准）

| 项 | 建议 | 备选 | 说明 |
|----|------|------|------|
| Rust DB 驱动 | `sqlx`（编译期 SQL 校验可关闭） | tokio-postgres | sqlx 生态成熟，支持 NUMERIC/TIMESTAMPTZ |
| Python DB 驱动 | `asyncpg` | psycopg3 | 与 asyncio pipeline 天然契合 |
| 迁移执行器 | `sqlx migrate`（Rust 侧权威） | alembic | 选其一，禁止双写 schema_migrations |
| 队列 | tokio mpsc / asyncio.Queue | 外部 MQ（过度设计，暂不需要） | 进程内即可 |
| 连接串 | `TRADING_DATABASE_URL` 环境变量 | — | 不入库、不入 git（安全规则沿用） |

## 6. 验收标准（接入时）

1. 下单热路径 P99 延迟增量 < 1ms（对拍 `latency_benchmark_report.md` 基线）。
2. 停库 10 分钟：下单不失败、不阻塞；恢复后事件零丢失零重复。
3. 全量重放 spill：与实时写入结果完全一致（幂等）。
4. `pytest db/tests/` + `cargo test` 全绿（不借接入之名改既有测试语义）。
5. 每类事件有端到端样例记录可展示。

## 7. 本轮明确不做

- 不改 `Cargo.toml` / `requirements*.txt` 增加 DB 依赖。
- 不改任何 Rust/Python 业务文件。
- 不创建真实数据库、不执行 migration 于任何持久库。
- 不实现 Web Dashboard（后续项目，复用本 Schema 只读视图）。
