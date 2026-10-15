-- ============================================================================
-- 0001_init.up.sql — 交易数据库初始 Schema（PostgreSQL 15+）
--
-- 设计文档：docs/design/trading_database_design.md（Draft v3.1 — 收尾定稿）
-- 字段映射：docs/design/trading_database_field_mapping.md（Draft v3）
-- 基线：commit 6722efd
--
-- ⚠️ 本 migration 仅用于独立测试库验证；禁止在真实交易数据库执行。
-- 精度约定：金额 NUMERIC(20,6)（USDC 6 位小数）、价格/概率 NUMERIC(10,6)、
--           数量 NUMERIC(20,6)（shares）、时间一律 TIMESTAMPTZ（UTC）。
--
-- v3 定稿要点（DB-HA-1~4 冻结 + 4 盲区修正）：
--   DB-HA-4  account_id 维度：orders/fills/positions/settlements/accounting_effects/pnl_records
--   DB-HA-3  signals 内部代理主键 signal_pk（signal_id 降级为 UNIQUE 业务键）
--   盲区②   orders/positions 加 last_source_seq 水位线（乱序 UPSERT 守卫）
--   盲区③   forecast_runs.model_version/product 一律 NOT NULL DEFAULT ''（NULL 唯一键陷阱）
--   盲区④   settlements/accounting_effects.outcome 支持 0/0.5/1（0.5 = UMA 争议 50/50）
--
-- v3.1 收尾补丁（多账户隔离唯一键 + SQL 边界加固）：
--   fills 唯一键加 account_id（自成交/共享 trade_id 不互斥）
--   settlements/accounting_effects 复合主键 (account_id, settlement_id)；pnl_records 复合外键
--   orders/positions.last_source_seq 改 NOT NULL DEFAULT 0（消除 NULL 覆盖漏洞）
--   signals.fk_signals_market 用 PG15 列级 SET NULL (market_ref)
--   orders 数量守恒约束 filled_size + remaining_size <= requested_size
-- ============================================================================

BEGIN;

-- --------------------------------------------------------------------------
-- 0. 迁移版本表
-- --------------------------------------------------------------------------
CREATE TABLE schema_migrations (
    version     INT PRIMARY KEY,
    name        TEXT NOT NULL,
    checksum    TEXT NOT NULL,
    applied_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- --------------------------------------------------------------------------
-- 1. markets — 市场目录（平台中立 + platform_meta 收 Polymarket 特有字段）
-- --------------------------------------------------------------------------
CREATE TABLE markets (
    market_id       BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    platform        TEXT NOT NULL DEFAULT 'polymarket',
    market_ref      TEXT NOT NULL,                 -- Polymarket = condition_id
    question        TEXT NOT NULL DEFAULT '',
    event_type      TEXT NOT NULL DEFAULT '',
    location        TEXT NOT NULL DEFAULT '',
    category        TEXT NOT NULL DEFAULT '',
    start_time      TIMESTAMPTZ,
    close_time      TIMESTAMPTZ,
    settlement_time TIMESTAMPTZ,                   -- TimeWindowGuard 注册源（P0-RISK-002）
    active          BOOLEAN NOT NULL DEFAULT true,
    closed          BOOLEAN NOT NULL DEFAULT false,
    volume          NUMERIC(20,6),
    liquidity       NUMERIC(20,6),
    platform_meta   JSONB NOT NULL DEFAULT '{}',   -- tick_size / neg_risk / gamma 原始字段
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_markets_platform_ref UNIQUE (platform, market_ref)
);
CREATE INDEX idx_markets_event_location ON markets (event_type, location);
CREATE INDEX idx_markets_close_open ON markets (close_time) WHERE NOT closed;

-- --------------------------------------------------------------------------
-- 2. market_tokens — 市场 ↔ 可交易 instrument（yes/no token）
-- --------------------------------------------------------------------------
CREATE TABLE market_tokens (
    platform    TEXT NOT NULL DEFAULT 'polymarket',
    token_id    TEXT NOT NULL,
    market_id   BIGINT NOT NULL REFERENCES markets (market_id) ON DELETE CASCADE,
    outcome     TEXT NOT NULL,                     -- yes / no / 其他
    CONSTRAINT pk_market_tokens PRIMARY KEY (platform, token_id),
    CONSTRAINT uq_market_tokens_market_outcome UNIQUE (market_id, outcome)
);

-- --------------------------------------------------------------------------
-- 3. forecast_runs — 气象预报拉取批次
--    v3：复合唯一键含 model_version/product，均 NOT NULL DEFAULT ''（NULL 陷阱）
-- --------------------------------------------------------------------------
CREATE TABLE forecast_runs (
    forecast_run_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source        TEXT NOT NULL,                   -- noaa / caiyun / ecmwf / jma
    location      TEXT NOT NULL DEFAULT '',
    lat           NUMERIC(9,5),
    lon           NUMERIC(9,5),
    issue_time    TIMESTAMPTZ NOT NULL,
    fetch_time    TIMESTAMPTZ NOT NULL,
    model_version TEXT NOT NULL DEFAULT '',        -- 新增设计（V-3）；盲区③
    product       TEXT NOT NULL DEFAULT '',        -- hourly/daily/minutely（C-9）；盲区③
    payload       JSONB NOT NULL,                  -- periods 全量（决策输入留痕）
    source_file   TEXT,                            -- 迁移期：data/raw 文件路径
    ingest_status TEXT NOT NULL DEFAULT 'ok'
                  CHECK (ingest_status IN ('ok', 'degraded', 'failed')),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_forecast_run
        UNIQUE (source, location, issue_time, model_version, product)
);
CREATE INDEX idx_forecast_runs_loc_issue ON forecast_runs (location, issue_time DESC);

-- --------------------------------------------------------------------------
-- 4. signals — 策略信号（DB-HA-3：内部代理主键 signal_pk；signal_id = 业务键）
-- --------------------------------------------------------------------------
CREATE TABLE signals (
    signal_pk        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    signal_id        TEXT NOT NULL,                -- TradingSignal.signal_id（48-bit，记技术债）
    forecast_run_id  BIGINT REFERENCES forecast_runs (forecast_run_id) ON DELETE SET NULL,
    platform         TEXT NOT NULL DEFAULT 'polymarket',
    market_ref       TEXT,                         -- condition_id
    token_id         TEXT NOT NULL DEFAULT '',
    yes_token_id     TEXT NOT NULL DEFAULT '',
    no_token_id      TEXT NOT NULL DEFAULT '',
    strategy         TEXT NOT NULL DEFAULT '',
    strategy_version TEXT,                         -- 新增设计（V-3 待约定）
    direction        TEXT NOT NULL CHECK (direction IN ('BUY_YES', 'BUY_NO')),
    event_type       TEXT NOT NULL DEFAULT '',
    location         TEXT NOT NULL DEFAULT '',
    target_price     NUMERIC(10,6),
    market_price     NUMERIC(10,6),
    quantity         NUMERIC(20,6),
    confidence       NUMERIC(10,6),
    deviation        NUMERIC(20,6),
    sigma_level      NUMERIC(20,6),
    generated_at     TIMESTAMPTZ NOT NULL,
    expires_at       TIMESTAMPTZ,
    context          JSONB NOT NULL DEFAULT '{}',  -- market snapshot / source_probs / weights
    metadata         JSONB NOT NULL DEFAULT '{}',
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- 48-bit 碰撞时写入层须 FAIL-CLOSED 报错（禁止 ON CONFLICT DO NOTHING 静默吞）
    CONSTRAINT uq_signals_signal_id UNIQUE (signal_id)
);
CREATE INDEX idx_signals_token_time ON signals (platform, token_id, generated_at DESC);
CREATE INDEX idx_signals_strategy_time ON signals (strategy, generated_at DESC);
CREATE INDEX idx_signals_expiry ON signals (expires_at) WHERE expires_at IS NOT NULL;
-- market_ref 非空时挂接 markets（复合外键）
-- v3.1：platform 为 NOT NULL，须用 PG15 列级 SET NULL，仅置空 market_ref
ALTER TABLE signals
    ADD CONSTRAINT fk_signals_market
    FOREIGN KEY (platform, market_ref) REFERENCES markets (platform, market_ref)
    ON DELETE SET NULL (market_ref);

-- --------------------------------------------------------------------------
-- 5. orders — 订单当前状态（OrderStateStatus 8 态）
--    v3：account_id（DB-HA-4）+ last_source_seq 水位线（盲区②）
-- --------------------------------------------------------------------------
CREATE TABLE orders (
    order_id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    platform          TEXT NOT NULL DEFAULT 'polymarket',
    account_id        TEXT NOT NULL DEFAULT 'default',   -- DB-HA-4
    client_order_id   TEXT NOT NULL,               -- AggregatedOrder.order_id
    exchange_order_id TEXT,                        -- CLOB 分配
    market_id         BIGINT REFERENCES markets (market_id) ON DELETE SET NULL,
    token_id          TEXT NOT NULL,
    side              TEXT NOT NULL CHECK (side IN ('BUY', 'SELL')),
    direction         TEXT NOT NULL CHECK (direction IN ('BUY_YES', 'BUY_NO')),
    limit_price       NUMERIC(10,6),
    target_price      NUMERIC(10,6),
    market_price      NUMERIC(10,6),
    requested_size    NUMERIC(20,6) NOT NULL CHECK (requested_size > 0),
    filled_size       NUMERIC(20,6) NOT NULL DEFAULT 0 CHECK (filled_size >= 0),
    remaining_size    NUMERIC(20,6) NOT NULL,
    status            TEXT NOT NULL CHECK (status IN (
                          'received', 'submitted', 'live', 'partial_filled',
                          'filled', 'cancelled', 'rejected', 'expired')),
    confidence        NUMERIC(10,6),
    contributing_strategies JSONB NOT NULL DEFAULT '[]',
    input_signal_count INT NOT NULL DEFAULT 0,
    block_reason      TEXT NOT NULL DEFAULT '',
    risk_checks       JSONB NOT NULL DEFAULT '[]',
    order_type        TEXT,
    tick_size         TEXT,
    neg_risk          BOOLEAN,
    paper_trading     BOOLEAN NOT NULL DEFAULT false,
    strategy_version  TEXT,
    metadata          JSONB NOT NULL DEFAULT '{}',
    context           JSONB NOT NULL DEFAULT '{}',
    last_source_seq   BIGINT NOT NULL DEFAULT 0,   -- 盲区②：乱序 UPSERT 水位线（v3.1 NOT NULL DEFAULT 0）
    placed_at         TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- D-4 幂等：与 OrderStore.claim 对应（client_order_id 全局唯一，多账户下仍成立）
    CONSTRAINT uq_orders_platform_client UNIQUE (platform, client_order_id),
    -- update_fill clamp 口径复刻
    CONSTRAINT chk_orders_fill_le_request CHECK (filled_size <= requested_size),
    CONSTRAINT chk_orders_remaining_nonneg CHECK (remaining_size >= 0),
    -- v3.1：数量守恒（filled + remaining 不得超 requested）
    CONSTRAINT chk_orders_size_conservation
        CHECK (filled_size + remaining_size <= requested_size)
);
-- D-4 幂等：与 bind_remote_order 一对一防护对应（部分唯一索引，NULL 不参与）
CREATE UNIQUE INDEX uq_orders_platform_exchange
    ON orders (platform, exchange_order_id)
    WHERE exchange_order_id IS NOT NULL;
CREATE INDEX idx_orders_token_status ON orders (platform, token_id, status);
CREATE INDEX idx_orders_active ON orders (platform, token_id)
    WHERE status IN ('received', 'submitted', 'live', 'partial_filled');
CREATE INDEX idx_orders_exchange ON orders (exchange_order_id)
    WHERE exchange_order_id IS NOT NULL;

-- --------------------------------------------------------------------------
-- 6. signal_order_links — 信号 ↔ 订单多对多（新增设计，C-3 缺口）
-- --------------------------------------------------------------------------
CREATE TABLE signal_order_links (
    signal_id TEXT NOT NULL REFERENCES signals (signal_id) ON DELETE CASCADE,
    order_id  BIGINT NOT NULL REFERENCES orders (order_id) ON DELETE CASCADE,
    role      TEXT NOT NULL DEFAULT 'contributing'
              CHECK (role IN ('contributing', 'voided')),
    CONSTRAINT pk_signal_order_links PRIMARY KEY (signal_id, order_id)
);
CREATE INDEX idx_sol_order ON signal_order_links (order_id);

-- --------------------------------------------------------------------------
-- 7. order_events — 订单状态变迁审计（新增设计）
--    v3：稳定 event_id 幂等（C-8 冻结），occurred_at 仅观测
-- --------------------------------------------------------------------------
CREATE TABLE order_events (
    order_event_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_id    TEXT NOT NULL,                     -- 事件源单调序列 / (order_id, trigger, seq)
    order_id    BIGINT NOT NULL REFERENCES orders (order_id) ON DELETE CASCADE,
    from_status TEXT CHECK (from_status IS NULL OR from_status IN (
                    'received', 'submitted', 'live', 'partial_filled',
                    'filled', 'cancelled', 'rejected', 'expired')),
    to_status   TEXT NOT NULL CHECK (to_status IN (
                    'received', 'submitted', 'live', 'partial_filled',
                    'filled', 'cancelled', 'rejected', 'expired')),
    trigger     TEXT NOT NULL,                     -- place/submit_ack/live/trade/cancel/reject/expire/recovery_*
    actor       TEXT NOT NULL DEFAULT 'rust',      -- rust / python / recovery
    payload     JSONB NOT NULL DEFAULT '{}',
    occurred_at TIMESTAMPTZ NOT NULL,              -- 纯观测，不参与幂等
    -- 重放幂等：同一 event_id 不产生第二行
    CONSTRAINT uq_order_events_replay UNIQUE (order_id, event_id)
);
CREATE INDEX idx_order_events_order ON order_events (order_id, occurred_at);

-- --------------------------------------------------------------------------
-- 8. fills — 成交明细（fill 级）
--    v3：account_id（DB-HA-4）
-- --------------------------------------------------------------------------
CREATE TABLE fills (
    fill_id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    platform          TEXT NOT NULL DEFAULT 'polymarket',
    account_id        TEXT NOT NULL DEFAULT 'default',   -- DB-HA-4
    order_id          BIGINT NOT NULL REFERENCES orders (order_id) ON DELETE RESTRICT,
    client_order_id   TEXT NOT NULL DEFAULT '',
    exchange_order_id TEXT NOT NULL DEFAULT '',
    trade_id          TEXT NOT NULL DEFAULT '',    -- fill 级幂等键（P0-015）；空串不参与去重
    token_id          TEXT NOT NULL,
    side              TEXT NOT NULL CHECK (side IN ('BUY', 'SELL')),
    limit_price       NUMERIC(10,6),
    filled_price      NUMERIC(10,6) NOT NULL,
    size              NUMERIC(20,6) NOT NULL CHECK (size > 0),
    fee               NUMERIC(20,6) NOT NULL DEFAULT 0,
    slippage          NUMERIC(20,6) NOT NULL DEFAULT 0,
    slippage_pct      NUMERIC(10,4) NOT NULL DEFAULT 0,
    status            TEXT NOT NULL DEFAULT 'filled'
                      CHECK (status IN ('filled', 'settled', 'cancelled')),
    placed_at         TIMESTAMPTZ,
    filled_at         TIMESTAMPTZ NOT NULL,
    settled_at        TIMESTAMPTZ,
    trader_side       TEXT NOT NULL DEFAULT '',    -- TAKER / MAKER
    taker_order_id    TEXT NOT NULL DEFAULT '',
    maker_order_id    TEXT NOT NULL DEFAULT '',
    owner             TEXT NOT NULL DEFAULT '',
    trade_status      TEXT NOT NULL DEFAULT '',    -- MATCHED / MINED 等
    tx_hash           TEXT NOT NULL DEFAULT '',
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- D-4 幂等：与 FillLedger.claim_trade 对应（空 trade_id 不参与，FAIL-CLOSED 口径）
-- v3.1：加 account_id —— 多账户共享同一撮合 trade_id（自成交/共同撮合）不互斥
CREATE UNIQUE INDEX uq_fills_platform_account_trade
    ON fills (platform, account_id, trade_id)
    WHERE trade_id <> '';
CREATE INDEX idx_fills_order ON fills (order_id);
CREATE INDEX idx_fills_token_time ON fills (platform, token_id, filled_at DESC);
-- 未结算扫描
CREATE INDEX idx_fills_unsettled ON fills (platform, token_id)
    WHERE settled_at IS NULL AND status = 'filled';

-- --------------------------------------------------------------------------
-- 9. positions — 持仓快照（投影表，非权威）
--    v3：account_id（DB-HA-4）+ last_source_seq 水位线（盲区②）
-- --------------------------------------------------------------------------
CREATE TABLE positions (
    position_id   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    platform      TEXT NOT NULL DEFAULT 'polymarket',
    account_id    TEXT NOT NULL DEFAULT 'default',       -- DB-HA-4
    token_id      TEXT NOT NULL,
    qty           NUMERIC(20,6) NOT NULL DEFAULT 0,
    notional      NUMERIC(20,6) NOT NULL DEFAULT 0,
    reserved      NUMERIC(20,6) NOT NULL DEFAULT 0,
    avg_price     NUMERIC(10,6),
    current_value NUMERIC(20,6),
    cash_pnl      NUMERIC(20,6),
    source        TEXT NOT NULL CHECK (source IN ('local_engine', 'remote_snapshot')),
    last_source_seq BIGINT NOT NULL DEFAULT 0,     -- 盲区②：乱序 UPSERT 水位线（v3.1 NOT NULL DEFAULT 0）
    snapshot_at   TIMESTAMPTZ NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_positions UNIQUE (platform, account_id, token_id, source)
);

-- --------------------------------------------------------------------------
-- 10. settlements — 结算记录（对齐 G1-001 v7 §5.2 逐字段）
--     ⚠️ settlement_id 未经 POC-3 验证前禁止生产写入
--     v3：account_id + outcome 支持 0/0.5/1 + 记账一致性 CHECK（C-10）
-- --------------------------------------------------------------------------
CREATE TABLE settlements (
    settlement_id       TEXT NOT NULL,             -- UMA 市场级事件 id（POC-3 待验证）
    platform            TEXT NOT NULL DEFAULT 'polymarket',
    account_id          TEXT NOT NULL DEFAULT 'default',   -- DB-HA-4；pnl 为账户级
    token_id            TEXT NOT NULL,
    outcome             NUMERIC(6,4),              -- 0 / 0.5 / 1（0.5 = UMA 争议 50/50）
    settlement_ts       TIMESTAMPTZ NOT NULL,      -- 权威结算时间（v7 §9）
    pnl_delta           NUMERIC(20,6),
    status              TEXT NOT NULL CHECK (status IN (
                            'PENDING', 'ACCOUNTING', 'ACCOUNTED',
                            'RETRYABLE', 'FAILED')),
    accounting_effect_id TEXT,
    received_at         TIMESTAMPTZ,               -- 仅观测
    processed_at        TIMESTAMPTZ,               -- 仅观测
    uma_tx_hash         TEXT,
    block_number        BIGINT,
    identifier          TEXT,
    requester           TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- v3.1：多账户各自结算（同 settlement_id 不同 account_id 可并存）
    CONSTRAINT pk_settlements PRIMARY KEY (account_id, settlement_id),
    -- C-10：outcome 范围（含 0.5 争议）；记账一致性兜底
    CONSTRAINT chk_settlements_outcome_range
        CHECK (outcome IS NULL OR (outcome >= 0 AND outcome <= 1)),
    CONSTRAINT chk_settlements_pnl_requires_accounted
        CHECK (pnl_delta IS NULL OR status = 'ACCOUNTED'),
    CONSTRAINT chk_settlements_effect_requires_accounted
        CHECK (accounting_effect_id IS NULL OR status = 'ACCOUNTED')
);
CREATE INDEX idx_settlements_open ON settlements (status) WHERE status <> 'ACCOUNTED';
CREATE INDEX idx_settlements_token ON settlements (platform, token_id);

-- --------------------------------------------------------------------------
-- 11. accounting_effects — 结算记账效果（v7 §5.6 方案 A：完整 identity；I2 恰一笔）
--     v3：account_id + outcome 支持 0/0.5/1
-- --------------------------------------------------------------------------
CREATE TABLE accounting_effects (
    settlement_id TEXT NOT NULL,
    account_id    TEXT NOT NULL DEFAULT 'default',       -- DB-HA-4
    token_id      TEXT NOT NULL,
    outcome       NUMERIC(6,4),                    -- 0 / 0.5 / 1
    pnl_delta     NUMERIC(20,6) NOT NULL,          -- NUMERIC 定点 = canonical 比较要求（v7 §6.4）
    settlement_ts TIMESTAMPTZ NOT NULL,
    applied_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- v3.1：I2 由复合主键保证（每账户每 settlement 恰一笔 effect）
    CONSTRAINT pk_accounting_effects PRIMARY KEY (account_id, settlement_id),
    CONSTRAINT fk_effects_settlement
        FOREIGN KEY (account_id, settlement_id)
        REFERENCES settlements (account_id, settlement_id) ON DELETE RESTRICT,
    CONSTRAINT chk_effects_outcome_range
        CHECK (outcome IS NULL OR (outcome >= 0 AND outcome <= 1))
);
-- settlement_ts::date 对 timestamptz 非 IMMUTABLE（依赖时区），须显式 UTC
CREATE INDEX idx_effects_day
    ON accounting_effects (((settlement_ts AT TIME ZONE 'UTC')::date));

-- --------------------------------------------------------------------------
-- 12. pnl_records — 盈亏流水（net 口径 = 现有 compute_pnl，冻结）
--     v3：account_id（DB-HA-4）纳入唯一索引
-- --------------------------------------------------------------------------
CREATE TABLE pnl_records (
    pnl_record_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    platform      TEXT NOT NULL DEFAULT 'polymarket',
    account_id    TEXT NOT NULL DEFAULT 'default',       -- DB-HA-4
    pnl_type      TEXT NOT NULL CHECK (pnl_type IN (
                      'realized_fill', 'realized_order',
                      'daily_snapshot', 'unrealized_snapshot')),
    amount        NUMERIC(20,6) NOT NULL,
    currency      TEXT NOT NULL DEFAULT 'USDC',
    fill_id       BIGINT REFERENCES fills (fill_id) ON DELETE SET NULL,
    order_id      BIGINT REFERENCES orders (order_id) ON DELETE SET NULL,
    settlement_id TEXT,                            -- 关联 settlements（复合外键见下表级约束）
    token_id      TEXT NOT NULL DEFAULT '',
    accounting_day DATE NOT NULL,                  -- settlement_ts 的 UTC 日（v7 §10）
    recorded_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- v3.1：复合外键引用 settlements(account_id, settlement_id)；删 settlement 仅置空 settlement_id
    CONSTRAINT fk_pnl_settlement
        FOREIGN KEY (account_id, settlement_id)
        REFERENCES settlements (account_id, settlement_id)
        ON DELETE SET NULL (settlement_id)
);
CREATE UNIQUE INDEX uq_pnl_fill ON pnl_records (platform, account_id, pnl_type, fill_id)
    WHERE fill_id IS NOT NULL;
CREATE UNIQUE INDEX uq_pnl_order ON pnl_records (platform, account_id, pnl_type, order_id)
    WHERE order_id IS NOT NULL AND pnl_type = 'realized_order';
CREATE UNIQUE INDEX uq_pnl_daily ON pnl_records (platform, account_id, pnl_type, accounting_day)
    WHERE pnl_type = 'daily_snapshot';
CREATE INDEX idx_pnl_day ON pnl_records (accounting_day);
CREATE INDEX idx_pnl_token ON pnl_records (platform, token_id, accounting_day);

-- --------------------------------------------------------------------------
-- 13. risk_events — 风控事件
-- --------------------------------------------------------------------------
CREATE TABLE risk_events (
    risk_event_id  BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    platform       TEXT NOT NULL DEFAULT 'polymarket',
    event_type     TEXT NOT NULL CHECK (event_type IN (
                       'rule_block', 'circuit_breaker', 'gate_state',
                       'oracle_deviation', 'manual_halt')),
    rule           TEXT,                           -- position_limit / settlement_window / daily_loss / ...
    breach_level   TEXT CHECK (breach_level IS NULL OR breach_level IN (
                       'normal', 'warning', 'restricted', 'halted')),
    order_id       BIGINT REFERENCES orders (order_id) ON DELETE SET NULL,
    client_order_id TEXT NOT NULL DEFAULT '',
    token_id       TEXT NOT NULL DEFAULT '',
    passed         BOOLEAN,
    reason         TEXT NOT NULL DEFAULT '',
    measured_value NUMERIC(20,6),
    threshold_value NUMERIC(20,6),
    daily_pnl      NUMERIC(20,6),
    detail         JSONB NOT NULL DEFAULT '{}',
    occurred_at    TIMESTAMPTZ NOT NULL
);
CREATE INDEX idx_risk_events_time ON risk_events (occurred_at);
CREATE INDEX idx_risk_events_type_time ON risk_events (event_type, occurred_at);
CREATE INDEX idx_risk_events_token ON risk_events (token_id);

-- --------------------------------------------------------------------------
-- 14. reconciliation_runs — 对账批次头
-- --------------------------------------------------------------------------
CREATE TABLE reconciliation_runs (
    recon_run_id      BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    platform          TEXT NOT NULL DEFAULT 'polymarket',
    started_at        TIMESTAMPTZ NOT NULL,
    finished_at       TIMESTAMPTZ,
    in_sync           BOOLEAN,
    positions_checked INT NOT NULL DEFAULT 0,
    orders_checked    INT NOT NULL DEFAULT 0,
    critical_diffs    INT NOT NULL DEFAULT 0,
    report            JSONB NOT NULL DEFAULT '{}'   -- ReconciliationReport 全量
);

-- --------------------------------------------------------------------------
-- 15. system_events — 系统事件
-- --------------------------------------------------------------------------
CREATE TABLE system_events (
    system_event_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    platform       TEXT,
    component      TEXT NOT NULL,                  -- daemon / state_sync / market_events / uma_monitor / grpc / db_writer / migration
    event_type     TEXT NOT NULL,                  -- startup / shutdown / reconnect / healthcheck / error / dead_letter / migration
    severity       TEXT NOT NULL DEFAULT 'info'
                   CHECK (severity IN ('info', 'warning', 'critical')),
    recon_run_id   BIGINT REFERENCES reconciliation_runs (recon_run_id) ON DELETE SET NULL,
    payload        JSONB NOT NULL DEFAULT '{}',
    occurred_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_system_events_comp ON system_events (component, event_type, occurred_at);
CREATE INDEX idx_system_events_critical ON system_events (occurred_at)
    WHERE severity = 'critical';

-- --------------------------------------------------------------------------
-- 版本登记
-- --------------------------------------------------------------------------
INSERT INTO schema_migrations (version, name, checksum)
VALUES (1, 'init', 'sha256:see-db/migrations/CHECKSUMS.txt');

COMMIT;
