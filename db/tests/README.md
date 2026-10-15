# db/tests — 交易数据库独立测试

## 运行方式（项目根目录）

```powershell
C:\Users\32389\.workbuddy\binaries\python\envs\default\Scripts\python.exe -m pytest db/tests -v
```

- 测试库：**一次性嵌入式 PostgreSQL 17.11**（zonky binaries，Maven Central），
  全部数据在系统临时目录，**绝不连接真实交易数据库**。
- 首次运行自动下载 PG 二进制（~23MB jar，缓存于 `%TEMP%\trading_db_test_pg\`，复用）。
- 依赖（已装入项目 venv）：`psycopg2-binary`、`testing.postgresql`；
  这两个依赖**尚未**写入 `requirements-dev.txt`——待集成授权时一并处理
  （见 integration_plan.md §7）。
- Windows 提示：teardown 时 `testing.common.database` 可能打印
  `failed to shutdown the server automatically` 告警，fixture 已用
  `taskkill` 兜底清理（本机无其他 PostgreSQL 进程），不影响结果。

## 覆盖范围（47 例）

| 组 | 数量 | 内容 |
|----|------|------|
| TestMigration | 4 | 16 张表齐全 / 版本登记 / 全部时间列为 TIMESTAMPTZ / down 回滚干净 |
| TestIdempotency | 6 | order claim 唯一 / exchange_order_id 部分唯一 / trade_id 幂等 / 空 trade_id 放行 / forecast 重复拉取 / order_events 重放（event_id） |
| TestRelations | 4 | signal↔order 链接 / signal↔market FK / fill→order FK / signal→order→fill→settlement→effect→pnl 全链 |
| TestOrderLifecycle | 4 | 部分成交→全成 / 撤单 / filled≤requested CHECK / status 枚举 CHECK |
| TestSettlementAccounting | 5 | I2 恰一笔 effect / D1 crash-before-COMMIT 回滚+重试 / 冲突拒绝不改写 / I1 按日可重建 / 迟到结算归属 settlement_ts 日 |
| TestPnlPositionsEvents | 5 | fill 级 PnL 幂等 / 日快照唯一 / 本地+远端持仓共存 / 熔断事件 / 对账 critical 告警 |
| TestRecovery | 3 | 未结算 fill 扫描 / 非 ACCOUNTED settlement 恢复集 / 活动订单集 |
| TestV3Concurrency | 4 | **V-7** 并发幂等：trade_id / client_order_id / settlement_id 恰一笔 + **D1** 双表事务并发 ACCOUNTING 恰一笔 effect |
| TestV3Watermark | 3 | **盲区②** 乱序 UPSERT 水位线：orders 旧 seq 不覆盖 / 新 seq 覆盖 / positions 守卫 |
| TestV3Outcome | 2 | **盲区④** outcome=0.5 争议结算可写 / outcome>1 被 CHECK 拒绝 |
| TestV3NullTrap | 2 | **盲区③** forecast 复合唯一键区分 product / model_version NOT NULL DEFAULT '' |
| TestV31MultiAccount | 5 | **v3.1** 多账户隔离：同 trade_id 各账户入账 / 同 settlement_id 各自结算 / 同 token_id 各自持仓 / signal 代理键 / signal_id 碰撞 FAIL-CLOSED |

## 最新运行结果（2026-10-15，v3.1 收尾补丁）

```
platform win32 -- Python 3.13.14, pytest-9.1.1
collected 47 items
db\tests\test_trading_schema.py ............................................... [100%]
=============================== 47 passed in 49.30s ================================
```

> PG 版本：真实 PostgreSQL 17.11（zonky 内嵌二进制，非 SQLite 模拟）——满足 DB-HA-2「PostgreSQL 唯一验收」。
> v3 相对 v2（31 例）新增 11 例：V-7 并发幂等（3）、D1 并发（1）、乱序水位线（3）、outcome=0.5（2）、NULL 陷阱（2）。
> v3.1 相对 v3（42 例）新增 5 例：多账户跨钱包隔离（4）+ signal_id FAIL-CLOSED 碰撞（1）；并重写 D1 为双表事务、`_concurrent` 补断言。

详细日志（本机）：`%TEMP%\dbtest_run7.log`
