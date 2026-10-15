# 交易数据库 v3.1 收尾定稿 — 测试验证报告

- **日期**：2026-10-15
- **执行者**：旺财（WorkBuddy）
- **对象**：`db/migrations/0001_init.up.sql` / `down.sql`（v3 重写 + v3.1 收尾补丁）
- **设计依据**：`trading_database_design.md`（Draft v3.1）+ `trading_database_field_mapping.md`（Draft v3.1）
- **基线**：commit `6722efd`

## 1. 环境

| 项 | 值 |
|----|----|
| 数据库 | **真实 PostgreSQL 17.11**（zonky 内嵌二进制，Maven Central） |
| 验收依据 | DB-HA-2：PostgreSQL 唯一验收（放弃 SQLite） |
| Migration 目标 | **PG 15+**（v3.1 引入列级 `SET NULL (col)` 语法） |
| Python | 3.13.14 / pytest 9.1.1 |
| 连接 | 一次性临时实例，**不连接真实交易数据库** |
| 生产代码 | **零改动**（未触 Rust/Python 交易核心） |

## 2. 结果

```
collected 47 items
db\tests\test_trading_schema.py ............................................... [100%]
=============================== 47 passed in 49.30s ================================
```

**47/47 通过**（v2 31 例 → v3 42 例 → v3.1 47 例）。

## 3. 裁决与盲区 → 测试覆盖映射（v3 部分）

| 裁决/盲区 | 落点 | 测试 |
|-----------|------|------|
| DB-HA-3 `signal_pk` 代理键 | `signals.signal_pk` PK + `signal_id` UNIQUE | `TestRelations` + `TestV31MultiAccount::test_signal_id_collision_fail_closed` |
| DB-HA-4 `account_id` | 6 张核心表加 `account_id DEFAULT 'default'` | `TestV3Watermark`、`TestPnlPositionsEvents`、`TestV31MultiAccount` |
| 盲区① fsync 移出热路径 | 设计 D-5 文本（无 DB 结构变更） | 无 SQL 用例（纯实现约定，见设计 §2 D-5） |
| 盲区② 乱序水位线 | `orders`/`positions.last_source_seq` | `TestV3Watermark`（3 例） |
| 盲区③ PG NULL 唯一键陷阱 | `forecast_runs.model_version/product NOT NULL DEFAULT ''` | `TestV3NullTrap`（2 例） |
| 盲区④ outcome 争议结算 | `settlements`/`accounting_effects` outcome 支持 0/0.5/1 | `TestV3Outcome`（2 例） |
| V-7 并发幂等 | 多连接并发 INSERT 同一键恰一笔 | `TestV3Concurrency`（3 例） |
| D1 单事务并发 | 并发 ACCOUNTING 同一 settlement | `TestV3Concurrency::test_d1_concurrent_accounting_effect_exactly_once` |

## 4. v3.1 补丁 → 测试覆盖映射

| 补丁 | 落点 | 测试 |
|------|------|------|
| `fills` 唯一键加 `account_id` | `UNIQUE (platform, account_id, trade_id) WHERE trade_id <> ''` | `TestV31MultiAccount::test_multi_account_same_trade_id` |
| `settlements`/`accounting_effects` 复合主键 | `PRIMARY KEY (account_id, settlement_id)` | `TestV31MultiAccount::test_multi_account_same_settlement_id` |
| `pnl_records` 复合外键 | `FOREIGN KEY (account_id, settlement_id) ... ON DELETE SET NULL (settlement_id)` | `TestRelations::test_full_chain`（回归通过） |
| `last_source_seq NOT NULL DEFAULT 0` | orders/positions 列 + 简化守卫 | `TestV3Watermark`（守卫去掉 IS NULL 分支后仍 3 例通过） |
| `fk_signals_market SET NULL (market_ref)` | PG15 列级语法 | `TestMigration`（up/down 干净，无报错） |
| `orders` 数量守恒 | `CHECK (filled_size + remaining_size <= requested_size)` | `TestOrderLifecycle`（回归通过） |
| `_concurrent` 补断言 | `assert not errors` | `TestV3Concurrency`（4 例） |
| D1 双表事务重写 | `INSERT effect + UPDATE settlement + COMMIT` | `TestV3Concurrency::test_d1_concurrent_accounting_effect_exactly_once` |
| signal_id FAIL-CLOSED | 移除 `seed_signal` 的 `ON CONFLICT DO NOTHING` | `TestV31MultiAccount::test_signal_id_collision_fail_closed` |

## 5. 关键验证结论

1. **并发幂等（V-7）**：8 线程并发 `INSERT ... ON CONFLICT DO NOTHING` 同一键，最终均**恰一笔**，`assert not errors` 确认无死锁、无 SQL 错误。
2. **D1 双表事务并发**：两连接各自单事务 `INSERT accounting_effects + UPDATE settlements + COMMIT`，复合主键 + `ON CONFLICT` 保证 **I2 不变量**，最终 effect 恰一笔且 `status='ACCOUNTED'`。
3. **多账户隔离（v3.1 核心）**：`wallet_a`/`wallet_b` 可同时写入同 `trade_id`（fills）、同 `settlement_id`（settlements & accounting_effects）、同 `token_id`（positions）互不冲突——修复了原单列唯一键的多账户互斥漏洞。
4. **乱序水位线（盲区② + v3.1）**：`NOT NULL DEFAULT 0` 后守卫简化为纯 `WHERE last_source_seq < EXCLUDED.last_source_seq`，旧序号 no-op、新序号覆盖、初始 0 放行——三态验证。
5. **NULL 陷阱（盲区③）**：`NOT NULL DEFAULT ''` 复合唯一键下，不同 product 的预报不再被静默去重丢失。
6. **outcome 争议（盲区④）**：`outcome = 0.5` 可入库；`outcome = 1.5` 被 CHECK 拒绝。
7. **signal_id FAIL-CLOSED（DB-HA-3）**：重复 `signal_id` 显式抛 `UniqueViolation`，不再静默吞。

## 6. 边界与状态（不变）

- `settlement_id` 未经 POC-3 验证前，`settlements`/`accounting_effects` **禁止生产写入**（HA-007 BLOCKED）。
- 本 migration 与全部测试**仅用于独立测试库**；接入生产需另行授权。
- `db/migrations/` 中 `CHECKSUMS.txt` 尚未生成（占位），待集成授权时补齐。

## 7. 交付物清单

| 文件 | 状态 |
|------|------|
| `docs/design/trading_database_design.md` | v3.1 收尾定稿 |
| `docs/design/trading_database_field_mapping.md` | v3.1 收尾定稿 |
| `db/migrations/0001_init.up.sql` | v3.1 收尾补丁 |
| `db/migrations/0001_init.down.sql` | 无需改（表清单未变） |
| `db/tests/test_trading_schema.py` | 47 例 |
| `db/tests/README.md` | 覆盖表 + 运行结果更新 |
