//! 市场事件监听 (MarketEvents)
//!
//! 实时抓取 Polymarket 市场事件：成交、订单簿更新、结算。
//! 通过 CLOB HTTP API 轮询实现，不依赖链上 WebSocket。
//!
//! ## 事件类型
//!
//! | 事件 | 数据源 | 轮询间隔 |
//! |------|--------|---------|
//! | Trade | `GET /data/trades?token_id=X` | 2s |
//! | OrderUpdate | `GET /data/orders` (open orders) | 5s |
//! | OrderBook | `GET /book?token_id=X` | 1s |
//! | Settlement | 链上事件 (后续扩展) | — |
//!
//! ## 架构
//!
//! ```text
//! CLOB API ──poll──→ MarketEventPoller ──channel──→ subscribers
//!                                      └──→ EventsSnapshot
//! ```

use crate::clob::auth::{create_l2_headers, L2HeaderArgs};
use crate::fast::fill_records::FillLedger;
use crate::types::clob_types::{ApiKeyCreds, OrderBookSummary};
use ethers::signers::LocalWallet;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::broadcast;

// ============================================================================
// 常量
// ============================================================================

pub const DEFAULT_POLL_INTERVAL_TRADE: Duration = Duration::from_secs(2);
pub const DEFAULT_POLL_INTERVAL_ORDER: Duration = Duration::from_secs(5);
pub const DEFAULT_POLL_INTERVAL_BOOK: Duration = Duration::from_secs(1);
pub const EVENT_CHANNEL_CAPACITY: usize = 256;

// ============================================================================
// 事件类型
// ============================================================================

/// 市场事件枚举
#[derive(Debug, Clone)]
pub enum MarketEvent {
    /// 成交事件
    Trade(TradeEvent),
    /// 订单簿更新
    OrderBookUpdate(BookSnapshot),
    /// 订单状态变更（部分成交、取消等）
    OrderStateChange(OrderStateEvent),
    /// 市场结算
    Settlement(SettlementEvent),
}

/// 成交事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeEvent {
    pub token_id: String,
    /// 成交唯一标识（trade id，作为 fill 幂等 key）
    pub trade_id: String,
    /// 归属后的「我的订单 ID」（按 trader_side 正确归属，见 P0-013）
    pub order_id: String,
    pub side: String, // BUY / SELL
    pub price: f64,
    pub size: f64,
    pub timestamp: u64,
    pub taker_order_id: String,
    pub maker_order_id: String,
    /// 当前用户在此成交中的角色：TAKER / MAKER
    pub trader_side: String,
    /// 归属方地址（owner）
    pub owner: String,
    /// 成交状态（MATCHED / MINED 等）
    pub status: String,
}

/// 订单簿快照
#[derive(Debug, Clone)]
pub struct BookSnapshot {
    pub token_id: String,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    pub timestamp: u64,
}

/// 订单簿价格档位
#[derive(Debug, Clone)]
pub struct Level {
    pub price: f64,
    pub size: f64,
}

/// 订单状态变更
#[derive(Debug, Clone)]
pub struct OrderStateEvent {
    pub order_id: String,
    pub token_id: String,
    pub status: String,
    pub filled_size: f64,
    pub remaining_size: f64,
    pub avg_price: Option<f64>,
}

/// 结算事件
#[derive(Debug, Clone)]
pub struct SettlementEvent {
    pub token_id: String,
    pub outcome: f64, // 0.0 或 1.0 for binary markets
    pub settled_at: u64,
    pub uma_proposal: bool,
}

// ============================================================================
// TradeData — API 响应反序列化
// ============================================================================

#[derive(Debug, Deserialize)]
struct TradeApiResponse {
    #[serde(default)]
    id: String,
    #[serde(default)]
    side: String,
    #[serde(default)]
    price: serde_json::Value, // string or number
    #[serde(default)]
    size: serde_json::Value,
    #[serde(default)]
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(default)]
    #[serde(rename = "takerOrderId")]
    taker_order_id: String,
    #[serde(default)]
    #[serde(rename = "makerOrderId")]
    maker_order_id: String,
    // ── P0-013 新增：完整 trade metadata ──
    /// 当前用户角色：TAKER / MAKER
    #[serde(default)]
    #[serde(rename = "traderSide")]
    trader_side: String,
    /// taker 地址
    #[serde(default)]
    owner: String,
    #[serde(default)]
    #[serde(rename = "makerAddress")]
    maker_address: String,
    /// maker 订单列表（归属 MAKER 成交用）
    #[serde(default)]
    #[serde(rename = "makerOrders")]
    maker_orders: Vec<MakerOrderRef>,
    /// 成交状态（MATCHED / MINED 等）
    #[serde(default)]
    status: String,
    #[serde(default)]
    #[serde(rename = "feeRateBps")]
    fee_rate_bps: String,
    #[serde(default)]
    #[serde(rename = "transactionHash")]
    transaction_hash: String,
}

/// `maker_orders` 数组元素（P0-013 归属 MAKER 成交用）。
#[derive(Debug, Clone, Deserialize)]
struct MakerOrderRef {
    #[serde(default)]
    #[serde(rename = "orderId")]
    order_id: String,
    #[serde(default)]
    #[serde(rename = "makerAddress")]
    maker_address: String,
}

// ============================================================================
// OpenOrderSnapshot — CLOB API 返回的开仓订单结构
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct OpenOrderSnapshot {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub asset_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    #[serde(rename = "size_matched")]
    pub size_matched: String,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub price: String,
}

// ============================================================================
// MarketEventPoller
// ============================================================================

/// L2 认证上下文：对需认证的 `/data/*` 端点签名（P0-EXEC-004）。
struct L2Auth {
    wallet: LocalWallet,
    creds: ApiKeyCreds,
}

/// 市场事件轮询器
///
/// 在每个 tick 中通过 HTTP 拉取最新数据，与上次快照对比，
/// 将增量变化以 Event 形式推送到 broadcast channel。
pub struct MarketEventPoller {
    /// HTTP 客户端
    client: reqwest::Client,
    /// CLOB API host
    host: String,
    /// 事件广播通道
    tx: broadcast::Sender<MarketEvent>,
    /// 上次轮询的快照（用于去重）
    last_trade_ids: RwLock<HashMap<String, HashSet<String>>>,
    last_order_snapshot: RwLock<HashMap<String, Vec<OpenOrderSnapshot>>>,
    /// 轮询统计
    poll_count: AtomicU64,
    event_count: AtomicU64,
    /// 已知结算的市场
    settled_markets: RwLock<HashSet<String>>,
    /// 成交记录账簿（可选）。挂载后 `mark_settled` 会自动回填盈亏。
    fill_ledger: Option<Arc<FillLedger>>,
    /// 自己的钱包地址（P0-013：归属 MAKER 成交时在 maker_orders 里匹配）。
    owner_address: Option<String>,
    /// P0-EXEC-004：L2 认证上下文（未配置时 `/data/*` 会 401）。
    l2_auth: Option<L2Auth>,
}

impl MarketEventPoller {
    /// 创建事件轮询器
    pub fn new(host: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .tcp_nodelay(true)
            .build()
            .expect("Failed to build HTTP client");

        let (tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

        Self {
            client,
            host: host.to_string(),
            tx,
            last_trade_ids: RwLock::new(HashMap::new()),
            last_order_snapshot: RwLock::new(HashMap::new()),
            poll_count: AtomicU64::new(0),
            event_count: AtomicU64::new(0),
            settled_markets: RwLock::new(HashSet::new()),
            fill_ledger: None,
            owner_address: None,
            l2_auth: None,
        }
    }

    /// 挂载成交记录账簿。此后 `mark_settled` 会自动回填该市场所有成交的盈亏。
    pub fn with_fill_ledger(mut self, ledger: Arc<FillLedger>) -> Self {
        self.fill_ledger = Some(ledger);
        self
    }

    /// 挂载自己的钱包地址（P0-013：归属 MAKER 成交时用于匹配 maker_orders）。
    pub fn with_owner_address(mut self, address: String) -> Self {
        self.owner_address = Some(address);
        self
    }

    /// P0-EXEC-004：挂载 L2 认证上下文。`/data/trades`、`/data/orders` 需 L2 HMAC 头，
    /// 未挂载时这些请求将返回 401（由 `tick` 上报为错误）。
    pub fn with_l2_auth(mut self, wallet: LocalWallet, creds: ApiKeyCreds) -> Self {
        self.l2_auth = Some(L2Auth { wallet, creds });
        self
    }

    /// 获取事件订阅者 (broadcast receiver)
    pub fn subscribe(&self) -> broadcast::Receiver<MarketEvent> {
        self.tx.subscribe()
    }

    /// 轮询单次 trades（对指定 token_id）
    pub async fn poll_trades(&self, token_id: &str) -> Result<Vec<TradeEvent>, String> {
        // P0-EXEC-004：/data/trades 需 L2 认证；path 含 query，须一并签名。
        let request_path = format!("/data/trades?token_id={}", token_id);
        let url = format!("{}{}", self.host, request_path);

        let mut req = self.client.get(&url);
        if let Some(auth) = &self.l2_auth {
            let args = L2HeaderArgs {
                method: "GET".to_string(),
                request_path,
                body: None,
            };
            let headers = create_l2_headers(&auth.wallet, &auth.creds, &args, now_secs())
                .map_err(|e| format!("L2 签名失败: {e}"))?;
            for (k, v) in headers {
                req = req.header(k, v);
            }
        }

        let resp = req.send().await.map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "trades API request failed: status={}, body={}",
                status, body
            ));
        }

        let trades: Vec<TradeApiResponse> = resp.json().await.map_err(|e| e.to_string())?;

        let last_ids = self.last_trade_ids.read().unwrap();
        let known = last_ids.get(token_id);
        let mut new_events = Vec::new();

        for t in trades {
            // 成交 identity 缺失时无法安全幂等，拒绝进入真实成交链。
            if t.id.trim().is_empty() {
                tracing::warn!(
                    "market_events: 丢弃缺少 trade_id 的成交响应 token_id={}",
                    token_id
                );
                continue;
            }

            // 去重
            if let Some(known_set) = known {
                if known_set.contains(&t.id) && !t.id.is_empty() {
                    continue;
                }
            }

            // P0-013：按 trader_side 正确归属「我的订单 ID」。无法安全归属则跳过
            //（宁可漏记，不可把成交错误归属到别的订单导致 RiskEngine/FillLedger 错误记账）。
            let order_id = match attribute_order(
                &t.trader_side,
                &t.taker_order_id,
                &t.maker_orders,
                self.owner_address.as_deref(),
            ) {
                Some(oid) => oid,
                None => {
                    tracing::warn!(
                        "market_events: 无法归属成交 trade_id={} trader_side={:?}，跳过",
                        t.id,
                        t.trader_side
                    );
                    continue;
                }
            };

            let price = match parse_f64(&t.price) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("market_events: invalid trade price, trade_id={}, err={}", t.id, e);
                    continue;
                }
            };
            let size = match parse_f64(&t.size) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("market_events: invalid trade size, trade_id={}, err={}", t.id, e);
                    continue;
                }
            };
            let timestamp = match parse_timestamp(&t.created_at) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        "market_events: invalid trade timestamp, trade_id={}, err={}",
                        t.id,
                        e
                    );
                    continue;
                }
            };

            let event = TradeEvent {
                token_id: token_id.to_string(),
                trade_id: t.id.clone(),
                order_id,
                // P0-EXEC-005：顶层 side 是 taker 方向，需按 trader_side 归一到本方视角
                side: own_side(&t.trader_side, &t.side),
                price,
                size,
                timestamp,
                taker_order_id: t.taker_order_id.clone(),
                maker_order_id: t.maker_order_id.clone(),
                trader_side: t.trader_side.clone(),
                owner: t.owner.clone(),
                status: t.status.clone(),
            };

            new_events.push(event);
        }

        // 更新已知 trade ids
        if !new_events.is_empty() {
            drop(last_ids);
            let mut ids = self.last_trade_ids.write().unwrap();
            let entry = ids.entry(token_id.to_string()).or_default();
            for e in &new_events {
                if !e.trade_id.is_empty() {
                    entry.insert(e.trade_id.clone());
                }
            }
            // 限制缓存大小
            if entry.len() > 500 {
                entry.clear(); // 简单截断
            }
        }

        Ok(new_events)
    }

    /// 轮询单次 order book
    pub async fn poll_order_book(&self, token_id: &str) -> Result<BookSnapshot, String> {
        let url = format!("{}/book?token_id={}", self.host, token_id);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let book: OrderBookSummary = resp.json().await.map_err(|e| e.to_string())?;

        let snapshot = BookSnapshot {
            token_id: token_id.to_string(),
            bids: book
                .bids
                .into_iter()
                .map(|b| Level {
                    price: b.price.parse().unwrap_or(0.0),
                    size: b.size.parse().unwrap_or(0.0),
                })
                .collect(),
            asks: book
                .asks
                .into_iter()
                .map(|b| Level {
                    price: b.price.parse().unwrap_or(0.0),
                    size: b.size.parse().unwrap_or(0.0),
                })
                .collect(),
            timestamp: now_secs(),
        };

        Ok(snapshot)
    }

    /// 轮询 open orders
    pub async fn poll_open_orders(&self) -> Result<Vec<OpenOrderSnapshot>, String> {
        // P0-EXEC-004：/data/orders 需 L2 认证。
        let request_path = "/data/orders".to_string();
        let url = format!("{}{}", self.host, request_path);

        let mut req = self.client.get(&url);
        if let Some(auth) = &self.l2_auth {
            let args = L2HeaderArgs {
                method: "GET".to_string(),
                request_path,
                body: None,
            };
            let headers = create_l2_headers(&auth.wallet, &auth.creds, &args, now_secs())
                .map_err(|e| format!("L2 签名失败: {e}"))?;
            for (k, v) in headers {
                req = req.header(k, v);
            }
        }

        let resp = req.send().await.map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "orders API request failed: status={}, body={}",
                status, body
            ));
        }

        let orders: Vec<OpenOrderSnapshot> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(orders)
    }

    /// 执行一轮完整的市场事件轮询（trades + book + orders）
    ///
    /// `token_ids`: 需要监控的市场列表
    ///
    /// P0-EXEC-004：不再静默吞错——每类失败计入 `TickOutcome.poll_errors` 并
    /// `tracing::error!`，调用方据此 FAIL-CLOSED 降级 `TradingGate`。
    pub async fn tick(&self, token_ids: &[String]) -> TickOutcome {
        self.poll_count.fetch_add(1, Ordering::Relaxed);

        let mut outcome = TickOutcome {
            tokens: token_ids.len(),
            ..Default::default()
        };

        if token_ids.is_empty() {
            tracing::warn!(
                "market_events: tick 未配置任何监控 token（token_ids 为空），本轮不会产生事件"
            );
        }

        for tid in token_ids {
            // Trades
            match self.poll_trades(tid).await {
                Ok(trades) => {
                    for t in trades {
                        if self.tx.send(MarketEvent::Trade(t)).is_err() {
                            tracing::warn!(
                                "market_events: Trade 事件无订阅者，已丢弃（需 StateSync 兜底） token_id={}",
                                tid
                            );
                        }
                        self.event_count.fetch_add(1, Ordering::Relaxed);
                        outcome.events += 1;
                    }
                }
                Err(e) => {
                    outcome.poll_errors += 1;
                    tracing::error!("market_events: poll_trades 失败 token_id={}: {}", tid, e);
                }
            }

            // Order book
            match self.poll_order_book(tid).await {
                Ok(book) => {
                    let _ = self.tx.send(MarketEvent::OrderBookUpdate(book));
                    self.event_count.fetch_add(1, Ordering::Relaxed);
                    outcome.events += 1;
                }
                Err(e) => {
                    outcome.poll_errors += 1;
                    tracing::warn!(
                        "market_events: poll_order_book 失败 token_id={}: {}",
                        tid,
                        e
                    );
                }
            }
        }

        // Open orders
        match self.poll_open_orders().await {
            Ok(orders) => {
                let prev = self.last_order_snapshot.read().unwrap().clone();
                let current: HashMap<String, Vec<OpenOrderSnapshot>> =
                    orders.into_iter().fold(HashMap::new(), |mut acc, o| {
                        acc.entry(o.asset_id.clone()).or_default().push(o);
                        acc
                    });

                // 比对变更
                for (tid, orders) in &current {
                    let prev_orders = prev.get(tid);
                    for o in orders {
                        let changed = prev_orders.is_none_or(|po| {
                            !po.iter().any(|p| {
                                p.id == o.id
                                    && p.status == o.status
                                    && p.size_matched == o.size_matched
                            })
                        });
                        if changed {
                            let event = OrderStateEvent {
                                order_id: o.id.clone(),
                                token_id: tid.clone(),
                                status: o.status.clone(),
                                filled_size: o.size_matched.parse().unwrap_or(0.0),
                                remaining_size: o.size.parse().unwrap_or(0.0),
                                avg_price: o.price.parse().ok(),
                            };
                            let _ = self.tx.send(MarketEvent::OrderStateChange(event));
                            self.event_count.fetch_add(1, Ordering::Relaxed);
                            outcome.events += 1;
                        }
                    }
                }

                *self.last_order_snapshot.write().unwrap() = current;
            }
            Err(e) => {
                outcome.poll_errors += 1;
                tracing::error!("market_events: poll_open_orders 失败: {}", e);
            }
        }

        outcome
    }

    /// 标记市场已结算
    pub fn mark_settled(&self, token_id: &str, outcome: f64) {
        let mut settled = self.settled_markets.write().unwrap();
        if settled.insert(token_id.to_string()) {
            // 自动回填成交记录的盈亏（若已挂载账簿）
            if let Some(ref ledger) = self.fill_ledger {
                let n = ledger.settle_by_token(token_id, outcome);
                if n > 0 {
                    tracing::info!(
                        "market_events: 市场 {} 结算 outcome={}, 回填 {} 条成交盈亏",
                        token_id,
                        outcome,
                        n
                    );
                }
            }

            let event = SettlementEvent {
                token_id: token_id.to_string(),
                outcome,
                settled_at: now_secs(),
                uma_proposal: false,
            };
            let _ = self.tx.send(MarketEvent::Settlement(event));
            self.event_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 统计
    pub fn stats(&self) -> EventStats {
        EventStats {
            poll_count: self.poll_count.load(Ordering::Relaxed),
            event_count: self.event_count.load(Ordering::Relaxed),
            monitored_markets: self.last_trade_ids.read().unwrap().len(),
            settled_markets: self.settled_markets.read().unwrap().len(),
        }
    }

    /// HTTP 客户端引用
    pub fn http_client(&self) -> &reqwest::Client {
        &self.client
    }

    /// CLOB Host
    pub fn host(&self) -> &str {
        &self.host
    }

    /// P0-EXEC-004：从 MarketDiscovery 注册表 JSON 读取需监控的 token 列表（YES+NO）。
    ///
    /// 注册表格式：`{ "<condition_id>": { "yes_token_id":..., "no_token_id":..., ... }, ... }`
    /// （见 Python `market_discovery/registry.py`，默认 `./data/market_registry.json`）。
    /// 文件不存在/解析失败时返回 `Err`，由调用方决定处置（server 侧选择告警 + 空列表）。
    pub fn load_token_ids_from_registry(path: &str) -> Result<Vec<String>, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("读取注册表 {path} 失败: {e}"))?;
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("解析注册表 {path} 失败: {e}"))?;
        let obj = value
            .as_object()
            .ok_or_else(|| format!("注册表 {path} 顶层不是对象"))?;
        let mut ids = Vec::new();
        for rec in obj.values() {
            for key in ["yes_token_id", "no_token_id"] {
                if let Some(s) = rec.get(key).and_then(|v| v.as_str()) {
                    if !s.trim().is_empty() {
                        ids.push(s.to_string());
                    }
                }
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }
}

/// 单轮 `tick` 结果（P0-EXEC-004：供调用方判断是否 FAIL-CLOSED 降级）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickOutcome {
    /// 本轮配置监控的 token 数
    pub tokens: usize,
    /// 轮询失败次数（trades/book/orders 任一失败各计一次）
    pub poll_errors: usize,
    /// 本轮产生的事件数
    pub events: usize,
}

impl TickOutcome {
    /// 出现任何轮询失败即应降级（FAIL-CLOSED）。
    pub fn should_degrade(&self) -> bool {
        self.poll_errors > 0
    }
}

// ============================================================================
// EventStats
// ============================================================================

#[derive(Debug, Clone, Default)]
pub struct EventStats {
    pub poll_count: u64,
    pub event_count: u64,
    pub monitored_markets: usize,
    pub settled_markets: usize,
}

// ============================================================================
// 工具函数
// ============================================================================

/// P0-013：按 `trader_side` 正确归属「我的订单 ID」。
///
/// - `TAKER` → 返回 `taker_order_id`
/// - `MAKER` → 在 `maker_orders` 里匹配 `owner_address` 的 order_id；未配地址或匹配不到时，
///   仅当 `maker_orders` 只有一条才兜底取它，否则返回 `None`
/// - 未知 `trader_side` → 返回 `None`
///
/// 返回 `None` 表示无法安全归属，调用方应跳过该成交（避免错误记账）。
fn attribute_order(
    trader_side: &str,
    taker_order_id: &str,
    maker_orders: &[MakerOrderRef],
    owner_address: Option<&str>,
) -> Option<String> {
    let order_id = match trader_side {
        "TAKER" => {
            if taker_order_id.is_empty() {
                return None;
            }
            Some(taker_order_id.to_string())
        }
        "MAKER" => {
            if let Some(addr) = owner_address.filter(|a| !a.is_empty()) {
                maker_orders
                    .iter()
                    .find(|m| m.maker_address == addr)
                    .map(|m| m.order_id.clone())
                    .or_else(|| {
                        // 兜底：地址未匹配，但只有一个 maker 时取它
                        if maker_orders.len() == 1 {
                            maker_orders.first().map(|m| m.order_id.clone())
                        } else {
                            None
                        }
                    })
            } else if maker_orders.len() == 1 {
                maker_orders.first().map(|m| m.order_id.clone())
            } else {
                None
            }
        }
        _ => return None,
    };
    // 兜底：maker_orders 里 order_id 字段缺失（空串）时，视作无法安全归属
    order_id.filter(|oid| !oid.is_empty())
}

/// P0-EXEC-005：把成交方向归一化到**本方视角**。
///
/// Polymarket 顶层 `side` 是 taker 方向；当 `trader_side == "MAKER"` 时，
/// 本方（maker）方向与其相反（taker 买 = maker 卖）。TAKER 成交保持原方向。
/// 非 BUY/SELL 的未知值原样返回，交由下游校验。
fn own_side(trader_side: &str, taker_side: &str) -> String {
    if trader_side.eq_ignore_ascii_case("MAKER") {
        match taker_side.to_ascii_uppercase().as_str() {
            "BUY" => "SELL".to_string(),
            "SELL" => "BUY".to_string(),
            _ => taker_side.to_string(),
        }
    } else {
        taker_side.to_string()
    }
}

/// 解析成交价/数量：字符串或数字 → f64。
///
/// 失败（非字符串/数字、非法字符串、非有限数）返回 `Err`，**绝不静默转 0.0**
/// —— 否则会把 `"abc"`/`null` 伪造成合法的 0 价成交，绕过 P0-009 数值安全。
fn parse_f64(v: &serde_json::Value) -> Result<f64, String> {
    let value = match v {
        serde_json::Value::String(s) => s
            .parse::<f64>()
            .map_err(|_| format!("invalid float string: {s}"))?,
        serde_json::Value::Number(n) => n
            .as_f64()
            .ok_or_else(|| "invalid JSON number".to_string())?,
        _ => return Err("expected string or number".to_string()),
    };
    if !value.is_finite() {
        return Err("non-finite float".to_string());
    }
    Ok(value)
}

/// 解析成交时间戳：字符串 → u64。
///
/// 失败返回 `Err`，不静默转成 Unix epoch 0。
fn parse_timestamp(s: &str) -> Result<u64, String> {
    s.parse::<u64>().map_err(|_| format!("invalid timestamp: {s}"))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_f64_string() {
        let v = serde_json::json!("0.55");
        assert!((parse_f64(&v).unwrap() - 0.55).abs() < 0.001);
    }

    #[test]
    fn test_parse_f64_number() {
        let v = serde_json::json!(0.72);
        assert!((parse_f64(&v).unwrap() - 0.72).abs() < 0.001);
    }

    /// P0-009 数值安全：非法数据不得静默转 0.0，必须返回 Err。
    #[test]
    fn test_parse_f64_invalid_string_returns_err() {
        assert!(parse_f64(&serde_json::json!("abc")).is_err());
    }

    #[test]
    fn test_parse_f64_null_returns_err() {
        assert!(parse_f64(&serde_json::Value::Null).is_err());
    }

    #[test]
    fn test_parse_f64_non_finite_returns_err() {
        // serde_json 的 Number 无法表示 NaN/Inf，用字符串 "NaN" 触发有限性检查
        assert!(parse_f64(&serde_json::json!("NaN")).is_err());
    }

    #[test]
    fn test_parse_timestamp_invalid_returns_err() {
        assert!(parse_timestamp("abc").is_err());
        assert!(parse_timestamp("").is_err());
    }

    #[test]
    fn test_event_stats_default() {
        let s = EventStats::default();
        assert_eq!(s.poll_count, 0);
        assert_eq!(s.event_count, 0);
    }

    #[test]
    fn test_poller_creation() {
        let poller = MarketEventPoller::new("https://clob.polymarket.com");
        let rx = poller.subscribe();
        assert!(rx.is_empty()); // Initially empty
    }

    /// P1-035：poll_trades 必须丢弃空/空白 trade_id，只放行合法成交。
    #[tokio::test]
    async fn test_poll_trades_drops_empty_trade_id() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!([
            {
                "id": "",
                "side": "BUY",
                "price": "0.5",
                "size": "10",
                "createdAt": "1700000000",
                "takerOrderId": "o-empty",
                "traderSide": "TAKER"
            },
            {
                "id": "   ",
                "side": "BUY",
                "price": "0.5",
                "size": "10",
                "createdAt": "1700000000",
                "takerOrderId": "o-blank",
                "traderSide": "TAKER"
            },
            {
                "id": "t-valid",
                "side": "BUY",
                "price": "0.5",
                "size": "10",
                "createdAt": "1700000000",
                "takerOrderId": "o-valid",
                "traderSide": "TAKER"
            }
        ]);

        Mock::given(method("GET"))
            .and(path("/data/trades"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let poller = MarketEventPoller::new(&server.uri());
        let events = poller.poll_trades("tok-1").await.unwrap();

        assert_eq!(events.len(), 1, "仅合法 trade_id 的成交应被保留");
        assert_eq!(events[0].trade_id, "t-valid");
        assert_eq!(events[0].order_id, "o-valid");
    }

    /// P0-EXEC-005：顶层 `side` 是 taker 方向；MAKER 成交须取反到本方视角。
    #[test]
    fn test_own_side_maker_inverts() {
        assert_eq!(own_side("MAKER", "BUY"), "SELL");
        assert_eq!(own_side("MAKER", "SELL"), "BUY");
        assert_eq!(own_side("maker", "buy"), "SELL");
    }

    /// P0-EXEC-005：TAKER 成交方向不变；未知 trader_side 不取反。
    #[test]
    fn test_own_side_taker_unchanged() {
        assert_eq!(own_side("TAKER", "BUY"), "BUY");
        assert_eq!(own_side("TAKER", "SELL"), "SELL");
        assert_eq!(own_side("", "BUY"), "BUY");
    }

    /// P0-EXEC-005：MAKER 成交的 TradeEvent.side 必须是本方方向。
    #[tokio::test]
    async fn test_poll_trades_maker_side_inverted() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!([
            {
                "id": "t-mk",
                "side": "SELL",
                "price": "0.5",
                "size": "10",
                "createdAt": "1700000000",
                "takerOrderId": "o-taker",
                "traderSide": "MAKER",
                "makerOrders": [
                    {"orderId": "o-mine", "makerAddress": "0xMe"},
                    {"orderId": "o-other", "makerAddress": "0xOther"}
                ]
            }
        ]);

        Mock::given(method("GET"))
            .and(path("/data/trades"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let poller = MarketEventPoller::new(&server.uri()).with_owner_address("0xMe".to_string());
        let events = poller.poll_trades("tok-1").await.unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].order_id, "o-mine");
        assert_eq!(events[0].trader_side, "MAKER");
        assert_eq!(events[0].side, "BUY", "maker 成交方向应取反（taker SELL → 本方 BUY）");
    }

    /// P0-EXEC-004：构造测试用 L2 认证上下文（不联网）。
    fn test_l2_auth() -> (LocalWallet, ApiKeyCreds) {
        let wallet: LocalWallet =
            "bd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b"
                .parse()
                .unwrap();
        let creds = ApiKeyCreds {
            key: "k".into(),
            secret: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
            passphrase: "p".into(),
        };
        (wallet, creds)
    }

    /// P0-EXEC-004：`/data/trades` 请求必须携带完整 L2 HMAC 头（否则线上 401）。
    #[tokio::test]
    async fn test_poll_trades_attaches_l2_headers() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/data/trades"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let (wallet, creds) = test_l2_auth();
        let poller = MarketEventPoller::new(&server.uri()).with_l2_auth(wallet, creds);
        let events = poller.poll_trades("tok-1").await.unwrap();
        assert!(events.is_empty());

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        for name in [
            "poly_address",
            "poly_signature",
            "poly_timestamp",
            "poly_api_key",
            "poly_passphrase",
        ] {
            assert!(
                requests[0].headers.contains_key(name),
                "缺少 L2 认证头 {}",
                name
            );
        }
    }

    /// P0-EXEC-004：轮询失败必须上报（不再静默），并触发降级判定。
    #[tokio::test]
    async fn test_tick_reports_errors_and_degrades_on_poll_failure() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/data/trades"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        // book / orders 正常，隔离出「仅 trades 失败」场景
        Mock::given(method("GET"))
            .and(path("/book"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "market": "m",
                "asset_id": "tok-1",
                "timestamp": "0",
                "bids": [],
                "asks": [],
                "min_order_size": "1",
                "tick_size": "0.01",
                "neg_risk": false
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/data/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let poller = MarketEventPoller::new(&server.uri());
        let outcome = poller.tick(&["tok-1".to_string()]).await;

        assert_eq!(outcome.tokens, 1);
        assert_eq!(
            outcome.poll_errors, 1,
            "仅 trades 失败应恰好上报 1 次错误，不能静默或重复计数"
        );
        assert!(outcome.should_degrade(), "轮询失败应触发 FAIL-CLOSED 降级");
    }

    /// P0-EXEC-004：tick 必须按传入的 token 列表轮询（而非恒空列表）。
    #[tokio::test]
    async fn test_tick_uses_configured_token_ids() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/data/trades"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let poller = MarketEventPoller::new(&server.uri());
        let tokens = vec!["tok-a".to_string(), "tok-b".to_string()];
        let outcome = poller.tick(&tokens).await;
        assert_eq!(outcome.tokens, 2);

        let reqs = server.received_requests().await.unwrap();
        let trade_queries: Vec<String> = reqs
            .iter()
            .filter(|r| r.url.path() == "/data/trades")
            .map(|r| r.url.query().unwrap_or_default().to_string())
            .collect();
        assert!(
            trade_queries.iter().any(|q| q.contains("token_id=tok-a")),
            "未轮询 tok-a: {:?}",
            trade_queries
        );
        assert!(
            trade_queries.iter().any(|q| q.contains("token_id=tok-b")),
            "未轮询 tok-b: {:?}",
            trade_queries
        );
    }

    /// P0-EXEC-004：从 MarketDiscovery 注册表 JSON 提取 YES/NO token 列表。
    #[test]
    fn test_load_token_ids_from_registry() {
        let path = std::env::temp_dir().join("wb_registry_test.json");
        let body = serde_json::json!({
            "c1": {"yes_token_id": "y1", "no_token_id": "n1"},
            "c2": {"yes_token_id": "y2", "no_token_id": "n2"}
        });
        std::fs::write(&path, body.to_string()).unwrap();

        let ids = MarketEventPoller::load_token_ids_from_registry(path.to_str().unwrap()).unwrap();
        assert_eq!(ids, vec!["n1", "n2", "y1", "y2"]);

        // 缺失文件 → Err（调用方 fail-closed 处置）
        let missing = std::env::temp_dir().join("wb_registry_missing_xyz.json");
        assert!(MarketEventPoller::load_token_ids_from_registry(missing.to_str().unwrap()).is_err());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_mark_settled() {
        let poller = MarketEventPoller::new("https://example.com");
        poller.mark_settled("token-1", 1.0);
        let stats = poller.stats();
        assert_eq!(stats.settled_markets, 1);
    }

    #[test]
    fn test_mark_settled_backfills_pnl() {
        use crate::fast::fill_records::{FillLedger, FillRecord, FillStatus};

        let ledger = Arc::new(FillLedger::new());
        let mut r = FillRecord::new(
            "o1".to_string(),
            "token-1".to_string(),
            "BUY".to_string(),
            0.5,
            10.0,
        );
        r.mark_filled(0.5, 0.0);
        ledger.record(r);

        let poller =
            MarketEventPoller::new("https://example.com").with_fill_ledger(ledger.clone());
        poller.mark_settled("token-1", 1.0);

        // 结算回填：BUY 10 @ 0.5 结算 1.0 → +5.0
        let rec = ledger.get("o1").unwrap();
        assert_eq!(rec.status, FillStatus::Settled);
        assert!((rec.pnl.unwrap() - 5.0).abs() < 1e-9);
    }

    // ── P0-013：Trade 归属 attribution ──

    fn mkr(order_id: &str, maker_address: &str) -> MakerOrderRef {
        MakerOrderRef {
            order_id: order_id.to_string(),
            maker_address: maker_address.to_string(),
        }
    }

    #[test]
    fn test_attribute_order_taker() {
        let oid = attribute_order("TAKER", "taker-1", &[], None);
        assert_eq!(oid.as_deref(), Some("taker-1"));
    }

    #[test]
    fn test_attribute_order_taker_empty_returns_none() {
        assert_eq!(attribute_order("TAKER", "", &[], None), None);
    }

    /// P0-016/审计：maker_orders 里 order_id 为空串时，不能产生 Some("")，须返回 None。
    #[test]
    fn test_attribute_order_maker_empty_order_id_returns_none() {
        let makers = vec![mkr("", "0xAAA")];
        assert_eq!(attribute_order("MAKER", "taker-1", &makers, None), None);
        assert_eq!(attribute_order("MAKER", "taker-1", &makers, Some("0xAAA")), None);
    }

    #[test]
    fn test_attribute_order_maker_single_fallback() {
        let makers = vec![mkr("maker-1", "0xmaker1")];
        assert_eq!(
            attribute_order("MAKER", "taker-1", &makers, None).as_deref(),
            Some("maker-1")
        );
    }

    #[test]
    fn test_attribute_order_maker_match_owner() {
        let makers = vec![mkr("maker-a", "0xAAA"), mkr("maker-b", "0xme")];
        assert_eq!(
            attribute_order("MAKER", "taker-1", &makers, Some("0xme")).as_deref(),
            Some("maker-b")
        );
    }

    #[test]
    fn test_attribute_order_maker_multi_no_match_returns_none() {
        let makers = vec![mkr("maker-a", "0xAAA"), mkr("maker-b", "0xBBB")];
        assert_eq!(attribute_order("MAKER", "taker-1", &makers, Some("0xme")), None);
        assert_eq!(attribute_order("MAKER", "taker-1", &makers, None), None);
    }

    #[test]
    fn test_attribute_order_unknown_side_returns_none() {
        assert_eq!(attribute_order("UNKNOWN", "taker-1", &[], None), None);
    }
}
