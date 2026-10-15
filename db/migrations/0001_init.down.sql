-- ============================================================================
-- 0001_init.down.sql — 回滚 0001_init（按依赖逆序 DROP）
-- ⚠️ 仅限独立测试库使用；禁止在真实交易数据库执行。
-- ============================================================================

BEGIN;

DELETE FROM schema_migrations WHERE version = 1;

DROP TABLE IF EXISTS system_events;
DROP TABLE IF EXISTS reconciliation_runs;
DROP TABLE IF EXISTS risk_events;
DROP TABLE IF EXISTS pnl_records;
DROP TABLE IF EXISTS accounting_effects;
DROP TABLE IF EXISTS settlements;
DROP TABLE IF EXISTS positions;
DROP TABLE IF EXISTS fills;
DROP TABLE IF EXISTS order_events;
DROP TABLE IF EXISTS signal_order_links;
DROP TABLE IF EXISTS orders;
DROP TABLE IF EXISTS signals;
DROP TABLE IF EXISTS forecast_runs;
DROP TABLE IF EXISTS market_tokens;
DROP TABLE IF EXISTS markets;
DROP TABLE IF EXISTS schema_migrations;

COMMIT;
