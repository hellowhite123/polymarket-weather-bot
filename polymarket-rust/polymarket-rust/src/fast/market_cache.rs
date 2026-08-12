//! 市场元数据缓存 —— 消除下单前的额外网络往返
//!
//! ## 问题
//!
//! `OrderFlow::place_order` 在 `tick_size` 未显式提供时，会先发一次
//! `GET /tick-size` 再下单：
//!
//! ```text
//! 信号 → [GET /tick-size  ~1 RTT] → [POST /order  ~1 RTT] → 成交
//!         ^^^^^^^^^^^^^^^^^^^^^^^ 白白多花一个往返
//! ```
//!
//! 跨太平洋链路一个 RTT 就 150~250ms，这一步直接让 100ms 目标失守。
//!
//! ## 方案
//!
//! `tick_size` 与 `neg_risk` 是市场的**静态属性**，上市后不再变化。
//! 启动时批量拉取并缓存，下单时零网络开销直接命中。

use crate::types::enums::TickSize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// 市场静态元数据
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketMeta {
    pub tick_size: TickSize,
    pub neg_risk: bool,
}

impl Default for MarketMeta {
    fn default() -> Self {
        Self {
            tick_size: TickSize::Point01,
            neg_risk: false,
        }
    }
}

struct Entry {
    meta: MarketMeta,
    inserted_ms: u64,
}

/// 市场元数据缓存
///
/// 默认 TTL 6 小时——元数据实质不变，设 TTL 只是为了容忍极端情况下的
/// 市场重建，避免进程长跑时缓存永久失准。
pub struct MarketMetaCache {
    entries: RwLock<HashMap<String, Entry>>,
    epoch: Instant,
    ttl_ms: u64,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl MarketMetaCache {
    pub fn new() -> Self {
        Self::with_ttl(Duration::from_secs(6 * 60 * 60))
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            epoch: Instant::now(),
            ttl_ms: ttl.as_millis() as u64,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    #[inline]
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// 查询缓存
    pub fn get(&self, token_id: &str) -> Option<MarketMeta> {
        let now = self.now_ms();
        if let Ok(guard) = self.entries.read() {
            if let Some(e) = guard.get(token_id) {
                if now.saturating_sub(e.inserted_ms) <= self.ttl_ms {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    return Some(e.meta);
                }
            }
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// 写入缓存
    pub fn put(&self, token_id: &str, meta: MarketMeta) {
        let now = self.now_ms();
        if let Ok(mut guard) = self.entries.write() {
            guard.insert(
                token_id.to_string(),
                Entry {
                    meta,
                    inserted_ms: now,
                },
            );
        }
    }

    /// 批量写入（预热用）
    pub fn put_many<I: IntoIterator<Item = (String, MarketMeta)>>(&self, items: I) {
        let now = self.now_ms();
        if let Ok(mut guard) = self.entries.write() {
            for (k, meta) in items {
                guard.insert(
                    k,
                    Entry {
                        meta,
                        inserted_ms: now,
                    },
                );
            }
        }
    }

    /// 命中则返回，否则用 `fetch` 拉取后缓存
    ///
    /// `fetch` 只在未命中时被调用，因此热路径上不会产生任何网络请求。
    pub async fn get_or_fetch<F, Fut, E>(&self, token_id: &str, fetch: F) -> Result<MarketMeta, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<MarketMeta, E>>,
    {
        if let Some(m) = self.get(token_id) {
            return Ok(m);
        }
        let meta = fetch().await?;
        self.put(token_id, meta);
        Ok(meta)
    }

    /// 移除某个市场
    pub fn invalidate(&self, token_id: &str) {
        if let Ok(mut g) = self.entries.write() {
            g.remove(token_id);
        }
    }

    /// 清空
    pub fn clear(&self) {
        if let Ok(mut g) = self.entries.write() {
            g.clear();
        }
    }

    /// 已缓存的市场数量
    pub fn len(&self) -> usize {
        self.entries.read().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// (hits, misses)
    pub fn stats(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }

    /// 命中率
    pub fn hit_rate(&self) -> f64 {
        let (h, m) = self.stats();
        let t = h + m;
        if t == 0 {
            0.0
        } else {
            h as f64 / t as f64
        }
    }
}

impl Default for MarketMetaCache {
    fn default() -> Self {
        Self::new()
    }
}

/// 解析 tick size 字符串
pub fn parse_tick_size(s: &str) -> Option<TickSize> {
    match s.trim() {
        "0.1" => Some(TickSize::Point1),
        "0.01" => Some(TickSize::Point01),
        "0.001" => Some(TickSize::Point001),
        "0.0001" => Some(TickSize::Point0001),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tick_size() {
        assert_eq!(parse_tick_size("0.1"), Some(TickSize::Point1));
        assert_eq!(parse_tick_size(" 0.01 "), Some(TickSize::Point01));
        assert_eq!(parse_tick_size("0.001"), Some(TickSize::Point001));
        assert_eq!(parse_tick_size("0.0001"), Some(TickSize::Point0001));
        assert_eq!(parse_tick_size("x"), None);
    }

    #[test]
    fn test_put_get() {
        let c = MarketMetaCache::new();
        assert!(c.get("t1").is_none());

        let meta = MarketMeta {
            tick_size: TickSize::Point001,
            neg_risk: true,
        };
        c.put("t1", meta);
        assert_eq!(c.get("t1"), Some(meta));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn test_ttl_expiry() {
        let c = MarketMetaCache::with_ttl(Duration::from_millis(40));
        c.put("t1", MarketMeta::default());
        assert!(c.get("t1").is_some());
        std::thread::sleep(Duration::from_millis(70));
        assert!(c.get("t1").is_none(), "过期后不应命中");
    }

    #[test]
    fn test_put_many_and_clear() {
        let c = MarketMetaCache::new();
        c.put_many(vec![
            ("a".to_string(), MarketMeta::default()),
            (
                "b".to_string(),
                MarketMeta {
                    tick_size: TickSize::Point1,
                    neg_risk: true,
                },
            ),
        ]);
        assert_eq!(c.len(), 2);
        assert_eq!(c.get("b").unwrap().tick_size, TickSize::Point1);
        c.clear();
        assert!(c.is_empty());
    }

    #[test]
    fn test_invalidate() {
        let c = MarketMetaCache::new();
        c.put("t1", MarketMeta::default());
        c.invalidate("t1");
        assert!(c.get("t1").is_none());
    }

    #[tokio::test]
    async fn test_get_or_fetch_only_fetches_once() {
        let c = MarketMetaCache::new();
        let calls = std::sync::Arc::new(AtomicU64::new(0));

        for _ in 0..5 {
            let calls2 = calls.clone();
            let m: Result<MarketMeta, ()> = c
                .get_or_fetch("tok", || async move {
                    calls2.fetch_add(1, Ordering::SeqCst);
                    Ok(MarketMeta {
                        tick_size: TickSize::Point001,
                        neg_risk: false,
                    })
                })
                .await;
            assert_eq!(m.unwrap().tick_size, TickSize::Point001);
        }

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "5 次调用只应触发 1 次网络拉取"
        );
    }

    #[tokio::test]
    async fn test_get_or_fetch_propagates_error() {
        let c = MarketMetaCache::new();
        let r: Result<MarketMeta, &str> = c.get_or_fetch("x", || async { Err("boom") }).await;
        assert_eq!(r, Err("boom"));
        assert!(c.get("x").is_none(), "失败不应写入缓存");
    }

    #[test]
    fn test_hit_rate() {
        let c = MarketMetaCache::new();
        c.put("t", MarketMeta::default());
        for _ in 0..9 {
            let _ = c.get("t");
        }
        let _ = c.get("missing");
        assert_eq!(c.stats(), (9, 1));
        assert!((c.hit_rate() - 0.9).abs() < 1e-9);
    }
}
