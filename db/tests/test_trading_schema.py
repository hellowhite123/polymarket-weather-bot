"""交易数据库 Schema 独立测试（0001_init migration）。

运行方式（项目根目录）：
    python -m pytest db/tests/test_trading_schema.py -v

测试库：一次性嵌入式 PostgreSQL（zonky binaries → testing.postgresql），
全部数据落在系统临时目录，**绝不连接真实交易数据库**。
首次运行会从 Maven Central 下载 PG 17.11.0 二进制（约 100MB，缓存复用）。
"""

import io
import os
import subprocess
import tarfile
import tempfile
import threading
import urllib.request
import zipfile
from decimal import Decimal
from pathlib import Path

import psycopg2
import pytest
import testing.postgresql

# ---------------------------------------------------------------------------
# 路径与常量
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parents[2]
MIGRATIONS_DIR = PROJECT_ROOT / "db" / "migrations"
UP_SQL = MIGRATIONS_DIR / "0001_init.up.sql"
DOWN_SQL = MIGRATIONS_DIR / "0001_init.down.sql"

PG_VERSION = "17.11.0"
PG_JAR_URL = (
    "https://repo1.maven.org/maven2/io/zonky/test/postgres/"
    "embedded-postgres-binaries-windows-amd64/{v}/"
    "embedded-postgres-binaries-windows-amd64-{v}.jar"
).format(v=PG_VERSION)

CACHE_DIR = Path(tempfile.gettempdir()) / "trading_db_test_pg" / PG_VERSION
# zonky txz 解压根目录即 bin/ lib/ share/（无 pgsql/ 包装层）
PG_BIN = CACHE_DIR / "bin"

ALL_TABLES = [
    "schema_migrations", "markets", "market_tokens", "forecast_runs",
    "signals", "orders", "signal_order_links", "order_events", "fills",
    "positions", "settlements", "accounting_effects", "pnl_records",
    "risk_events", "reconciliation_runs", "system_events",
]

TS = "2026-10-14T12:00:00+00:00"


# ---------------------------------------------------------------------------
# PG 二进制准备（下载 + 解压 + 缓存）
# ---------------------------------------------------------------------------

def _ensure_pg_binaries() -> Path:
    initdb = PG_BIN / ("initdb.exe" if os.name == "nt" else "initdb")
    if initdb.exists():
        return PG_BIN

    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    jar_path = CACHE_DIR / "pg.jar"
    if not jar_path.exists():
        with urllib.request.urlopen(PG_JAR_URL, timeout=600) as resp:
            jar_path.write_bytes(resp.read())

    with zipfile.ZipFile(jar_path) as zf:
        txz_names = [n for n in zf.namelist() if n.endswith(".txz")]
        assert txz_names, f"jar 中未找到 .txz: {zf.namelist()[:5]}"
        txz_bytes = zf.read(txz_names[0])

    with tarfile.open(fileobj=io.BytesIO(txz_bytes), mode="r:xz") as tf:
        try:
            tf.extractall(CACHE_DIR, filter="data")
        except TypeError:  # Python < 3.12 无 filter 参数
            tf.extractall(CACHE_DIR)

    assert initdb.exists(), f"解压后未找到 initdb: {initdb}"
    return PG_BIN


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture(scope="session")
def pg_base():
    """整个会话共用一个嵌入式 PG 实例（避免每用例 initdb 的分钟级开销）。"""
    pg_bin = _ensure_pg_binaries()
    os.environ["PATH"] = str(pg_bin) + os.pathsep + os.environ.get("PATH", "")
    # Windows 下 initdb 需要显式 UTF-8/locale 以避免继承失败
    os.environ.setdefault("PGCLIENTENCODING", "UTF8")

    pg = testing.postgresql.Postgresql()
    yield pg
    try:
        pg.stop()
    except Exception:
        # testing.postgresql 在 Windows 上 stop 可能失效；兜底清理（本机无其他 PG）
        subprocess.run(["taskkill", "/F", "/IM", "postgres.exe"],
                       capture_output=True)


@pytest.fixture()
def db(pg_base):
    """每个用例：重置 public schema → 应用 0001_init.up（秒级）。"""
    conn = psycopg2.connect(pg_base.url())
    conn.autocommit = True
    with conn.cursor() as cur:
        cur.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
        cur.execute(UP_SQL.read_text(encoding="utf-8"))
    yield conn
    conn.close()


def exec_sql(conn, sql, params=None):
    with conn.cursor() as cur:
        cur.execute(sql, params)
        if cur.description:
            return cur.fetchall()
    return None


def scalar(conn, sql, params=None):
    rows = exec_sql(conn, sql, params)
    return rows[0][0] if rows else None


# ---------------------------------------------------------------------------
# 种子数据助手
# ---------------------------------------------------------------------------

def seed_market(conn, ref="cond-1", token_yes="tok-yes-1", token_no="tok-no-1"):
    exec_sql(conn, """
        INSERT INTO markets (market_ref, question, event_type, location,
                             close_time, settlement_time)
        VALUES (%s, 'Will it rain in NYC on 2026-10-15?', 'rain', 'New York',
                %s, %s)
        ON CONFLICT (platform, market_ref) DO NOTHING
    """, (ref, TS, TS))
    mid = scalar(conn,
                 "SELECT market_id FROM markets WHERE market_ref = %s", (ref,))
    exec_sql(conn, """
        INSERT INTO market_tokens (platform, token_id, market_id, outcome)
        VALUES ('polymarket', %s, %s, 'yes'),
               ('polymarket', %s, %s, 'no')
        ON CONFLICT (platform, token_id) DO NOTHING
    """, (token_yes, mid, token_no, mid))
    return mid


def seed_forecast(conn, source="noaa", location="New York",
                  issue=TS, run_payload='{"periods": []}',
                  model_version="", product=""):
    exec_sql(conn, """
        INSERT INTO forecast_runs (source, location, issue_time, fetch_time,
                                   model_version, product, payload)
        VALUES (%s, %s, %s, %s, %s, %s, %s::jsonb)
        ON CONFLICT (source, location, issue_time, model_version, product)
        DO NOTHING
    """, (source, location, issue, TS, model_version, product, run_payload))
    return scalar(conn, """
        SELECT forecast_run_id FROM forecast_runs
        WHERE source = %s AND location = %s AND issue_time = %s
    """, (source, location, issue))


def seed_signal(conn, sid="sig-1", market_ref=None, token="tok-yes-1"):
    # DB-HA-3：不静默吞数据——重复 signal_id 由 UNIQUE 约束显式抛错（FAIL-CLOSED）
    exec_sql(conn, """
        INSERT INTO signals (signal_id, market_ref, token_id, direction,
                             strategy, target_price, market_price, quantity,
                             confidence, generated_at, context)
        VALUES (%s, %s, %s, 'BUY_YES', 'forecast_deviation',
                0.62, 0.55, 40, 0.7, %s,
                '{"source_probs": {"noaa": 0.62}}'::jsonb)
    """, (sid, market_ref, token, TS))
    return sid


def seed_order(conn, cid="ord-1", exchange=None, token="tok-yes-1",
               status="received"):
    exec_sql(conn, """
        INSERT INTO orders (client_order_id, exchange_order_id, token_id,
                            side, direction, limit_price, requested_size,
                            remaining_size, status, placed_at)
        VALUES (%s, %s, %s, 'BUY', 'BUY_YES', 0.55, 100, 100, %s, %s)
        ON CONFLICT (platform, client_order_id) DO NOTHING
    """, (cid, exchange, token, status, TS))
    return scalar(conn, """
        SELECT order_id FROM orders WHERE client_order_id = %s
    """, (cid,))


def seed_fill(conn, order_id, trade_id="trade-1", size=37.0, price=0.56):
    exec_sql(conn, """
        INSERT INTO fills (order_id, client_order_id, trade_id, token_id,
                           side, limit_price, filled_price, size,
                           placed_at, filled_at, trader_side)
        VALUES (%s, 'ord-1', %s, 'tok-yes-1', 'BUY', 0.55, %s, %s,
                %s, %s, 'TAKER')
    """, (order_id, trade_id, price, size, TS, TS))
    return scalar(conn,
                  "SELECT fill_id FROM fills WHERE trade_id = %s", (trade_id,))


# ===========================================================================
# 1. 迁移与结构
# ===========================================================================

class TestMigration:
    def test_up_creates_all_16_tables(self, db):
        for table in ALL_TABLES:
            n = scalar(db, """
                SELECT count(*) FROM information_schema.tables
                WHERE table_schema = 'public' AND table_name = %s
            """, (table,))
            assert n == 1, f"缺表: {table}"

    def test_schema_migrations_records_version_1(self, db):
        row = exec_sql(db,
                       "SELECT version, name FROM schema_migrations")
        assert row == [(1, "init")]

    def test_timestamptz_columns_have_timezone(self, db):
        """所有 *_at / *_time 列必须是 timestamptz（D-2）。"""
        rows = exec_sql(db, """
            SELECT table_name, column_name
            FROM information_schema.columns
            WHERE table_schema = 'public'
              AND (column_name LIKE '%\\_at' OR column_name LIKE '%\\_time')
              AND data_type <> 'timestamp with time zone'
        """)
        assert rows == [], f"存在非 TIMESTAMPTZ 时间列: {rows}"

    def test_down_migration_rolls_back(self, pg_base):
        conn = psycopg2.connect(pg_base.url())
        conn.autocommit = True
        try:
            with conn.cursor() as cur:
                cur.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
                cur.execute(UP_SQL.read_text(encoding="utf-8"))
                cur.execute(DOWN_SQL.read_text(encoding="utf-8"))
            n = scalar(conn, """
                SELECT count(*) FROM information_schema.tables
                WHERE table_schema = 'public'
            """)
            assert n == 0, f"down 后仍有残留表: {n}"
        finally:
            conn.close()


# ===========================================================================
# 2. 幂等约束（D-4：与内存幂等机制一一对应）
# ===========================================================================

class TestIdempotency:
    def test_order_claim_unique(self, db):
        """OrderStore.claim 对应：同 platform+client_order_id 只能占位一次。"""
        seed_order(db, cid="ord-dup")
        with pytest.raises(psycopg2.errors.UniqueViolation):
            exec_sql(db, """
                INSERT INTO orders (client_order_id, token_id, side,
                                    direction, requested_size,
                                    remaining_size, status)
                VALUES ('ord-dup', 'tok-yes-1', 'BUY', 'BUY_YES',
                        10, 10, 'received')
            """)
        db.rollback()
        n = scalar(db, "SELECT count(*) FROM orders WHERE client_order_id='ord-dup'")
        assert n == 1

    def test_exchange_order_id_partial_unique(self, db):
        """bind_remote_order 一对一防护：exchange_order_id 唯一；NULL 不受限。"""
        seed_order(db, cid="o1", exchange="ex-1")
        seed_order(db, cid="o2", exchange=None)
        seed_order(db, cid="o3", exchange=None)  # 多个 NULL 允许
        with pytest.raises(psycopg2.errors.UniqueViolation):
            seed_order(db, cid="o4", exchange="ex-1")
        db.rollback()

    def test_fill_claim_trade_unique(self, db):
        """FillLedger.claim_trade 对应：同 trade_id 只入账一次。"""
        oid = seed_order(db)
        seed_fill(db, oid, trade_id="t-1")
        with pytest.raises(psycopg2.errors.UniqueViolation):
            seed_fill(db, oid, trade_id="t-1")
        db.rollback()
        assert scalar(db, "SELECT count(*) FROM fills WHERE trade_id='t-1'") == 1

    def test_fill_empty_trade_id_not_deduped(self, db):
        """空 trade_id 不参与幂等（FAIL-CLOSED 口径：宁可拒记不可错记，
        由应用层三处拦截；DB 层允许存在但不构成幂等键）。"""
        oid = seed_order(db)
        seed_fill(db, oid, trade_id="", size=1.0)
        seed_fill(db, oid, trade_id="", size=2.0)
        assert scalar(db, "SELECT count(*) FROM fills WHERE trade_id=''") == 2

    def test_forecast_run_reingest_idempotent(self, db):
        seed_forecast(db)
        seed_forecast(db)  # 同 source+location+issue_time
        assert scalar(db, "SELECT count(*) FROM forecast_runs") == 1

    def test_order_event_replay_no_duplicate(self, db):
        """同一事件（event_id）重放不产生第二行（C-8：稳定事件 ID 幂等）。"""
        oid = seed_order(db)
        for _ in range(2):
            exec_sql(db, """
                INSERT INTO order_events (event_id, order_id, from_status,
                                          to_status, trigger, occurred_at)
                VALUES (%s, %s, NULL, 'received', 'place', %s)
                ON CONFLICT (order_id, event_id)
                DO NOTHING
            """, (f"ev-{oid}-1", oid, TS))
        assert scalar(db, "SELECT count(*) FROM order_events") == 1


# ===========================================================================
# 3. 关联（signal → order → fill → settlement）
# ===========================================================================

class TestRelations:
    def test_signal_to_order_link(self, db):
        seed_market(db)
        seed_signal(db, market_ref="cond-1")
        oid = seed_order(db)
        exec_sql(db, """
            INSERT INTO signal_order_links (signal_id, order_id, role)
            VALUES ('sig-1', %s, 'contributing')
        """, (oid,))
        n = scalar(db, """
            SELECT count(*) FROM signal_order_links sol
            JOIN signals s USING (signal_id)
            JOIN orders o ON o.order_id = sol.order_id
            WHERE s.signal_id = 'sig-1' AND o.client_order_id = 'ord-1'
        """)
        assert n == 1

    def test_signal_market_fk(self, db):
        seed_market(db)
        seed_signal(db, market_ref="cond-1")
        q = scalar(db, """
            SELECT m.question FROM signals s
            JOIN markets m ON m.platform = s.platform
                          AND m.market_ref = s.market_ref
            WHERE s.signal_id = 'sig-1'
        """)
        assert "rain" in q

    def test_fill_requires_existing_order(self, db):
        with pytest.raises(psycopg2.errors.ForeignKeyViolation):
            seed_fill(db, order_id=999999, trade_id="t-x")
        db.rollback()

    def test_full_chain_signal_order_fill_settlement_pnl(self, db):
        """完整链路：signal → order → fill → settlement → effect → pnl。"""
        seed_market(db)
        seed_signal(db, market_ref="cond-1")
        oid = seed_order(db, status="filled")
        fid = seed_fill(db, oid, trade_id="t-chain")
        exec_sql(db, """
            INSERT INTO signal_order_links (signal_id, order_id, role)
            VALUES ('sig-1', %s, 'contributing')
        """, (oid,))
        exec_sql(db, """
            INSERT INTO settlements (settlement_id, token_id, outcome,
                                     settlement_ts, status)
            VALUES ('stl-1', 'tok-yes-1', 1.0, %s, 'PENDING')
        """, (TS,))
        exec_sql(db, """
            INSERT INTO accounting_effects
                (settlement_id, token_id, outcome, pnl_delta, settlement_ts)
            VALUES ('stl-1', 'tok-yes-1', 1.0, 16.28, %s)
        """, (TS,))
        exec_sql(db, """
            INSERT INTO pnl_records (pnl_type, amount, fill_id, order_id,
                                     settlement_id, token_id, accounting_day)
            VALUES ('realized_fill', 16.28, %s, %s, 'stl-1',
                    'tok-yes-1', '2026-10-14')
        """, (fid, oid))
        row = exec_sql(db, """
            SELECT s.signal_id, o.client_order_id, f.trade_id,
                   ae.settlement_id, p.amount
            FROM signals s
            JOIN signal_order_links sol USING (signal_id)
            JOIN orders o ON o.order_id = sol.order_id
            JOIN fills f ON f.order_id = o.order_id
            LEFT JOIN accounting_effects ae ON ae.token_id = f.token_id
            LEFT JOIN pnl_records p ON p.fill_id = f.fill_id
            WHERE s.signal_id = 'sig-1'
        """)
        assert len(row) == 1
        got = row[0]
        assert (got[0], got[1], got[2], got[3], float(got[4])) == (
            "sig-1", "ord-1", "t-chain", "stl-1", 16.28)


# ===========================================================================
# 4. 部分成交 / 撤单 / 状态机
# ===========================================================================

class TestOrderLifecycle:
    def test_partial_fill_flow(self, db):
        """received → submitted → live → partial_filled → filled，事件完整。"""
        oid = seed_order(db)
        transitions = [
            ("received", "submitted", "submit_ack"),
            ("submitted", "live", "live"),
            ("live", "partial_filled", "trade"),
            ("partial_filled", "filled", "trade"),
        ]
        for i, (frm, to, trig) in enumerate(transitions):
            exec_sql(db, """
                INSERT INTO order_events
                    (event_id, order_id, from_status, to_status, trigger,
                     occurred_at)
                VALUES (%s, %s, %s, %s, %s,
                        %s::timestamptz + (%s || ' seconds')::interval)
            """, (f"ev-{oid}-{i + 1}", oid, frm, to, trig, TS, str(i + 1)))
            exec_sql(db, "UPDATE orders SET status = %s WHERE order_id = %s",
                     (to, oid))
        seed_fill(db, oid, trade_id="t-p1", size=37.0)
        seed_fill(db, oid, trade_id="t-p2", size=63.0)
        exec_sql(db, """
            UPDATE orders SET filled_size = 100, remaining_size = 0
            WHERE order_id = %s
        """, (oid,))
        st = exec_sql(db, """
            SELECT status, filled_size, remaining_size FROM orders
            WHERE order_id = %s
        """, (oid,))
        assert st == [("filled", 100, 0)]
        assert scalar(db, "SELECT count(*) FROM order_events WHERE order_id=%s",
                      (oid,)) == 4
        assert scalar(db, "SELECT coalesce(sum(size),0) FROM fills WHERE order_id=%s",
                      (oid,)) == 100

    def test_cancel_flow(self, db):
        oid = seed_order(db, status="live")
        exec_sql(db, """
            UPDATE orders SET status='cancelled' WHERE order_id=%s
        """, (oid,))
        exec_sql(db, """
            INSERT INTO order_events (event_id, order_id, from_status,
                                      to_status, trigger, occurred_at)
            VALUES (%s, %s, 'live', 'cancelled', 'cancel', %s)
        """, (f"ev-{oid}-cancel", oid, TS))
        assert scalar(db, "SELECT status FROM orders WHERE order_id=%s",
                      (oid,)) == "cancelled"

    def test_filled_cannot_exceed_requested(self, db):
        """复刻 update_fill clamp：filled_size <= requested_size。"""
        oid = seed_order(db)
        with pytest.raises(psycopg2.errors.CheckViolation):
            exec_sql(db, """
                UPDATE orders SET filled_size = 101 WHERE order_id = %s
            """, (oid,))
        db.rollback()

    def test_status_check_constraint(self, db):
        with pytest.raises(psycopg2.errors.CheckViolation):
            exec_sql(db, """
                INSERT INTO orders (client_order_id, token_id, side,
                                    direction, requested_size,
                                    remaining_size, status)
                VALUES ('ord-bad', 't', 'BUY', 'BUY_YES', 1, 1, 'bogus')
            """)
        db.rollback()


# ===========================================================================
# 5. 结算 exactly-once（G1-001 v7 D1 单事务语义）
# ===========================================================================

class TestSettlementAccounting:
    def test_i2_effect_at_most_once(self, db):
        """I2 不变量：同一 settlement_id 恰一笔 effect（PK 保证）。"""
        exec_sql(db, """
            INSERT INTO settlements (settlement_id, token_id, outcome,
                                     settlement_ts, status)
            VALUES ('stl-i2', 'tok', 1.0, %s, 'PENDING')
        """, (TS,))
        exec_sql(db, """
            INSERT INTO accounting_effects
                (settlement_id, token_id, outcome, pnl_delta, settlement_ts)
            VALUES ('stl-i2', 'tok', 1.0, -10.0, %s)
        """, (TS,))
        with pytest.raises(psycopg2.errors.UniqueViolation):
            exec_sql(db, """
                INSERT INTO accounting_effects
                    (settlement_id, token_id, outcome, pnl_delta,
                     settlement_ts)
                VALUES ('stl-i2', 'tok', 1.0, -10.0, %s)
            """, (TS,))
        db.rollback()
        assert scalar(db, "SELECT count(*) FROM accounting_effects "
                          "WHERE settlement_id='stl-i2'") == 1

    def test_d1_transaction_atomicity_crash_before_commit(self, db):
        """v7 Test B：COMMIT 前 crash → rollback → effect 与 ACCOUNTED 均未提交 → 可安全重试。"""
        exec_sql(db, """
            INSERT INTO settlements (settlement_id, token_id, outcome,
                                     settlement_ts, status)
            VALUES ('stl-b', 'tok', 1.0, %s, 'PENDING')
        """, (TS,))
        conn2 = psycopg2.connect(db.dsn)
        conn2.autocommit = False
        try:
            with conn2.cursor() as cur:
                cur.execute("""
                    INSERT INTO accounting_effects
                        (settlement_id, token_id, outcome, pnl_delta,
                         settlement_ts)
                    VALUES ('stl-b', 'tok', 1.0, -10.0, %s)
                """, (TS,))
                cur.execute("""
                    UPDATE settlements
                    SET status='ACCOUNTED', pnl_delta=-10.0,
                        accounting_effect_id='eff-stl-b'
                    WHERE settlement_id='stl-b'
                """)
            conn2.rollback()  # 模拟 COMMIT 前 crash
        finally:
            conn2.close()
        assert scalar(db, "SELECT count(*) FROM accounting_effects "
                          "WHERE settlement_id='stl-b'") == 0
        assert scalar(db, "SELECT status FROM settlements "
                          "WHERE settlement_id='stl-b'") == "PENDING"

        # 重试 → 恰一笔
        exec_sql(db, """
            INSERT INTO accounting_effects
                (settlement_id, token_id, outcome, pnl_delta, settlement_ts)
            VALUES ('stl-b', 'tok', 1.0, -10.0, %s)
        """, (TS,))
        exec_sql(db, """
            UPDATE settlements SET status='ACCOUNTED', pnl_delta=-10.0,
                accounting_effect_id='eff-stl-b'
            WHERE settlement_id='stl-b'
        """)
        assert scalar(db, "SELECT count(*) FROM accounting_effects "
                          "WHERE settlement_id='stl-b'") == 1
        assert scalar(db, "SELECT status FROM settlements "
                          "WHERE settlement_id='stl-b'") == "ACCOUNTED"

    def test_d1_conflict_rejected_no_mutation(self, db):
        """v7 §6.4：同 settlement_id 不同 canonical payload → reject，不累加不覆盖。"""
        exec_sql(db, """
            INSERT INTO settlements (settlement_id, token_id, outcome,
                                     settlement_ts, status)
            VALUES ('stl-c', 'tok', 1.0, %s, 'PENDING')
        """, (TS,))
        exec_sql(db, """
            INSERT INTO accounting_effects
                (settlement_id, token_id, outcome, pnl_delta, settlement_ts)
            VALUES ('stl-c', 'tok', 1.0, -10.0, %s)
        """, (TS,))
        # 冲突请求：同 id，pnl_delta -20 → 事务内 canonical_compare → ROLLBACK
        conn2 = psycopg2.connect(db.dsn)
        conn2.autocommit = False
        rejected = False
        try:
            with conn2.cursor() as cur:
                cur.execute("""
                    SELECT pnl_delta, settlement_ts, token_id, outcome
                    FROM accounting_effects WHERE settlement_id='stl-c'
                """)
                existing = cur.fetchone()
                incoming = (-20.0, existing[1], existing[2], existing[3])
                if abs(float(existing[0]) - incoming[0]) > 0:
                    conn2.rollback()
                    rejected = True
                else:
                    conn2.commit()
        finally:
            conn2.close()
        assert rejected
        assert scalar(db, "SELECT pnl_delta FROM accounting_effects "
                          "WHERE settlement_id='stl-c'") == -10.0
        assert scalar(db, "SELECT count(*) FROM accounting_effects "
                          "WHERE settlement_id='stl-c'") == 1

    def test_i1_daily_pnl_rebuildable_from_effects(self, db):
        """I1：daily_pnl(day) = Σ committed effects（跨 UTC 日分桶，v7 §10）。"""
        for sid, delta, ts in [
            ("stl-d1a", "-10.0", "2026-10-13T23:59:59+00:00"),
            ("stl-d1b", "-5.0", "2026-10-13T12:00:00+00:00"),
            ("stl-d2a", "3.0", "2026-10-14T00:00:05+00:00"),
        ]:
            exec_sql(db, """
                INSERT INTO settlements (settlement_id, token_id, outcome,
                                         settlement_ts, status)
                VALUES (%s, 'tok', 1.0, %s, 'ACCOUNTED')
            """, (sid, ts))
            exec_sql(db, """
                INSERT INTO accounting_effects
                    (settlement_id, token_id, outcome, pnl_delta,
                     settlement_ts)
                VALUES (%s, 'tok', 1.0, %s, %s)
            """, (sid, delta, ts))
        rows = exec_sql(db, """
            SELECT (settlement_ts AT TIME ZONE 'UTC')::date AS day,
                   sum(pnl_delta)
            FROM accounting_effects
            GROUP BY day ORDER BY day
        """)
        import datetime as _dt
        assert rows == [
            (_dt.date(2026, 10, 13), -15.0),
            (_dt.date(2026, 10, 14), 3.0),
        ]

    def test_late_settlement_bucket_by_settlement_ts(self, db):
        """v7 §10 示例：settlement_ts=23:59:59, received=次日 00:00:05 → 归属前一日。"""
        exec_sql(db, """
            INSERT INTO settlements (settlement_id, token_id, outcome,
                                     settlement_ts, status, received_at)
            VALUES ('stl-late', 'tok', 1.0,
                    '2026-09-17T23:59:59+00:00', 'ACCOUNTED',
                    '2026-09-18T00:00:05+00:00')
        """)
        day = scalar(db, """
            SELECT (settlement_ts AT TIME ZONE 'UTC')::date FROM settlements
            WHERE settlement_id='stl-late'
        """)
        import datetime as _dt
        assert day == _dt.date(2026, 9, 17)


# ===========================================================================
# 6. PnL / 持仓 / 风控与系统事件
# ===========================================================================

class TestPnlPositionsEvents:
    def test_pnl_realized_fill_idempotent(self, db):
        oid = seed_order(db, status="filled")
        fid = seed_fill(db, oid, trade_id="t-pnl")
        for _ in range(2):
            exec_sql(db, """
                INSERT INTO pnl_records (pnl_type, amount, fill_id,
                                         token_id, accounting_day)
                VALUES ('realized_fill', 5.0, %s, 'tok-yes-1', '2026-10-14')
                ON CONFLICT (platform, account_id, pnl_type, fill_id)
                    WHERE fill_id IS NOT NULL
                DO NOTHING
            """, (fid,))
        assert scalar(db, "SELECT count(*) FROM pnl_records") == 1

    def test_pnl_daily_snapshot_unique_per_day(self, db):
        for _ in range(2):
            exec_sql(db, """
                INSERT INTO pnl_records (pnl_type, amount, token_id,
                                         accounting_day)
                VALUES ('daily_snapshot', -12.5, '', '2026-10-14')
                ON CONFLICT (platform, account_id, pnl_type, accounting_day)
                    WHERE pnl_type = 'daily_snapshot'
                DO NOTHING
            """)
        assert scalar(db, "SELECT count(*) FROM pnl_records") == 1

    def test_positions_local_and_remote_coexist(self, db):
        exec_sql(db, """
            INSERT INTO positions (token_id, qty, notional, reserved,
                                   source, snapshot_at)
            VALUES ('tok', 100, 55, 10, 'local_engine', %s)
        """, (TS,))
        exec_sql(db, """
            INSERT INTO positions (token_id, qty, avg_price, cash_pnl,
                                   source, snapshot_at)
            VALUES ('tok', 100, 0.55, 45, 'remote_snapshot', %s)
        """, (TS,))
        with pytest.raises(psycopg2.errors.UniqueViolation):
            exec_sql(db, """
                INSERT INTO positions (token_id, qty, source, snapshot_at)
                VALUES ('tok', 1, 'local_engine', %s)
            """, (TS,))
        db.rollback()
        assert scalar(db, "SELECT count(*) FROM positions WHERE token_id='tok'") == 2

    def test_risk_event_circuit_breaker(self, db):
        oid = seed_order(db)
        exec_sql(db, """
            INSERT INTO risk_events (event_type, rule, breach_level, order_id,
                                     token_id, passed, reason, measured_value,
                                     threshold_value, daily_pnl, occurred_at)
            VALUES ('circuit_breaker', 'daily_loss', 'halted', %s,
                    'tok-yes-1', false,
                    '日亏损触发 halted 级熔断', 500.0, 500.0, -500.0, %s)
        """, (oid, TS))
        row = exec_sql(db, """
            SELECT rule, breach_level, daily_pnl FROM risk_events
            WHERE event_type = 'circuit_breaker'
        """)
        assert row == [("daily_loss", "halted", -500.0)]

    def test_reconciliation_run_with_critical_diff_alert(self, db):
        rid = scalar(db, """
            INSERT INTO reconciliation_runs
                (started_at, in_sync, positions_checked, orders_checked,
                 critical_diffs, report)
            VALUES (%s, false, 3, 5, 1,
                    '{"diffs": [{"token_id": "tok", "field": "qty", "severity": "Critical"}]}'::jsonb)
            RETURNING recon_run_id
        """, (TS,))
        exec_sql(db, """
            INSERT INTO system_events (component, event_type, severity,
                                       recon_run_id, payload)
            VALUES ('state_sync', 'reconcile_report', 'critical', %s,
                    '{"reason": "position mismatch"}'::jsonb)
        """, (rid,))
        n = scalar(db, """
            SELECT count(*) FROM system_events se
            JOIN reconciliation_runs rr USING (recon_run_id)
            WHERE se.severity = 'critical' AND rr.critical_diffs = 1
        """)
        assert n == 1


# ===========================================================================
# 7. 恢复场景（进程重启语义）
# ===========================================================================

class TestRecovery:
    def test_unsettled_fills_discoverable_after_restart(self, db):
        """重启后扫描：未结算 fill（status=filled 且 settled_at IS NULL）。"""
        oid = seed_order(db, status="filled")
        seed_fill(db, oid, trade_id="t-u1")
        seed_fill(db, oid, trade_id="t-u2")
        # 其中一笔结算
        exec_sql(db, """
            UPDATE fills SET status='settled', settled_at=%s
            WHERE trade_id='t-u1'
        """, (TS,))
        rows = exec_sql(db, """
            SELECT trade_id FROM fills
            WHERE settled_at IS NULL AND status = 'filled'
        """)
        assert rows == [("t-u2",)]

    def test_pending_settlements_recoverable(self, db):
        """重启恢复扫描：status != ACCOUNTED 的 settlement（v7 §11）。"""
        for sid, st in [("s1", "PENDING"), ("s2", "ACCOUNTING"),
                        ("s3", "ACCOUNTED"), ("s4", "RETRYABLE")]:
            exec_sql(db, """
                INSERT INTO settlements (settlement_id, token_id, outcome,
                                         settlement_ts, status)
                VALUES (%s, 'tok', 1.0, %s, %s)
            """, (sid, TS, st))
        rows = exec_sql(db, """
            SELECT settlement_id FROM settlements
            WHERE status <> 'ACCOUNTED' ORDER BY settlement_id
        """)
        assert rows == [("s1",), ("s2",), ("s4",)]

    def test_active_orders_scan(self, db):
        """OrderStore.pending_order_ids 口径：非终态订单。"""
        for cid, st in [("a1", "live"), ("a2", "partial_filled"),
                        ("a3", "filled"), ("a4", "cancelled"),
                        ("a5", "received")]:
            seed_order(db, cid=cid, status=st)
        rows = exec_sql(db, """
            SELECT client_order_id FROM orders
            WHERE status IN ('received', 'submitted', 'live',
                             'partial_filled')
            ORDER BY client_order_id
        """)
        assert rows == [("a1",), ("a2",), ("a5",)]


# ===========================================================================
# 8. v3 定稿补强：并发幂等 / 乱序水位线 / D1 并发 / outcome=0.5 / NULL 陷阱
# ===========================================================================

def _concurrent(db, sql, params, n=8):
    """n 个线程各持独立连接，同时执行同一 INSERT（ON CONFLICT 兜底）。"""
    barrier = threading.Barrier(n)
    errors = []

    def worker():
        conn = psycopg2.connect(db.dsn)
        conn.autocommit = True
        try:
            barrier.wait()
            with conn.cursor() as cur:
                cur.execute(sql, params)
        except Exception as exc:  # 唯一约束兜底（极端情况下未走 ON CONFLICT）
            errors.append(exc)
        finally:
            conn.close()

    threads = [threading.Thread(target=worker) for _ in range(n)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return errors


class TestV3Concurrency:
    def test_v7_concurrent_trade_id_exactly_once(self, db):
        """V-7：多连接并发 INSERT 同一 trade_id（同账户）→ 恰一笔。"""
        oid = seed_order(db)
        sql = """
            INSERT INTO fills (order_id, client_order_id, trade_id, token_id,
                               side, filled_price, size, filled_at)
            VALUES (%s, 'ord-1', 't-conc', 'tok-yes-1', 'BUY', 0.56, 10, %s)
            ON CONFLICT (platform, account_id, trade_id) WHERE trade_id <> ''
            DO NOTHING
        """
        errors = _concurrent(db, sql, (oid, TS), n=8)
        assert not errors
        assert scalar(db, "SELECT count(*) FROM fills WHERE trade_id='t-conc'") == 1

    def test_v7_concurrent_client_order_id_exactly_once(self, db):
        """V-7：多连接并发 INSERT 同一 client_order_id → 恰一笔。"""
        sql = """
            INSERT INTO orders (client_order_id, token_id, side, direction,
                                requested_size, remaining_size, status)
            VALUES ('ord-conc', 'tok-yes-1', 'BUY', 'BUY_YES', 10, 10, 'received')
            ON CONFLICT (platform, client_order_id) DO NOTHING
        """
        errors = _concurrent(db, sql, None, n=8)
        assert not errors
        assert scalar(db, "SELECT count(*) FROM orders "
                          "WHERE client_order_id='ord-conc'") == 1

    def test_v7_concurrent_settlement_id_exactly_once(self, db):
        """V-7：多连接并发 INSERT 同一 (account_id, settlement_id) → 恰一笔。"""
        sql = """
            INSERT INTO settlements (settlement_id, token_id, outcome,
                                     settlement_ts, status)
            VALUES ('stl-conc', 'tok', 1.0, %s, 'PENDING')
            ON CONFLICT (account_id, settlement_id) DO NOTHING
        """
        errors = _concurrent(db, sql, (TS,), n=8)
        assert not errors
        assert scalar(db, "SELECT count(*) FROM settlements "
                          "WHERE settlement_id='stl-conc'") == 1

    def test_d1_concurrent_accounting_effect_exactly_once(self, db):
        """D1 并发：两连接各自单事务（INSERT effect + UPDATE settlement + COMMIT）→ I2 恰一笔 + ACCOUNTED。"""
        exec_sql(db, """
            INSERT INTO settlements (settlement_id, token_id, outcome,
                                     settlement_ts, status)
            VALUES ('stl-d1c', 'tok', 1.0, %s, 'PENDING')
        """, (TS,))
        barrier = threading.Barrier(2)
        errors = []

        def account():
            conn = psycopg2.connect(db.dsn)
            conn.autocommit = False
            try:
                barrier.wait()
                with conn.cursor() as cur:
                    cur.execute("""
                        INSERT INTO accounting_effects
                            (account_id, settlement_id, token_id, outcome,
                             pnl_delta, settlement_ts)
                        VALUES ('default', 'stl-d1c', 'tok', 1.0, -10.0, %s)
                        ON CONFLICT (account_id, settlement_id) DO NOTHING
                    """, (TS,))
                    cur.execute("""
                        UPDATE settlements
                        SET status='ACCOUNTED', pnl_delta=-10.0,
                            accounting_effect_id='eff-stl-d1c'
                        WHERE account_id='default' AND settlement_id='stl-d1c'
                    """)
                conn.commit()
            except Exception as exc:
                conn.rollback()
                errors.append(exc)
            finally:
                conn.close()

        threads = [threading.Thread(target=account) for _ in range(2)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert not errors
        assert scalar(db, "SELECT count(*) FROM accounting_effects "
                          "WHERE account_id='default' AND settlement_id='stl-d1c'") == 1
        assert scalar(db, "SELECT status FROM settlements "
                          "WHERE account_id='default' AND settlement_id='stl-d1c'") == "ACCOUNTED"


class TestV3Watermark:
    def test_out_of_order_upsert_guard_orders(self, db):
        """盲区②：orders 乱序事件（旧 last_source_seq）不覆盖新状态。"""
        oid = seed_order(db, cid="wm-1")
        exec_sql(db, """
            UPDATE orders SET status='filled', last_source_seq=10
            WHERE order_id=%s
        """, (oid,))
        # 迟到 seq=5 的旧事件 → 水位线守卫 no-op
        exec_sql(db, """
            INSERT INTO orders (client_order_id, token_id, side, direction,
                                requested_size, remaining_size, status,
                                last_source_seq)
            VALUES ('wm-1', 'tok-yes-1', 'BUY', 'BUY_YES', 100, 100,
                    'received', 5)
            ON CONFLICT (platform, client_order_id) DO UPDATE
                SET status = EXCLUDED.status,
                    last_source_seq = EXCLUDED.last_source_seq
                WHERE orders.last_source_seq < EXCLUDED.last_source_seq
        """)
        row = exec_sql(db, """
            SELECT status, last_source_seq FROM orders WHERE order_id=%s
        """, (oid,))
        assert row == [("filled", 10)]

    def test_out_of_order_upsert_guard_newer_applies(self, db):
        """盲区②：新 seq 正常覆盖；初始 DEFAULT 0 水位线放行首个事件。"""
        oid = seed_order(db, cid="wm-2")  # last_source_seq = 0（NOT NULL DEFAULT 0）
        exec_sql(db, """
            INSERT INTO orders (client_order_id, token_id, side, direction,
                                requested_size, remaining_size, status,
                                last_source_seq)
            VALUES ('wm-2', 'tok-yes-1', 'BUY', 'BUY_YES', 100, 100,
                    'live', 3)
            ON CONFLICT (platform, client_order_id) DO UPDATE
                SET status = EXCLUDED.status,
                    last_source_seq = EXCLUDED.last_source_seq
                WHERE orders.last_source_seq < EXCLUDED.last_source_seq
        """)
        # 再应用 seq=7 → 应覆盖
        exec_sql(db, """
            INSERT INTO orders (client_order_id, token_id, side, direction,
                                requested_size, remaining_size, status,
                                last_source_seq)
            VALUES ('wm-2', 'tok-yes-1', 'BUY', 'BUY_YES', 100, 100,
                    'filled', 7)
            ON CONFLICT (platform, client_order_id) DO UPDATE
                SET status = EXCLUDED.status,
                    last_source_seq = EXCLUDED.last_source_seq
                WHERE orders.last_source_seq < EXCLUDED.last_source_seq
        """)
        row = exec_sql(db, """
            SELECT status, last_source_seq FROM orders WHERE order_id=%s
        """, (oid,))
        assert row == [("filled", 7)]

    def test_out_of_order_upsert_guard_positions(self, db):
        """盲区②：positions 同样带水位线守卫。"""
        exec_sql(db, """
            INSERT INTO positions (token_id, qty, source, snapshot_at,
                                   last_source_seq)
            VALUES ('tok', 100, 'local_engine', %s, 5)
        """, (TS,))
        exec_sql(db, """
            INSERT INTO positions (token_id, qty, source, snapshot_at,
                                   last_source_seq)
            VALUES ('tok', 999, 'local_engine', %s, 3)
            ON CONFLICT (platform, account_id, token_id, source) DO UPDATE
                SET qty = EXCLUDED.qty,
                    last_source_seq = EXCLUDED.last_source_seq
                WHERE positions.last_source_seq < EXCLUDED.last_source_seq
        """, (TS,))
        row = exec_sql(db, """
            SELECT qty, last_source_seq FROM positions WHERE token_id='tok'
              AND source='local_engine'
        """)
        assert row == [(100, 5)]


class TestV31MultiAccount:
    def test_multi_account_same_trade_id(self, db):
        """v3.1：两账户共享同一 trade_id（自成交/共同撮合）可各自入账 fills。"""
        oid_a = seed_order(db, cid="oa")
        oid_b = seed_order(db, cid="ob")
        for acc, oid, cid in (("wallet_a", oid_a, "oa"),
                              ("wallet_b", oid_b, "ob")):
            exec_sql(db, """
                INSERT INTO fills (account_id, order_id, client_order_id,
                                   trade_id, token_id, side, filled_price,
                                   size, filled_at)
                VALUES (%s, %s, %s, 't-shared', 'tok-yes-1', 'BUY',
                        0.56, 10, %s)
            """, (acc, oid, cid, TS))
        assert scalar(db, "SELECT count(*) FROM fills "
                          "WHERE trade_id='t-shared'") == 2

    def test_multi_account_same_settlement_id(self, db):
        """v3.1：两账户各自结算同一 settlement_id（复合主键互不冲突）。"""
        for acc in ("wallet_a", "wallet_b"):
            exec_sql(db, """
                INSERT INTO settlements (account_id, settlement_id, token_id,
                                         outcome, settlement_ts, status)
                VALUES (%s, 'stl-shared', 'tok', 1.0, %s, 'PENDING')
            """, (acc, TS))
            exec_sql(db, """
                INSERT INTO accounting_effects (account_id, settlement_id,
                                                token_id, outcome, pnl_delta,
                                                settlement_ts)
                VALUES (%s, 'stl-shared', 'tok', 1.0, -5.0, %s)
            """, (acc, TS))
        assert scalar(db, "SELECT count(*) FROM settlements "
                          "WHERE settlement_id='stl-shared'") == 2
        assert scalar(db, "SELECT count(*) FROM accounting_effects "
                          "WHERE settlement_id='stl-shared'") == 2

    def test_multi_account_same_token_positions(self, db):
        """v3.1：两账户各自持仓同一 token_id（唯一键含 account_id）。"""
        for acc in ("wallet_a", "wallet_b"):
            exec_sql(db, """
                INSERT INTO positions (account_id, token_id, qty, source,
                                       snapshot_at)
                VALUES (%s, 'tok', 100, 'local_engine', %s)
            """, (acc, TS))
        assert scalar(db, "SELECT count(*) FROM positions "
                          "WHERE token_id='tok'") == 2

    def test_multi_account_same_signal_no_collision(self, db):
        """v3.1：两账户同 strategy 同 token 的信号由 signal_pk 代理键区分（signal_id 各自唯一）。"""
        seed_signal(db, sid="sig-a", token="tok-yes-1")
        seed_signal(db, sid="sig-b", token="tok-yes-1")
        assert scalar(db, "SELECT count(*) FROM signals") == 2

    def test_signal_id_collision_fail_closed(self, db):
        """DB-HA-3：重复 signal_id 显式抛 UniqueViolation（禁止静默吞）。"""
        seed_signal(db, sid="sig-dup")
        with pytest.raises(psycopg2.errors.UniqueViolation):
            seed_signal(db, sid="sig-dup")
        db.rollback()
        assert scalar(db, "SELECT count(*) FROM signals "
                          "WHERE signal_id='sig-dup'") == 1


class TestV3Outcome:
    def test_outcome_0_5_disputed_settlement_accepted(self, db):
        """盲区④：UMA 争议「50/50」结算 outcome=0.5 可写入。"""
        exec_sql(db, """
            INSERT INTO settlements (settlement_id, token_id, outcome,
                                     settlement_ts, status)
            VALUES ('stl-half', 'tok', 0.5, %s, 'PENDING')
        """, (TS,))
        assert scalar(db, "SELECT outcome FROM settlements "
                          "WHERE settlement_id='stl-half'") == Decimal("0.5000")

    def test_outcome_out_of_range_rejected(self, db):
        """outcome > 1 被 CHECK 拒绝（C-10 范围约束）。"""
        with pytest.raises(psycopg2.errors.CheckViolation):
            exec_sql(db, """
                INSERT INTO settlements (settlement_id, token_id, outcome,
                                         settlement_ts, status)
                VALUES ('stl-bad', 'tok', 1.5, %s, 'PENDING')
            """, (TS,))
        db.rollback()


class TestV3NullTrap:
    def test_forecast_unique_key_distinguishes_product(self, db):
        """盲区③：model_version/product 参与唯一键 → 不同 product 不丢历史。"""
        seed_forecast(db, product="hourly")
        seed_forecast(db, product="daily")
        assert scalar(db, "SELECT count(*) FROM forecast_runs") == 2

    def test_forecast_model_version_default_not_null(self, db):
        """盲区③：model_version 无显式值时默认 ''（非 NULL），唯一键仍生效。"""
        seed_forecast(db)  # 默认 model_version='' product=''
        seed_forecast(db)  # 同键重放 → 去重
        assert scalar(db, "SELECT count(*) FROM forecast_runs") == 1
        # model_version 列为 NOT NULL DEFAULT ''
        val = scalar(db, """
            SELECT model_version FROM forecast_runs LIMIT 1
        """)
        assert val == ""
