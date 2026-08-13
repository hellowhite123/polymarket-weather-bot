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

use crate::fast::fill_records::FillLedger;
use crate::types::clob_types::OrderBookSummary;
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
    pub trade_id: String,
    /// 统一订单 ID（taker 侧订单 ID，主动成交方）
    pub order_id: String,
    pub side: String, // BUY / SELL
    pub price: f64,
    pub size: f64,
    pub timestamp: u64,
    pub taker_order_id: String,
    pub maker_order_id: String,
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
}

// ============================================================================
// OpenOrderSnapshot — CLOB API 返回的开仓订单结构
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
struct OpenOrderSnapshot {
    #[serde(default)]
    id: String,
    #[serde(default)]
    asset_id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    #[serde(rename = "size_matched")]
    size_matched: String,
    #[serde(default)]
    size: String,
    #[serde(default)]
    price: String,
}

// ============================================================================
// MarketEventPoller
// ============================================================================

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
        }
    }

    /// 挂载成交记录账簿。此后 `mark_settled` 会自动回填该市场所有成交的盈亏。
    pub fn with_fill_ledger(mut self, ledger: Arc<FillLedger>) -> Self {
        self.fill_ledger = Some(ledger);
        self
    }

    /// 获取事件订阅者 (broadcast receiver)
    pub fn subscribe(&self) -> broadcast::Receiver<MarketEvent> {
        self.tx.subscribe()
    }

    /// 轮询单次 trades（对指定 token_id）
    pub async fn poll_trades(&self, token_id: &str) -> Result<Vec<TradeEvent>, String> {
        let url = format!("{}/data/trades?token_id={}", self.host, token_id);
        let resp = self.client.get(&url).send().await.map_err(|e| e.to_string())?;

        let trades: Vec<TradeApiResponse> = resp.json().await.map_err(|e| e.to_string())?;

        let last_ids = self.last_trade_ids.read().unwrap();
        let known = last_ids.get(token_id);
        let mut new_events = Vec::new();

        for t in trades {
            // 去重
            if let Some(known_set) = known {
                if known_set.contains(&t.id) && !t.id.is_empty() {
                    continue;
                }
            }

            let price = parse_f64(&t.price);
            let size = parse_f64(&t.size);

            let event = TradeEvent {
                token_id: token_id.to_string(),
                trade_id: t.id.clone(),
                order_id: t.taker_order_id.clone(),
                side: t.side,
                price,
                size,
                timestamp: parse_timestamp(&t.created_at),
                taker_order_id: t.taker_order_id,
                maker_order_id: t.maker_order_id,
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
        let resp = self.client.get(&url).send().await.map_err(|e| e.to_string())?;
        let book: OrderBookSummary = resp.json().await.map_err(|e| e.to_string())?;

        let snapshot = BookSnapshot {
            token_id: token_id.to_string(),
            bids: book.bids.into_iter().map(|b| Level {
                price: b.price.parse().unwrap_or(0.0),
                size: b.size.parse().unwrap_or(0.0),
            }).collect(),
            asks: book.asks.into_iter().map(|b| Level {
                price: b.price.parse().unwrap_or(0.0),
                size: b.size.parse().unwrap_or(0.0),
            }).collect(),
            timestamp: now_secs(),
        };

        Ok(snapshot)
    }

    /// 轮询 open orders
    pub async fn poll_open_orders(&self) -> Result<Vec<OpenOrderSnapshot>, String> {
        let url = format!("{}/data/orders", self.host);
        let resp = self.client.get(&url).send().await.map_err(|e| e.to_string())?;
        let orders: Vec<OpenOrderSnapshot> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(orders)
    }

    /// 执行一轮完整的市场事件轮询（trades + book + orders）
    ///
    /// `token_ids`: 需要监控的市场列表
    pub async fn tick(&self, token_ids: &[String]) {
        self.poll_count.fetch_add(1, Ordering::Relaxed);

        for tid in token_ids {
            // Trades
            if let Ok(trades) = self.poll_trades(tid).await {
                for t in trades {
                    let _ = self.tx.send(MarketEvent::Trade(t));
                    self.event_count.fetch_add(1, Ordering::Relaxed);
                }
            }

            // Order book
            if let Ok(book) = self.poll_order_book(tid).await {
                let _ = self.tx.send(MarketEvent::OrderBookUpdate(book));
                self.event_count.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Open orders
        if let Ok(orders) = self.poll_open_orders().await {
            let prev = self.last_order_snapshot.read().unwrap().clone();
            let current: HashMap<String, Vec<OpenOrderSnapshot>> = orders
                .into_iter()
                .fold(HashMap::new(), |mut acc, o| {
                    acc.entry(o.asset_id.clone()).or_default().push(o);
                    acc
                });

            // 比对变更
            for (tid, orders) in &current {
                let prev_orders = prev.get(tid);
                for o in orders {
                    let changed = prev_orders.map_or(true, |po| {
                        !po.iter().any(|p| p.id == o.id && p.status == o.status && p.size_matched == o.size_matched)
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
                    }
                }
            }

            *self.last_order_snapshot.write().unwrap() = current;
        }
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
                        token_id, outcome, n
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

fn parse_f64(v: &serde_json::Value) -> f64 {
    match v {
        serde_json::Value::String(s) => s.parse().unwrap_or(0.0),
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn parse_timestamp(s: &str) -> u64 {
    s.parse().unwrap_or(0)
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
        assert!((parse_f64(&v) - 0.55).abs() < 0.001);
    }

    #[test]
    fn test_parse_f64_number() {
        let v = serde_json::json!(0.72);
        assert!((parse_f64(&v) - 0.72).abs() < 0.001);
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
        assert!(!rx.is_empty() == false); // Initially empty
    }

    #[test]
    fn test_mark_settled() {
        let poller = MarketEventPoller::new("https://example.com");
        poller.mark_settled("token-1", 1.0);
        let stats = poller.stats();
        assert_eq!(stats.settled_markets, 1);
    }
}
