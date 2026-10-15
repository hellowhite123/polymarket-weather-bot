# 交易数据库字段映射 — 每个字段的追溯来源

- **状态**：Draft v3.1 — 收尾定稿（随 `trading_database_design.md` v3.1；DB-HA-1~4 冻结 + 4 盲区 + v3.1 多账户隔离补丁）
- **基线**：commit `6722efd`
- **标注**：✅ = 现有代码可追溯（附位置）；🆕 = 新增设计；⚠️ = 待验证；🔒 = v3 冻结裁决

---

## markets

| 列 | 来源 | 标注 |
|----|------|------|
| market_id | — | 🆕 内部代理键 |
| platform | — | 🆕 多平台预留 |
| market_ref | `MarketInfo.condition_id`（`market_discovery/models.py:70`） | ✅ |
| question | `MarketInfo.question`（`models.py:73`） | ✅ |
| event_type | `MarketInfo.event_type`（`models.py:86`）；枚举同源 `EventType`（`strategy_engine/models.py:21-27`） | ✅ |
| location | `MarketInfo.location`（`models.py:85`） | ✅ |
| category | `MarketInfo.category`（`models.py:80`） | ✅ |
| start_time | `MarketInfo.start_date_utc`（unix 秒 → TIMESTAMPTZ） | ✅ |
| close_time | `MarketInfo.end_date_utc`（`models.py:76`） | ✅ |
| settlement_time | `TimeWindowGuard.settlements`（`risk_engine.rs:517-535`）+ `update_settlement_times_rpc.md` | ⚠️ 注册链路未实现（P0-RISK-002） |
| active / closed | `MarketInfo.active/closed`（`models.py:78-79`） | ✅ |
| volume / liquidity | `MarketInfo.volume/liquidity`（`models.py:82-83`） | ✅ |
| platform_meta | `MarketMeta{tick_size,neg_risk}`（`fast/market_cache.rs:28-31`）+ Gamma 原始字段 | ✅ + 🆕 JSONB 容器 |
| created_at / updated_at | — | 🆕 审计字段 |

## market_tokens

| 列 | 来源 | 标注 |
|----|------|------|
| platform / token_id | `MarketInfo.yes_token_id`/`no_token_id`（`models.py:71-72`）；`MarketSnapshot.token_for()`（`strategy_engine/models.py:108-114`） | ✅ + 🆕 表拆分 |
| market_id | — | 🆕 FK |
| outcome | `MarketInfo.outcomes`（`models.py:74`，默认 ["Yes","No"]） | ✅ |

## forecast_runs

| 列 | 来源 | 标注 |
|----|------|------|
| forecast_run_id | — | 🆕 |
| source | `SourceMeta.source`（`weather_data_ingestion/models.py`，经 `WeatherForecast.source.source`，`models.py:190-194`） | ✅ |
| location / lat / lon | `WeatherForecast.location_name/lat/lon`（`models.py:172-174`） | ✅ |
| issue_time | `SourceMeta.issue_time`（`models.py:192`） | ✅ |
| fetch_time | `SourceMeta.fetch_time`（`models.py:193`） | ✅ |
| model_version | — | 🆕（现有代码无，V-3）；🔒 `NOT NULL DEFAULT ''`（PG NULL 唯一键陷阱，盲区③） |
| product | — | 🆕（C-9：hourly/daily/minutely）；🔒 `NOT NULL DEFAULT ''` |
| payload | `ForecastPeriod.to_dict()` 全量（`models.py:123-142`）+ `minutely_*`（`models.py:181-182`） | ✅（JSONB 原样收） |
| source_file | `bridge.find_forecast_files` 命名约定 `<source>_<Location>_<ts>.json`（`market_discovery/bridge.py:64-83`） | ✅ |
| ingest_status | — | 🆕 |

> 唯一键：`UNIQUE(source, location, issue_time, model_version, product)`（v3 冻结，C-9）。

## signals

| 列 | 来源 | 标注 |
|----|------|------|
| signal_pk | — | 🆕🔒 内部代理主键（DB-HA-3：不碰 Python 源码，`signal_id` 降级为业务键） |
| signal_id | `TradingSignal.signal_id`（`strategy_engine/models.py:137`，`uuid4().hex[:12]` = 48-bit） | ✅；🔒 `TEXT NOT NULL UNIQUE`（128-bit UUID 记技术债；碰撞 FAIL-CLOSED 报错） |
| forecast_run_id | — | 🆕（现有代码无关联，C-3 同类缺口） |
| platform | — | 🆕 |
| market_ref | `TradingSignal.condition_id`（`models.py:145`） | ✅ |
| token_id / yes_token_id / no_token_id | `TradingSignal.token_id/yes_token_id/no_token_id`（`models.py:140-147`） | ✅ |
| strategy | `TradingSignal.strategy`（`models.py:138`） | ✅ |
| strategy_version | — | 🆕（V-3） |
| direction | `SignalDirection`（`models.py:15-18`） | ✅ |
| event_type / location | `TradingSignal.event_type/location`（`models.py:141-142`） | ✅ |
| target_price / market_price / quantity | `TradingSignal`（`models.py:150-152`） | ✅ |
| confidence / deviation / sigma_level | `TradingSignal`（`models.py:155-157`） | ✅ |
| generated_at / expires_at | `TradingSignal`（`models.py:160-161`） | ✅ |
| context | `EnsembleForecast.source_probs/source_weights/dispersion/confidence_interval`（`ensemble_fusion.py:273-305`）+ `MarketSnapshot`（`models.py:72-127`） | ✅（内容来源）+ 🆕（落库行为） |
| metadata | `TradingSignal.metadata`（`models.py:164`） | ✅ |

## signal_order_links 🆕

| 列 | 来源 | 标注 |
|----|------|------|
| signal_id | `TradingSignal.signal_id` | ✅（键） |
| order_id | orders 内部 ID | 🆕 |
| role | `ConflictResolution.contributing_signals / is_void`（`aggregator/models.py:45-60`） | ✅（语义来源）+ 🆕（表） |

> 缺口：`AggregatedOrder` 只有 `contributing_strategies`（策略名）与 `input_signal_count`（`aggregator/models.py:95-96`），无 signal_id 列表（C-3）。

## orders

| 列 | 来源 | 标注 |
|----|------|------|
| order_id | — | 🆕 内部代理键 |
| platform | — | 🆕 |
| account_id | — | 🆕🔒 账户维度（DB-HA-4），默认 `'default'` |
| client_order_id | `OrderState.client_order_id`（`fast/order_state.rs:60`）= `AggregatedOrder.order_id`（`aggregator/models.py:80`） | ✅ |
| exchange_order_id | `OrderState.exchange_order_id`（`order_state.rs:61`）= `OrderResult.exchange_order_id`（proto:126） | ✅ |
| market_id / token_id | `OrderState.token_id`（`order_state.rs:62`） | ✅ |
| side | `OrderState.side`（`order_state.rs:63`）；gRPC 映射 BUY_YES→Buy / BUY_NO→Sell（AGENTS.md 约定） | ✅ |
| direction | `AggregatedOrder.direction`（proto:68-72） | ✅ |
| limit_price | `OrderState.limit_price`（`order_state.rs:68`，G4/P1-RISK-006） | ✅ |
| target_price / market_price | `AggregatedOrder.target_price/market_price`（proto:73-74） | ✅ |
| requested_size / filled_size / remaining_size | `OrderState`（`order_state.rs:64-66`） | ✅ |
| status | `OrderStateStatus` 8 值（`order_state.rs:30-39`）；CHECK 与 `Display` 串逐值一致 | ✅ |
| confidence | `AggregatedOrder.confidence`（proto:78） | ✅ |
| contributing_strategies / input_signal_count | proto:81-82 | ✅ |
| block_reason / risk_checks | `AggregatedOrder.block_reason/risk_checks`（proto:86-87） | ✅ |
| order_type / tick_size / neg_risk | `FastOrderParams.order_type/tick_size/neg_risk`（`fast/executor.rs:71-75`） | ✅ |
| paper_trading | `AggregatedOrder.paper_trading`（proto:96） | ✅ |
| strategy_version / context | — | 🆕 |
| metadata | `AggregatedOrder.metadata`（proto:93） | ✅ |
| last_source_seq | 事件源单调序号 | 🆕🔒 乱序 UPSERT 水位线守卫（盲区②） |
| placed_at | `FillRecord.placed_at`（`fill_records.rs:84`）口径 | ✅ |
| created_at / updated_at | `OrderState.created_at/updated_at`（`order_state.rs:70-71`） | ✅（unix 秒 → TIMESTAMPTZ） |

## order_events 🆕（整表新增）

| 列 | 来源 | 标注 |
|----|------|------|
| event_id | 事件源单调序列 / `(order_id, trigger, seq)` | 🆕🔒 稳定事件 ID（C-8 冻结，幂等键 `UNIQUE(order_id, event_id)`） |
| from_status/to_status | `OrderStateStatus` 迁移方法（`order_state.rs:99-157`） | ✅（枚举）+ 🆕（记录行为） |
| trigger | 迁移方法语义（mark_submitted/mark_live/update_fill/apply_fill/mark_cancelled/mark_rejected/mark_expired/bind_remote_order） | ✅ |
| actor | — | 🆕 |
| payload | `TradeEvent`/`OrderStateEvent`（`fast/market_events.rs:61-106`） | ✅ |
| occurred_at | — | 🆕（纯观测时间戳，不参与幂等） |

## fills

| 列 | 来源 | 标注 |
|----|------|------|
| fill_id | — | 🆕 |
| platform | — | 🆕 |
| account_id | — | 🆕🔒 账户维度（DB-HA-4） |
| order_id（FK） | `FillRecord.order_id`（`fill_records.rs:76`） | ✅ |
| client_order_id / exchange_order_id | 同上 + `OrderState` | ✅ |
| trade_id | `FillRecord.trade_id`（`fill_records.rs:78`，P0-015 幂等键）；空串语义 `claim_trade`（`fill_records.rs:334-340`） | ✅ |
| token_id / side | `FillRecord.token_id/side`（`fill_records.rs:80-82`） | ✅ |
| limit_price / filled_price | `FillRecord`（`fill_records.rs:90-92`） | ✅ |
| size | `FillRecord.size`（`fill_records.rs:94`，shares） | ✅（单位 ⚠️ C-6） |
| fee / slippage / slippage_pct | `FillRecord`（`fill_records.rs:96-100`）+ `compute_slippage`（`fill_records.rs:272-279`） | ✅ |
| status | `FillStatus`（`fill_records.rs:49-56`） | ✅ |
| placed_at / filled_at / settled_at | `FillRecord`（`fill_records.rs:84-88`） | ✅（settled_at 时间源问题见 C-1） |
| pnl | `FillRecord.pnl`（`fill_records.rs:102`）→ 落 pnl_records（fill 级 pnl 在 settle_by_token 逐笔计算，`fill_records.rs:401-422`） | ✅ |
| trader_side / taker_order_id / maker_order_id / owner / trade_status / tx_hash | `TradeEvent`（`market_events.rs:61-79`）+ `TradeApiResponse`（`market_events.rs:121-164`，P0-013） | ✅ |

## positions

| 列 | 来源 | 标注 |
|----|------|------|
| account_id | — | 🆕🔒 账户维度（DB-HA-4），默认 `'default'` |
| qty / notional / reserved | `PositionLimits.PositionEntry`（`risk_engine.rs:219-224`） | ✅ |
| avg_price / current_value / cash_pnl | `RemotePosition`（`state_sync.rs:37-53`） | ✅ |
| source | local_engine（`post_check`/`apply_fill_order`，`risk_engine.rs:334-404`）/ remote_snapshot（`sync_position`，`risk_engine.rs:435-448`） | ✅ + 🆕（列） |
| last_source_seq | 事件源单调序号 | 🆕🔒 乱序 UPSERT 水位线守卫（盲区②） |
| snapshot_at | — | 🆕 |
| （唯一键 platform+account_id+token_id+source） | `positions: RwLock<HashMap<String, PositionEntry>>`（`risk_engine.rs:232`） | ✅（内存态）+ 🔒（DB 加 account_id，DB-HA-4） |

## settlements（对齐 G1-001 v7 §5.2，逐字段）

| 列 | 来源 | 标注 |
|----|------|------|
| settlement_id | v7 §5.2 `SettlementRecord.settlement_id` | ⚠️ POC-3 BLOCKED（V-1）；🔒 v3.1 与 account_id 构成复合主键 |
| account_id | — | 🆕🔒 账户维度（DB-HA-4）；v3.1 与 settlement_id 构成复合主键 |
| token_id / outcome | v7 §5.2 + `SettlementEvent.token_id/outcome`（`market_events.rs:111-112`） | ✅；🔒 outcome 支持 0/0.5/1（0.5 = 争议 50/50，盲区④） |
| settlement_ts | v7 §9（唯一来源 = settlement event）+ `SettlementEvent.settled_at`（`market_events.rs:113`） | ✅ |
| pnl_delta | v7 §3.4（唯一权威来源 = `settle_by_token`） | ✅（设计）⚠️（未实现） |
| status | v7 §5.3 五态 | ✅（设计） |
| accounting_effect_id | v7 §5.2 | ✅（设计） |
| received_at / processed_at | v7 §5.2（仅观测） | ✅（设计） |
| uma_tx_hash / block_number / identifier / requester | `PriceSettledEvent`（`uma_monitor.rs:93-101`） | ✅ |

## accounting_effects（v7 §5.6 方案 A，逐字段）

| 列 | 来源 | 标注 |
|----|------|------|
| settlement_id PK | v7 §5.6 D1 表定义 `effects(settlement_id TEXT PRIMARY KEY, ...)` | ✅（设计）；🔒 v3.1 复合主键 `(account_id, settlement_id)` + 复合外键 |
| account_id | — | 🆕🔒 账户维度（DB-HA-4） |
| token_id / outcome / pnl_delta / settlement_ts / applied_at | 同上 | ✅（设计）；🔒 outcome 支持 0/0.5/1 |

## pnl_records

| 列 | 来源 | 标注 |
|----|------|------|
| account_id | — | 🆕🔒 账户维度（DB-HA-4），唯一索引含 account_id |
| pnl_type | realized_fill（`settle_by_token` 逐笔）/ realized_order（`OrderFill.pnl`，`fill_records.rs:205`）/ daily_snapshot（`DailyLossLimit.daily_pnl`，`risk_engine.rs:602`）/ unrealized_snapshot（`RemotePosition.cash_pnl`） | ✅（来源）+ 🆕（表） |
| amount | `compute_pnl`（`fill_records.rs:291-297`，net 口径冻结，v7 §3.3） | ✅ |
| currency | USDC（chain_id=137 约定，AGENTS.md） | ✅ |
| accounting_day | v7 §10（归属 settlement_ts 的 UTC 日） | ✅（设计） |
| fill_id / order_id / settlement_id / token_id | 上述各表 | ✅ |

## risk_events

| 列 | 来源 | 标注 |
|----|------|------|
| event_type / rule | `RiskCheckResult.rule`（proto:58）+ 各检查器（`risk_engine.rs` PositionLimits/TimeWindowGuard/DailyLossLimit/OracleDisputeMonitor/GasFeeProtector/ConcentrationChecker） | ✅ |
| breach_level | `BreachLevel`（`risk_engine.rs:80-89`） | ✅ |
| passed / reason | `RiskCheckResult`（proto:59-60） | ✅ |
| measured_value / threshold_value / daily_pnl | `RiskConfig` 阈值（`risk_engine.rs:108-127`）+ `DailyLossLimit.daily_pnl`（`risk_engine.rs:602`） | ✅ |
| detail | `OracleStatus`（`risk_engine.rs:710-722`） | ✅ |
| occurred_at / order 关联 | — | 🆕 |

## system_events / reconciliation_runs

| 列 | 来源 | 标注 |
|----|------|------|
| component / event_type / severity | `ConnectionState`（`state_sync.rs:273-289`）、daemon 六阶段（`fast/daemon.rs`）、`DiffSeverity`（`state_sync.rs:185-189`） | ✅（枚举来源）+ 🆕（表） |
| report JSONB / in_sync / positions_checked / orders_checked | `ReconciliationReport`（`state_sync.rs:193-199`）+ `DiffItem`（`state_sync.rs:175-181`） | ✅ |
| payload | `RecoveryPlan`/`RecoveryAction`（`state_sync.rs:227-254`）、`InDoubtResolution`（`state_sync.rs:1150`） | ✅ |

## schema_migrations 🆕（整表新增，行业惯例）
