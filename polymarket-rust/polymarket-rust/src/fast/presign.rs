//! 预签名订单池 —— 把签名成本彻底移出关键路径
//!
//! ## 思路
//!
//! 即使消除了 PBKDF2，热路径上仍有 keccak256 + secp256k1 ECDSA 签名
//! （数十到数百微秒）。对追求极限延迟的场景，可以在**信号到达之前**
//! 就把候选订单签好放进池子：
//!
//! ```text
//! 常规:  信号 → 构建 → 签名 → HMAC → POST
//! 预签:  ┌ 空闲期: 构建 + 签名 (离线)
//!        └ 信号 → take() → HMAC → POST
//! ```
//!
//! ## 关键约束：salt 唯一性
//!
//! 每个订单携带随机 `salt`，交易所据此去重。**同一份预签名订单绝不能
//! 提交两次**，否则第二次会被判为重复订单而拒绝。因此本池的取用语义是
//! `take`（取走即移除），而非 `get`（读取）。
//!
//! ## 有效期
//!
//! 预签名订单锁定了具体的价格与数量。行情走远后这些订单就失去意义，
//! 甚至会造成不利成交，所以必须设 TTL 并定期清理。

use crate::types::clob_types::SignedOrder;
use crate::types::enums::Side;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// 预签名订单默认有效期
pub const DEFAULT_PRESIGN_TTL: Duration = Duration::from_secs(60);

/// 一份预签名订单
#[derive(Debug, Clone)]
pub struct PreSignedOrder {
    pub token_id: String,
    pub side: Side,
    /// 价格（0~1）
    pub price: f64,
    /// 数量（shares）
    pub size: f64,
    /// 已签名的订单体
    pub signed: SignedOrder,
    /// 生成时刻（相对池 epoch 的毫秒数）
    created_ms: u64,
}

impl PreSignedOrder {
    pub fn new(
        token_id: impl Into<String>,
        side: Side,
        price: f64,
        size: f64,
        signed: SignedOrder,
    ) -> Self {
        Self {
            token_id: token_id.into(),
            side,
            price,
            size,
            signed,
            created_ms: 0,
        }
    }

    /// 订单的 salt，用于唯一性校验
    pub fn salt(&self) -> &str {
        &self.signed.order.salt
    }
}

/// 池内分桶键：(token_id, side)
type BucketKey = (String, u8);

/// 把价格量化为整数刻度，避免浮点做 HashMap 键
#[inline]
fn price_ticks(price: f64) -> i64 {
    // 统一按 1e-4 精度量化，覆盖 Polymarket 全部 tick size
    (price * 10_000.0).round() as i64
}

/// 预签名订单池统计
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreSignStats {
    /// 成功取用次数
    pub takes: u64,
    /// 取用未命中次数（回退到实时签名）
    pub take_misses: u64,
    /// 因过期被清理的数量
    pub expired: u64,
    /// 当前池内订单数
    pub available: usize,
}

impl PreSignStats {
    /// 预签名覆盖率：有多少比例的下单走到了快路径
    pub fn coverage(&self) -> f64 {
        let t = self.takes + self.take_misses;
        if t == 0 {
            0.0
        } else {
            self.takes as f64 / t as f64
        }
    }
}

/// 预签名订单池
///
/// 按 `(token_id, side)` 分桶，桶内按价格刻度索引。
pub struct PreSignedPool {
    buckets: RwLock<HashMap<BucketKey, HashMap<i64, Vec<PreSignedOrder>>>>,
    epoch: Instant,
    ttl_ms: u64,
    takes: AtomicU64,
    take_misses: AtomicU64,
    expired: AtomicU64,
}

impl PreSignedPool {
    pub fn new() -> Self {
        Self::with_ttl(DEFAULT_PRESIGN_TTL)
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            buckets: RwLock::new(HashMap::new()),
            epoch: Instant::now(),
            ttl_ms: ttl.as_millis() as u64,
            takes: AtomicU64::new(0),
            take_misses: AtomicU64::new(0),
            expired: AtomicU64::new(0),
        }
    }

    #[inline]
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    #[inline]
    fn is_expired(&self, o: &PreSignedOrder, now: u64) -> bool {
        now.saturating_sub(o.created_ms) > self.ttl_ms
    }

    /// 放入一份预签名订单
    pub fn insert(&self, mut order: PreSignedOrder) {
        order.created_ms = self.now_ms();
        let key = (order.token_id.clone(), order.side.as_u8());
        let ticks = price_ticks(order.price);
        if let Ok(mut b) = self.buckets.write() {
            b.entry(key)
                .or_default()
                .entry(ticks)
                .or_default()
                .push(order);
        }
    }

    /// 批量放入
    pub fn insert_many<I: IntoIterator<Item = PreSignedOrder>>(&self, orders: I) {
        for o in orders {
            self.insert(o);
        }
    }

    /// 按精确价格取走一份订单
    ///
    /// 取走后即从池中移除，保证 salt 不被复用。
    pub fn take(&self, token_id: &str, side: Side, price: f64) -> Option<PreSignedOrder> {
        let key = (token_id.to_string(), side.as_u8());
        let ticks = price_ticks(price);
        let now = self.now_ms();

        let mut result = None;
        if let Ok(mut b) = self.buckets.write() {
            if let Some(by_price) = b.get_mut(&key) {
                if let Some(list) = by_price.get_mut(&ticks) {
                    // 从尾部弹出，优先取最新签的
                    while let Some(o) = list.pop() {
                        if self.is_expired(&o, now) {
                            self.expired.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        result = Some(o);
                        break;
                    }
                    if list.is_empty() {
                        by_price.remove(&ticks);
                    }
                }
            }
        }

        match result {
            Some(o) => {
                self.takes.fetch_add(1, Ordering::Relaxed);
                Some(o)
            }
            None => {
                self.take_misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// 取一份「不劣于」给定限价的订单
    ///
    /// - BUY：取价格 ≤ `limit_price` 中最高的（最可能成交且不超预算）
    /// - SELL：取价格 ≥ `limit_price` 中最低的
    ///
    /// 这样即使信号给出的价格与预签档位不完全吻合，也能命中快路径。
    pub fn take_acceptable(
        &self,
        token_id: &str,
        side: Side,
        limit_price: f64,
    ) -> Option<PreSignedOrder> {
        let key = (token_id.to_string(), side.as_u8());
        let limit = price_ticks(limit_price);
        let now = self.now_ms();

        let mut result = None;
        if let Ok(mut b) = self.buckets.write() {
            if let Some(by_price) = b.get_mut(&key) {
                // 选出符合条件的最优价格档
                let best: Option<i64> = match side {
                    Side::Buy => by_price
                        .keys()
                        .filter(|t| **t <= limit)
                        .max()
                        .copied(),
                    Side::Sell => by_price
                        .keys()
                        .filter(|t| **t >= limit)
                        .min()
                        .copied(),
                };

                if let Some(ticks) = best {
                    if let Some(list) = by_price.get_mut(&ticks) {
                        while let Some(o) = list.pop() {
                            if self.is_expired(&o, now) {
                                self.expired.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            result = Some(o);
                            break;
                        }
                        if list.is_empty() {
                            by_price.remove(&ticks);
                        }
                    }
                }
            }
        }

        match result {
            Some(o) => {
                self.takes.fetch_add(1, Ordering::Relaxed);
                Some(o)
            }
            None => {
                self.take_misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// 清理过期订单，返回清理数量
    pub fn sweep(&self) -> usize {
        let now = self.now_ms();
        let mut removed = 0usize;
        if let Ok(mut b) = self.buckets.write() {
            b.retain(|_, by_price| {
                by_price.retain(|_, list| {
                    let before = list.len();
                    list.retain(|o| now.saturating_sub(o.created_ms) <= self.ttl_ms);
                    removed += before - list.len();
                    !list.is_empty()
                });
                !by_price.is_empty()
            });
        }
        if removed > 0 {
            self.expired.fetch_add(removed as u64, Ordering::Relaxed);
        }
        removed
    }

    /// 清空指定市场的预签名订单（如市场已结算）
    pub fn invalidate_market(&self, token_id: &str) {
        if let Ok(mut b) = self.buckets.write() {
            b.retain(|(tid, _), _| tid != token_id);
        }
    }

    /// 清空全部
    pub fn clear(&self) {
        if let Ok(mut b) = self.buckets.write() {
            b.clear();
        }
    }

    /// 当前可用订单总数
    pub fn available(&self) -> usize {
        self.buckets
            .read()
            .map(|b| {
                b.values()
                    .map(|bp| bp.values().map(|v| v.len()).sum::<usize>())
                    .sum()
            })
            .unwrap_or(0)
    }

    /// 某个市场+方向下可用的价格档位数
    pub fn levels(&self, token_id: &str, side: Side) -> usize {
        self.buckets
            .read()
            .map(|b| {
                b.get(&(token_id.to_string(), side.as_u8()))
                    .map(|bp| bp.len())
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }

    pub fn stats(&self) -> PreSignStats {
        PreSignStats {
            takes: self.takes.load(Ordering::Relaxed),
            take_misses: self.take_misses.load(Ordering::Relaxed),
            expired: self.expired.load(Ordering::Relaxed),
            available: self.available(),
        }
    }
}

impl Default for PreSignedPool {
    fn default() -> Self {
        Self::new()
    }
}

/// 生成价格档位阶梯
///
/// 围绕 `center` 按 `tick` 步长生成 `2*depth+1` 个档位，并裁剪到 (0, 1)
/// 开区间——Polymarket 的概率价格不允许取到 0 或 1。
pub fn price_ladder(center: f64, tick: f64, depth: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(2 * depth + 1);
    for i in -(depth as i64)..=(depth as i64) {
        let p = center + tick * i as f64;
        // 量化到 tick 网格，消除浮点累积误差
        let q = (p / tick).round() * tick;
        if q > 0.0 && q < 1.0 {
            let rounded = (q * 10_000.0).round() / 10_000.0;
            if !out.contains(&rounded) {
                out.push(rounded);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::clob_types::Order;

    fn mk_signed(salt: &str, price: f64) -> SignedOrder {
        SignedOrder {
            order: Order {
                salt: salt.to_string(),
                maker: "0x1111111111111111111111111111111111111111".to_string(),
                signer: "0x1111111111111111111111111111111111111111".to_string(),
                taker: "0x0000000000000000000000000000000000000000".to_string(),
                token_id: "tok".to_string(),
                maker_amount: ((price * 1e6) as u64).to_string(),
                taker_amount: "1000000".to_string(),
                expiration: "0".to_string(),
                nonce: "0".to_string(),
                fee_rate_bps: "0".to_string(),
                side: 0,
                signature_type: 0,
            },
            signature: format!("0x{}", "ab".repeat(65)),
        }
    }

    fn mk(token: &str, side: Side, price: f64, salt: &str) -> PreSignedOrder {
        PreSignedOrder::new(token, side, price, 100.0, mk_signed(salt, price))
    }

    #[test]
    fn test_price_ticks_quantization() {
        assert_eq!(price_ticks(0.5), 5000);
        assert_eq!(price_ticks(0.0001), 1);
        assert_eq!(price_ticks(0.12345), 1235); // 四舍五入
    }

    #[test]
    fn test_insert_and_take() {
        let p = PreSignedPool::new();
        p.insert(mk("tok", Side::Buy, 0.55, "s1"));
        assert_eq!(p.available(), 1);

        let o = p.take("tok", Side::Buy, 0.55).unwrap();
        assert_eq!(o.salt(), "s1");
        assert_eq!(p.available(), 0, "取走后必须从池中移除");
    }

    #[test]
    fn test_take_is_not_repeatable() {
        let p = PreSignedPool::new();
        p.insert(mk("tok", Side::Buy, 0.55, "s1"));
        assert!(p.take("tok", Side::Buy, 0.55).is_some());
        assert!(
            p.take("tok", Side::Buy, 0.55).is_none(),
            "同一份预签名订单不得被取用两次（salt 会重复）"
        );
    }

    #[test]
    fn test_take_wrong_side_or_token_misses() {
        let p = PreSignedPool::new();
        p.insert(mk("tok", Side::Buy, 0.55, "s1"));
        assert!(p.take("tok", Side::Sell, 0.55).is_none());
        assert!(p.take("other", Side::Buy, 0.55).is_none());
        assert!(p.take("tok", Side::Buy, 0.60).is_none());
        // 原订单仍在
        assert_eq!(p.available(), 1);
    }

    #[test]
    fn test_take_acceptable_buy_picks_highest_within_limit() {
        let p = PreSignedPool::new();
        p.insert(mk("tok", Side::Buy, 0.50, "a"));
        p.insert(mk("tok", Side::Buy, 0.54, "b"));
        p.insert(mk("tok", Side::Buy, 0.58, "c"));

        // 限价 0.55：应取 0.54（≤0.55 中最高）
        let o = p.take_acceptable("tok", Side::Buy, 0.55).unwrap();
        assert_eq!(o.salt(), "b");
    }

    #[test]
    fn test_take_acceptable_sell_picks_lowest_above_limit() {
        let p = PreSignedPool::new();
        p.insert(mk("tok", Side::Sell, 0.50, "a"));
        p.insert(mk("tok", Side::Sell, 0.56, "b"));
        p.insert(mk("tok", Side::Sell, 0.60, "c"));

        // 限价 0.55：应取 0.56（≥0.55 中最低）
        let o = p.take_acceptable("tok", Side::Sell, 0.55).unwrap();
        assert_eq!(o.salt(), "b");
    }

    #[test]
    fn test_take_acceptable_no_match() {
        let p = PreSignedPool::new();
        p.insert(mk("tok", Side::Buy, 0.80, "a"));
        // 全部高于限价，BUY 无可用
        assert!(p.take_acceptable("tok", Side::Buy, 0.50).is_none());
    }

    #[test]
    fn test_expiry_on_take() {
        let p = PreSignedPool::with_ttl(Duration::from_millis(30));
        p.insert(mk("tok", Side::Buy, 0.55, "s1"));
        std::thread::sleep(Duration::from_millis(60));
        assert!(p.take("tok", Side::Buy, 0.55).is_none(), "过期订单不可取用");
        assert_eq!(p.stats().expired, 1);
    }

    #[test]
    fn test_sweep() {
        let p = PreSignedPool::with_ttl(Duration::from_millis(30));
        p.insert(mk("tok", Side::Buy, 0.55, "s1"));
        p.insert(mk("tok", Side::Buy, 0.56, "s2"));
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(p.sweep(), 2);
        assert_eq!(p.available(), 0);
    }

    #[test]
    fn test_invalidate_market() {
        let p = PreSignedPool::new();
        p.insert(mk("tokA", Side::Buy, 0.5, "a"));
        p.insert(mk("tokB", Side::Buy, 0.5, "b"));
        p.invalidate_market("tokA");
        assert_eq!(p.available(), 1);
        assert!(p.take("tokB", Side::Buy, 0.5).is_some());
    }

    #[test]
    fn test_levels_and_coverage() {
        let p = PreSignedPool::new();
        p.insert_many(vec![
            mk("tok", Side::Buy, 0.50, "a"),
            mk("tok", Side::Buy, 0.51, "b"),
            mk("tok", Side::Buy, 0.52, "c"),
        ]);
        assert_eq!(p.levels("tok", Side::Buy), 3);

        assert!(p.take("tok", Side::Buy, 0.50).is_some()); // hit
        assert!(p.take("tok", Side::Buy, 0.99).is_none()); // miss
        let s = p.stats();
        assert_eq!(s.takes, 1);
        assert_eq!(s.take_misses, 1);
        assert!((s.coverage() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_multiple_orders_same_price() {
        let p = PreSignedPool::new();
        p.insert(mk("tok", Side::Buy, 0.5, "a"));
        p.insert(mk("tok", Side::Buy, 0.5, "b"));
        assert_eq!(p.available(), 2);
        assert!(p.take("tok", Side::Buy, 0.5).is_some());
        assert!(p.take("tok", Side::Buy, 0.5).is_some());
        assert!(p.take("tok", Side::Buy, 0.5).is_none());
    }

    #[test]
    fn test_price_ladder() {
        let l = price_ladder(0.50, 0.01, 2);
        assert_eq!(l.len(), 5);
        assert!(l.contains(&0.48));
        assert!(l.contains(&0.50));
        assert!(l.contains(&0.52));
    }

    #[test]
    fn test_price_ladder_clamps_to_open_interval() {
        // 中心贴近 0，负档位和 0 都应被裁掉
        let l = price_ladder(0.02, 0.01, 5);
        assert!(l.iter().all(|p| *p > 0.0 && *p < 1.0));
        assert!(!l.contains(&0.0));

        // 中心贴近 1
        let h = price_ladder(0.98, 0.01, 5);
        assert!(h.iter().all(|p| *p > 0.0 && *p < 1.0));
        assert!(!h.contains(&1.0));
    }

    #[test]
    fn test_clear() {
        let p = PreSignedPool::new();
        p.insert_many(vec![
            mk("a", Side::Buy, 0.5, "1"),
            mk("b", Side::Sell, 0.5, "2"),
        ]);
        p.clear();
        assert_eq!(p.available(), 0);
    }
}
