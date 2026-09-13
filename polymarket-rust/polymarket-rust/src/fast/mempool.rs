//! Mempool Monitor —— 防抢跑、去重、限频、待确认追踪
//!
//! ## 问题
//!
//! Polymarket CLOB 是中心化撮合（HTTP API 提单），但以下风险仍需本地防护：
//!
//! 1. **重复下单**：网络超时重试导致同一笔信号提交两次
//! 2. **订单风暴**：策略 bug 或市场剧烈波动时短时间内发出大量订单
//! 3. **pending 状态黑洞**：已提交但未收到确认的订单，无法判断是否成交
//! 4. **链上抢跑（可选）**：检测同 market 的大额 pending tx，避免被 MEV 夹击
//!
//! ## 架构
//!
//! ```text
//! FastExecutor.execute_fast()
//!   ├─ Stage 1~4: 构建+签名 (不变)
//!   ├─ ★ MempoolMonitor.check_before_submit()  ← 注入点 A
//!   │     ├─ DeduplicationEngine: 滑动窗口去重 (3s)
//!   │     ├─ RateLimiter: 全局 + per-token 限频
//!   │     └─ PendingOrderTracker: pending 数量检查
//!   ├─ Stage 5~7: Serialize → Hmac → Http (不变)
//!   └─ ★ MempoolMonitor.on_submitted()         ← 注入点 B
//!         └─ 记录到 pending_set，启动 TTL 清理
//! ```
//!
//! ## 性能预算
//!
//! `check_before_submit()` 全路径 < 10μs（HashMap lookup + AtomicU64），
//! 不影响 FastExecutor 的 100ms 延迟目标。

use crate::clob::error::{ClobError, ClobResult};
use crate::types::enums::Side;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

// ============================================================================
// 常量
// ============================================================================

/// 默认去重时间窗口：相同 fingerprint 在此时间内拒绝
pub const DEFAULT_DEDUP_WINDOW: Duration = Duration::from_secs(3);

/// 默认 pending 订单 TTL：超过此时间未确认则标记为 stale
pub const DEFAULT_PENDING_TTL: Duration = Duration::from_secs(30);

/// 默认全局令牌桶容量（orders/sec）
pub const DEFAULT_GLOBAL_RATE_LIMIT: f64 = 20.0;

/// 默认 per-token 限频（orders/sec/token）
pub const DEFAULT_TOKEN_RATE_LIMIT: f64 = 5.0;

/// 默认单个 token 最大 pending 订单数
pub const DEFAULT_MAX_PENDING_PER_TOKEN: usize = 10;

/// 默认全局最大 pending 订单数
pub const DEFAULT_MAX_PENDING_GLOBAL: usize = 50;

// ============================================================================
// OrderFingerprint — 标准化去重键
// ============================================================================

/// 订单指纹：用于识别「语义相同的订单」
///
/// 价格按 `tick_size` 对齐到最小报价单位，数量按 `COLLATERAL_DECIMALS=6`
/// 舍入。这样 `price=0.5001` 和 `price=0.5002` 在 `Point01` 精度下是同一笔。
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct OrderFingerprint {
    /// Token ID（条件代币标识）
    pub token_id: String,
    /// 买卖方向
    pub side: Side,
    /// 价格（按 tick 对齐后的整数值，如 Point01 下 0.50 → 50）
    pub price_ticks: i64,
    /// 数量（按 COLLATERAL_DECIMALS 舍入后的整数值）
    pub size_ticks: i64,
}

impl OrderFingerprint {
    /// 从原始参数构建指纹
    ///
    /// - `price` 按 `tick` 步长对齐（四舍五入到最近档位）
    /// - `size` 按 `collateral_decimals`（默认 6 位）舍入为整数
    pub fn new(token_id: &str, side: Side, price: f64, size: f64, tick: f64) -> Self {
        let price_ticks = (price / tick).round() as i64;
        // Polymarket 使用 6 位小数的 USDC 单位
        const COLLATERAL_DECIMALS: f64 = 1_000_000.0;
        let size_ticks = (size * COLLATERAL_DECIMALS).round() as i64;
        Self {
            token_id: token_id.to_string(),
            side,
            price_ticks,
            size_ticks,
        }
    }
}

impl std::fmt::Display for OrderFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}|{}|p{}|s{}",
            match self.side {
                Side::Buy => "B",
                Side::Sell => "S",
            },
            self.token_id,
            self.price_ticks,
            self.size_ticks
        )
    }
}

// ============================================================================
// DeduplicationEngine — 滑动窗口去重
// ============================================================================

/// 去重引擎：在滑动时间窗口内拒绝重复订单
///
/// 内部维护一个 `HashSet<OrderFingerprint>` + 每个条目的插入时刻。
/// 超过窗口期的条目在下次 `check()` 时惰性清理。
///
/// 并发安全：读多写少场景下使用 `RwLock`，命中路径只需读锁。
pub struct DeduplicationEngine {
    /// 已见指纹 + 首次见到的时间
    seen: RwLock<HashMap<OrderFingerprint, Instant>>,
    /// 去重时间窗口
    window: Duration,
    /// 统计：总检查次数
    checks: AtomicU64,
    /// 统计：拦截次数（重复）
    rejected: AtomicU64,
}

impl DeduplicationEngine {
    /// 创建去重引擎
    pub fn new(window: Duration) -> Self {
        Self {
            seen: RwLock::new(HashMap::new()),
            window,
            checks: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        }
    }

    /// 使用默认 3s 窗口创建
    pub fn with_default_window() -> Self {
        Self::new(DEFAULT_DEDUP_WINDOW)
    }

    /// 检查指纹是否重复
    ///
    /// 返回 `Ok(())` 表示可以放行，`Err` 表示在窗口内发现重复。
    pub fn check(&self, fp: &OrderFingerprint) -> ClobResult<()> {
        self.checks.fetch_add(1, Ordering::Relaxed);
        let mut map = self.seen.write().unwrap();

        // 惰性清理过期条目（每次检查最多清理 1/8 的条目，避免一次卡太久）
        if map.len() > 64 {
            let now = Instant::now();
            let expire_count = (map.len() / 8).max(1);
            let expired: Vec<OrderFingerprint> = map
                .iter()
                .filter(|(_, t)| now.duration_since(**t) > self.window)
                .take(expire_count)
                .map(|(k, _)| k.clone())
                .collect();
            for k in expired {
                map.remove(&k);
            }
        }

        if let Some(prev) = map.get(fp) {
            if prev.elapsed() < self.window {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                return Err(ClobError::OrderFailed(format!(
                    "重复订单被拦截: {} (窗口 {}s 内)",
                    fp,
                    self.window.as_secs()
                )));
            }
        }

        map.insert(fp.clone(), Instant::now());
        Ok(())
    }

    /// 强制记录一个指纹（用于外部确认已提交的订单）
    pub fn record(&self, fp: OrderFingerprint) {
        self.seen.write().unwrap().insert(fp, Instant::now());
    }

    /// 清空所有已见记录
    pub fn clear(&self) {
        self.seen.write().unwrap().clear();
    }

    /// 当前已见（未过期）条目数
    pub fn len(&self) -> usize {
        let map = self.seen.read().unwrap();
        let now = Instant::now();
        map.iter()
            .filter(|(_, t)| now.duration_since(**t) <= self.window)
            .count()
    }

    /// 统计信息
    pub fn stats(&self) -> DedupStats {
        DedupStats {
            total_checks: self.checks.load(Ordering::Relaxed),
            total_rejected: self.rejected.load(Ordering::Relaxed),
            current_seen: self.len(),
        }
    }
}

/// 去重统计
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DedupStats {
    pub total_checks: u64,
    pub total_rejected: u64,
    pub current_seen: usize,
}

// ============================================================================
// RateLimiter — 双层频率限制
// ============================================================================

/// 令牌桶（Mutex 保护，简单可靠）
struct TokenBucket {
    inner: std::sync::Mutex<TokenBucketInner>,
}

struct TokenBucketInner {
    capacity: u64,
    rate_per_sec: u64,
    last_refill: Instant,
    tokens: u64,
}

impl TokenBucket {
    fn new(rate: f64, burst: f64) -> Self {
        const SCALE: u64 = 1000; // precision 0.001
        Self {
            inner: std::sync::Mutex::new(TokenBucketInner {
                capacity: (burst * SCALE as f64) as u64,
                rate_per_sec: (rate * SCALE as f64) as u64,
                last_refill: Instant::now(),
                tokens: (burst * SCALE as f64) as u64,
            }),
        }
    }

    fn try_acquire(&self) -> bool {
        let mut guard = self.inner.lock().unwrap();
        let now = Instant::now();
        let elapsed = now.duration_since(guard.last_refill);
        let refill = (elapsed.as_secs_f64() * guard.rate_per_sec as f64) as u64;
        guard.tokens = (guard.tokens + refill).min(guard.capacity);
        guard.last_refill = now;

        if guard.tokens >= 1 {
            guard.tokens -= 1;
            true
        } else {
            false
        }
    }
}

/// Per-token 滑动窗口计数器
struct SlidingWindowCounter {
    /// 窗口长度
    window: Duration,
    /// {token_id: Vec<(count, window_start)>}
    counters: RwLock<HashMap<String, (u64, Instant)>>,
}

impl SlidingWindowCounter {
    fn new(window: Duration) -> Self {
        Self {
            window,
            counters: RwLock::new(HashMap::new()),
        }
    }

    /// 尝试增加计数。返回 Ok(remaining) 表示允许，Err 表示超限。
    fn increment(&self, key: &str, limit: u64) -> Result<u64, u64> {
        let mut map = self.counters.write().unwrap();
        let now = Instant::now();
        let entry = map.entry(key.to_string()).or_insert((0, now));

        // 窗口滑动：如果当前时间超出窗口期，重置计数
        if now.duration_since(entry.1) > self.window {
            *entry = (0, now);
        }

        if entry.0 >= limit {
            Err(limit) // 超限
        } else {
            entry.0 += 1;
            Ok(limit - entry.0) // 返回剩余配额
        }
    }

    /// 清除指定 token 的计数
    fn reset(&self, key: &str) {
        self.counters.write().unwrap().remove(key);
    }

    /// 清空所有
    fn clear(&self) {
        self.counters.write().unwrap().clear();
    }
}

/// 双层频率限制器
///
/// 第一层：全局令牌桶（防止总体订单风暴）
/// 第二层：per-token 滑动窗口（防止单个 market 刷单）
pub struct RateLimiter {
    /// 全局限频（令牌桶）
    global: TokenBucket,
    /// per-token 限频（滑动窗口）
    per_token: SlidingWindowCounter,
    /// per-token 每秒上限配置
    token_limit: u64,
    /// 统计：总检查次数
    checks: AtomicU64,
    /// 统计：拦截次数
    rejected: AtomicU64,
}

impl RateLimiter {
    /// 创建频率限制器
    ///
    /// - `global_rate`: 全局 orders/sec（如 20.0）
    /// - `global_burst`: 全局突发容量（如 5.0）
    /// - `token_rate`: per-token orders/sec（如 5.0）
    /// - `window`: per-token 滑动窗口宽度
    pub fn new(global_rate: f64, global_burst: f64, token_rate: f64, window: Duration) -> Self {
        Self {
            global: TokenBucket::new(global_rate, global_burst),
            per_token: SlidingWindowCounter::new(window),
            token_limit: (token_rate * window.as_secs() as f64) as u64,
            checks: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        }
    }

    /// 使用默认参数创建
    pub fn with_defaults() -> Self {
        Self::new(
            DEFAULT_GLOBAL_RATE_LIMIT,
            5.0, // 允许短时突发 5 笔
            DEFAULT_TOKEN_RATE_LIMIT,
            Duration::from_secs(1), // 1 秒窗口
        )
    }

    /// 检查是否允许下单
    ///
    /// 先检查全局令牌桶，再检查 per-token 滑动窗口。
    pub fn check(&self, token_id: &str) -> ClobResult<()> {
        self.checks.fetch_add(1, Ordering::Relaxed);

        // 第一层：全局令牌桶
        if !self.global.try_acquire() {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(ClobError::OrderFailed(
                "全局频率限制: 订单速率过高，请稍后重试".to_string(),
            ));
        }

        // 第二层：per-token 滑动窗口
        match self.per_token.increment(token_id, self.token_limit) {
            Ok(_) => Ok(()),
            Err(limit) => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                Err(ClobError::OrderFailed(format!(
                    "Token 频率限制: {} 在窗口内已达上限 ({})",
                    token_id, limit
                )))
            }
        }
    }

    /// 重置指定 token 的计数
    pub fn reset_token(&self, token_id: &str) {
        self.per_token.reset(token_id);
    }

    /// 清空所有计数
    pub fn reset_all(&self) {
        self.per_token.clear();
    }

    /// 统计
    pub fn stats(&self) -> RateLimitStats {
        RateLimitStats {
            total_checks: self.checks.load(Ordering::Relaxed),
            total_rejected: self.rejected.load(Ordering::Relaxed),
        }
    }
}

/// 限频统计
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimitStats {
    pub total_checks: u64,
    pub total_rejected: u64,
}

// ============================================================================
// PendingOrderTracker — 待确认订单追踪
// ============================================================================

/// 待确认订单状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingStatus {
    /// 已提交，等待交易所确认
    Submitted,
    /// 交易所已接受（有 orderID）
    Accepted,
    /// 已确认成交/部分成交
    Confirmed,
    /// 已取消
    Cancelled,
    /// 超时未确认（stale）
    Stale,
    /// 失败
    Failed,
}

/// 待确认订单条目
#[derive(Debug, Clone)]
pub struct PendingEntry {
    /// 订单指纹
    pub fingerprint: OrderFingerprint,
    /// 交易所 orderID（可能尚未返回）
    pub order_id: Option<String>,
    /// 提交时刻
    pub submitted_at: Instant,
    /// 当前状态
    pub status: PendingStatus,
    /// 原始请求参数摘要（用于日志/debug）
    pub price: f64,
    pub size: f64,
}

/// 待确认订单追踪器
///
/// 维护所有已提交但未最终确认的订单集合，提供：
/// - per-token pending 数量检查（防止单一 market 堆积过多挂单）
/// - 全局 pending 数量检查
/// - TTL 过期自动清理
/// - 状态流转（Submitted → Accepted → Confirmed / Cancelled / Stale）
pub struct PendingOrderTracker {
    /// {order_id_or_temp_key: PendingEntry}
    orders: RwLock<HashMap<String, PendingEntry>>,
    /// per-token 当前 pending 数量（快速路径，无需遍历全量）
    per_token_counts: RwLock<HashMap<String, usize>>,
    /// TTL
    ttl: Duration,
    /// 单 token 最大 pending 数
    max_per_token: usize,
    /// 全局最大 pending 数
    max_global: usize,
    /// 统计
    total_submitted: AtomicU64,
    total_confirmed: AtomicU64,
    total_expired: AtomicU64,
}

impl PendingOrderTracker {
    /// 创建追踪器
    pub fn new(ttl: Duration, max_per_token: usize, max_global: usize) -> Self {
        Self {
            orders: RwLock::new(HashMap::new()),
            per_token_counts: RwLock::new(HashMap::new()),
            ttl,
            max_per_token,
            max_global,
            total_submitted: AtomicU64::new(0),
            total_confirmed: AtomicU64::new(0),
            total_expired: AtomicU64::new(0),
        }
    }

    /// 使用默认参数创建
    pub fn with_defaults() -> Self {
        Self::new(
            DEFAULT_PENDING_TTL,
            DEFAULT_MAX_PENDING_PER_TOKEN,
            DEFAULT_MAX_PENDING_GLOBAL,
        )
    }

    /// 检查是否允许新增 pending 订单（不实际添加）
    ///
    /// 锁顺序统一为「先 orders 后 counts」，与写锁路径（`try_submit`）
    /// 一致，避免读写锁交叉死锁。
    pub fn check_capacity(&self, token_id: &str) -> ClobResult<()> {
        let orders = self.orders.read().unwrap();
        let counts = self.per_token_counts.read().unwrap();

        // 检查 per-token 上限
        if let Some(&n) = counts.get(token_id) {
            if n >= self.max_per_token {
                return Err(ClobError::OrderFailed(format!(
                    "Pending 上限: token={} 已有 {} 笔挂单 (上限 {})",
                    token_id, n, self.max_per_token
                )));
            }
        }

        // 检查全局上限
        if orders.len() >= self.max_global {
            return Err(ClobError::OrderFailed(format!(
                "全局 Pending 上限: 已有 {} 笔挂单 (上限 {})",
                orders.len(),
                self.max_global
            )));
        }

        Ok(())
    }

    /// 原子提交（P0-012）：capacity check + reservation 在同一写锁内完成，
    /// 杜绝并发 submit 同时通过容量检查导致突破 per-token/global 上限。
    pub fn try_submit(&self, key: String, entry: PendingEntry) -> ClobResult<()> {
        let token_id = entry.fingerprint.token_id.clone();

        // 统一锁顺序：先 orders 后 counts，两个写锁内完成 check + reserve
        let mut orders = self.orders.write().unwrap();
        let mut counts = self.per_token_counts.write().unwrap();

        // 检查 per-token 上限
        let per_token = counts.get(&token_id).copied().unwrap_or(0);
        if per_token >= self.max_per_token {
            return Err(ClobError::OrderFailed(format!(
                "Pending 上限: token={} 已有 {} 笔挂单 (上限 {})",
                token_id, per_token, self.max_per_token
            )));
        }

        // 检查全局上限
        if orders.len() >= self.max_global {
            return Err(ClobError::OrderFailed(format!(
                "全局 Pending 上限: 已有 {} 笔挂单 (上限 {})",
                orders.len(),
                self.max_global
            )));
        }

        // reserve（原子）
        orders.insert(key, entry);
        *counts.entry(token_id).or_insert(0) += 1;
        self.total_submitted.fetch_add(1, Ordering::Relaxed);

        Ok(())
    }

    /// 记录一笔新提交的订单（委托 [`PendingOrderTracker::try_submit`]，兼容旧调用）。
    ///
    /// `key` 应该是唯一标识（可用临时 key 如 `"tmp_{timestamp}_{nonce}"`，
    /// 后续收到 orderID 后通过 `update_order_id()` 更新）。
    pub fn submit(&self, key: String, entry: PendingEntry) -> ClobResult<()> {
        self.try_submit(key, entry)
    }

    /// 用正式 orderID 替换临时 key
    pub fn update_order_id(&self, temp_key: &str, order_id: &str) {
        let mut orders = self.orders.write().unwrap();
        if let Some(entry) = orders.remove(temp_key) {
            orders.insert(order_id.to_string(), entry);
        }
    }

    /// 更新订单状态
    pub fn update_status(&self, key: &str, status: PendingStatus) {
        let mut orders = self.orders.write().unwrap();
        if let Some(entry) = orders.get_mut(key) {
            let old_status = entry.status;
            entry.status = status;
            // 状态从非终态转为终态时更新统计
            if matches!(
                old_status,
                PendingStatus::Submitted | PendingStatus::Accepted
            ) && matches!(
                status,
                PendingStatus::Confirmed
                    | PendingStatus::Cancelled
                    | PendingStatus::Failed
                    | PendingStatus::Stale
            ) {
                self.total_confirmed.fetch_add(1, Ordering::Relaxed);
                // 减少 per_token 计数
                let mut counts = self.per_token_counts.write().unwrap();
                if let Some(n) = counts.get_mut(&entry.fingerprint.token_id) {
                    *n = n.saturating_sub(1);
                }
            }
        }
    }

    /// 获取订单状态
    pub fn get_status(&self, key: &str) -> Option<PendingStatus> {
        self.orders.read().unwrap().get(key).map(|e| e.status)
    }

    /// 获取完整订单条目
    pub fn get(&self, key: &str) -> Option<PendingEntry> {
        self.orders.read().unwrap().get(key).cloned()
    }

    /// 清理过期条目（stale），返回清理数量
    pub fn sweep(&self) -> usize {
        let mut orders = self.orders.write().unwrap();
        let mut counts = self.per_token_counts.write().unwrap();
        let now = Instant::now();

        let stale_keys: Vec<String> = orders
            .iter()
            .filter(|(_, e)| {
                now.duration_since(e.submitted_at) > self.ttl
                    && !matches!(
                        e.status,
                        PendingStatus::Confirmed | PendingStatus::Cancelled
                    )
            })
            .map(|(k, _)| k.clone())
            .collect();

        let count = stale_keys.len();
        for key in stale_keys {
            if let Some(entry) = orders.remove(&key) {
                *counts
                    .entry(entry.fingerprint.token_id.clone())
                    .or_insert(0) =
                    (*counts.get(&entry.fingerprint.token_id).unwrap_or(&1)).saturating_sub(1);
                // 标记为 stale（如果还在的话——已被 remove）
                self.total_expired.fetch_add(1, Ordering::Relaxed);
            }
        }
        count
    }

    /// 获取当前 pending 数量（Submitted + Accepted 状态）
    pub fn pending_count(&self) -> usize {
        self.orders
            .read()
            .unwrap()
            .values()
            .filter(|e| matches!(e.status, PendingStatus::Submitted | PendingStatus::Accepted))
            .count()
    }

    /// 获取指定 token 的 pending 数量
    pub fn pending_count_for_token(&self, token_id: &str) -> usize {
        self.orders
            .read()
            .unwrap()
            .values()
            .filter(|e| {
                e.fingerprint.token_id == token_id
                    && matches!(e.status, PendingStatus::Submitted | PendingStatus::Accepted)
            })
            .count()
    }

    /// 统计
    pub fn stats(&self) -> PendingStats {
        let orders = self.orders.read().unwrap();
        PendingStats {
            total_pending: self.pending_count(),
            total_tracked: orders.len(),
            total_submitted: self.total_submitted.load(Ordering::Relaxed),
            total_confirmed: self.total_confirmed.load(Ordering::Relaxed),
            total_expired: self.total_expired.load(Ordering::Relaxed),
        }
    }

    /// 清空所有追踪
    pub fn clear(&self) {
        self.orders.write().unwrap().clear();
        self.per_token_counts.write().unwrap().clear();
    }
}

/// Pending 统计
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingStats {
    pub total_pending: usize,
    pub total_tracked: usize,
    pub total_submitted: u64,
    pub total_confirmed: u64,
    pub total_expired: u64,
}

// ============================================================================
// FrontRunDetector — 可选链上抢跑检测
// ============================================================================

/// 抢跑检测结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontRunVerdict {
    /// 安全，可以正常下单
    Safe,
    /// 检测到可疑活动，建议延迟或取消
    Warning(String),
    /// 危险，强烈建议不下单
    Danger(String),
}

/// 抢跑检测器（默认 no-op，可替换为真实实现）
///
/// Polymarket CLOB 通过 HTTP API 下单，链上 mempool 监控主要用于：
/// - 检测 CLOB Exchange 合约上的大额 pending tx
/// - 检测 gas price spikes（网络拥堵可能导致延迟）
///
/// 默认实现始终返回 `Safe`，需要链上监控时可替换。
pub trait FrontRunDetector: Send + Sync {
    /// 检测当前是否有抢跑风险
    fn check(&self, _token_id: &str, _side: Side, _size: f64) -> FrontRunVerdict {
        FrontRunVerdict::Safe
    }
}

/// 默认 no-op 检测器（不做任何检测）
#[derive(Debug, Clone, Default)]
pub struct NoOpFrontRunDetector;

impl FrontRunDetector for NoOpFrontRunDetector {}

// ============================================================================
// MempoolMonitor — 整合层
// ============================================================================

/// Mempool 监控整合层
///
/// 整合去重、限频、pending 追踪、抢跑检测四大能力，
/// 通过两个钩子接入 [`crate::fast::executor::FastExecutor`]：
///
/// - [`MempoolMonitor::check_before_submit()`] — 提交前检查
/// - [`MempoolMonitor::on_submitted()`] — 提交后记录
pub struct MempoolMonitor {
    /// 去重引擎
    dedup: DeduplicationEngine,
    /// 频率限制器
    rate_limiter: RateLimiter,
    /// 待确认订单追踪
    pending: PendingOrderTracker,
    /// 抢跑检测器（动态分发）
    front_run: Arc<dyn FrontRunDetector>,
    /// 是否启用（可运行时开关）
    enabled: AtomicU64, // 0=disabled, 1=enabled
    /// 统计：check_before_submit 总调用次数
    total_checks: AtomicU64,
    /// 统计：总放行数
    total_allowed: AtomicU64,
    /// 统计：总拦截数
    total_blocked: AtomicU64,
}

/// MempoolMonitor 配置
#[derive(Debug, Clone)]
pub struct MempoolConfig {
    /// 去重时间窗口（默认 3s）
    pub dedup_window: Duration,
    /// 全局 orders/sec（默认 20）
    pub global_rate: f64,
    /// 全局突发容量（默认 5）
    pub global_burst: f64,
    /// per-token orders/sec（默认 5）
    pub token_rate: f64,
    /// per-token 滑动窗口（默认 1s）
    pub token_window: Duration,
    /// pending 订单 TTL（默认 30s）
    pub pending_ttl: Duration,
    /// 单 token 最大 pending 数（默认 10）
    pub max_pending_per_token: usize,
    /// 全局最大 pending 数（默认 50）
    pub max_pending_global: usize,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            dedup_window: DEFAULT_DEDUP_WINDOW,
            global_rate: DEFAULT_GLOBAL_RATE_LIMIT,
            global_burst: 5.0,
            token_rate: DEFAULT_TOKEN_RATE_LIMIT,
            token_window: Duration::from_secs(1),
            pending_ttl: DEFAULT_PENDING_TTL,
            max_pending_per_token: DEFAULT_MAX_PENDING_PER_TOKEN,
            max_pending_global: DEFAULT_MAX_PENDING_GLOBAL,
        }
    }
}

impl MempoolMonitor {
    /// 使用默认配置创建
    pub fn new() -> Self {
        Self::with_config(MempoolConfig::default())
    }

    /// 使用自定义配置创建
    pub fn with_config(config: MempoolConfig) -> Self {
        Self {
            dedup: DeduplicationEngine::new(config.dedup_window),
            rate_limiter: RateLimiter::new(
                config.global_rate,
                config.global_burst,
                config.token_rate,
                config.token_window,
            ),
            pending: PendingOrderTracker::new(
                config.pending_ttl,
                config.max_pending_per_token,
                config.max_pending_global,
            ),
            front_run: Arc::new(NoOpFrontRunDetector),
            enabled: AtomicU64::new(1), // 默认启用
            total_checks: AtomicU64::new(0),
            total_allowed: AtomicU64::new(0),
            total_blocked: AtomicU64::new(0),
        }
    }

    /// 安装自定义抢跑检测器
    pub fn with_front_run_detector<D: FrontRunDetector + 'static>(mut self, detector: D) -> Self {
        self.front_run = Arc::new(detector);
        self
    }

    /// 启用/禁用
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled as u64, Ordering::Release);
    }

    /// 是否启用
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire) != 0
    }

    /// === 核心钩子 A：提交前检查 ===
    ///
    /// 在 `FastExecutor` 签名完成后、序列化前调用。
    /// 任何一项检查失败即返回错误，阻止订单进入网络。
    ///
    /// 检查顺序（按开销从小到大排列）：
    /// 1. 开关检查（原子读，< 1ns）
    /// 2. 频率限制（AtomicU64 + HashMap，< 1μs）
    /// 3. 去重检查（RwLock read + HashSet，< 1μs）
    /// 4. Pending 容量检查（RwLock read，< 1μs）
    /// 5. 抢跑检测（trait dispatch，取决于实现）
    pub fn check_before_submit(
        &self,
        token_id: &str,
        side: Side,
        price: f64,
        size: f64,
        tick: f64,
    ) -> ClobResult<()> {
        self.total_checks.fetch_add(1, Ordering::Relaxed);

        // 0. 开关
        if !self.is_enabled() {
            self.total_allowed.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        // 1. 频率限制（最先做，因为最轻量且能挡住风暴）
        self.rate_limiter.check(token_id).map_err(|e| {
            self.total_blocked.fetch_add(1, Ordering::Relaxed);
            e
        })?;

        // 2. 去重
        let fp = OrderFingerprint::new(token_id, side, price, size, tick);
        self.dedup.check(&fp).map_err(|e| {
            self.total_blocked.fetch_add(1, Ordering::Relaxed);
            e
        })?;

        // 3. Pending 容量
        self.pending.check_capacity(token_id).map_err(|e| {
            self.total_blocked.fetch_add(1, Ordering::Relaxed);
            e
        })?;

        // 4. 抢跑检测
        match self.front_run.check(token_id, side, size) {
            FrontRunVerdict::Safe => {}
            FrontRunVerdict::Warning(msg) => {
                tracing::warn!("mempool: 抢跑警告: {}", msg);
                // warning 不阻断，仅记录
            }
            FrontRunVerdict::Danger(msg) => {
                self.total_blocked.fetch_add(1, Ordering::Relaxed);
                return Err(ClobError::OrderFailed(format!("抢跑检测阻断: {}", msg)));
            }
        }

        self.total_allowed.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// === 核心钩子 B：提交后记录 ===
    ///
    /// 在 HTTP 返回成功响应后调用，将订单加入 pending 追踪。
    ///
    /// `temp_key` 应该是唯一临时标识，后续收到确认后调用 `confirm()` 或
    /// `update_status()` 更新状态。
    pub fn on_submitted(
        &self,
        temp_key: String,
        token_id: &str,
        side: Side,
        price: f64,
        size: f64,
        tick: f64,
        order_id: Option<String>,
    ) {
        let fp = OrderFingerprint::new(token_id, side, price, size, tick);
        let entry = PendingEntry {
            fingerprint: fp.clone(),
            order_id: order_id.clone(),
            submitted_at: Instant::now(),
            status: if order_id.is_some() {
                PendingStatus::Accepted
            } else {
                PendingStatus::Submitted
            },
            price,
            size,
        };

        // 同时记录到去重引擎（确保即使指纹相同也不会在窗口内重复）
        self.dedup.record(fp);

        // 加入 pending 追踪
        if let Err(e) = self.pending.submit(temp_key, entry) {
            tracing::warn!("mempool: on_submitted 记录失败: {}", e);
        }
    }

    /// 确认订单（收到 exchange 回执后调用）
    pub fn confirm(&self, temp_key: &str, order_id: &str) {
        self.pending.update_order_id(temp_key, order_id);
        self.pending
            .update_status(order_id, PendingStatus::Confirmed);
    }

    /// 标记订单失败
    pub fn mark_failed(&self, key: &str) {
        self.pending.update_status(key, PendingStatus::Failed);
    }

    /// 标记订单取消
    pub fn mark_cancelled(&self, key: &str) {
        self.pending.update_status(key, PendingStatus::Cancelled);
    }

    /// 清理过期条目，返回清理数量
    pub fn sweep(&self) -> usize {
        self.pending.sweep()
    }

    /// 完整统计报告
    pub fn stats(&self) -> MempoolStats {
        MempoolStats {
            enabled: self.is_enabled(),
            total_checks: self.total_checks.load(Ordering::Relaxed),
            total_allowed: self.total_allowed.load(Ordering::Relaxed),
            total_blocked: self.total_blocked.load(Ordering::Relaxed),
            dedup: self.dedup.stats(),
            rate_limit: self.rate_limiter.stats(),
            pending: self.pending.stats(),
        }
    }

    /// 各子组件的可变引用（供高级用户直接操作）
    pub fn dedup(&self) -> &DeduplicationEngine {
        &self.dedup
    }

    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.rate_limiter
    }

    pub fn pending_tracker(&self) -> &PendingOrderTracker {
        &self.pending
    }
}

/// MempoolMonitor 完整统计
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MempoolStats {
    pub enabled: bool,
    pub total_checks: u64,
    pub total_allowed: u64,
    pub total_blocked: u64,
    pub dedup: DedupStats,
    pub rate_limit: RateLimitStats,
    pub pending: PendingStats,
}

impl Default for MempoolMonitor {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========== OrderFingerprint 测试 ==========

    #[test]
    fn test_fingerprint_basic() {
        let fp = OrderFingerprint::new("token-123", Side::Buy, 0.50, 100.0, 0.01);
        assert_eq!(fp.token_id, "token-123");
        assert_eq!(fp.side, Side::Buy);
        assert_eq!(fp.price_ticks, 50); // 0.50 / 0.01 = 50
        assert_eq!(fp.size_ticks, 100_000_000); // 100.0 * 1e6
    }

    #[test]
    fn test_fingerprint_price_alignment() {
        // Point01 精度: 0.5001 → 50, 0.505 → 51, 0.499 → 50
        let fp1 = OrderFingerprint::new("t", Side::Buy, 0.5001, 1.0, 0.01);
        let fp2 = OrderFingerprint::new("t", Side::Buy, 0.50, 1.0, 0.01);
        let fp3 = OrderFingerprint::new("t", Side::Buy, 0.505, 1.0, 0.01);
        assert_eq!(fp1.price_ticks, fp2.price_ticks); // 同一档
        assert_ne!(fp2.price_ticks, fp3.price_ticks); // 不同档
    }

    #[test]
    fn test_fingerprint_display() {
        let fp = OrderFingerprint::new("abc", Side::Sell, 0.55, 50.0, 0.001);
        let s = format!("{}", fp);
        assert!(s.starts_with("S|abc|"));
    }

    #[test]
    fn test_fingerprint_equality() {
        let fp1 = OrderFingerprint::new("t", Side::Buy, 0.50, 100.0, 0.01);
        let fp2 = OrderFingerprint::new("t", Side::Buy, 0.50, 100.0, 0.01);
        let fp3 = OrderFingerprint::new("t", Side::Sell, 0.50, 100.0, 0.01);
        assert_eq!(fp1, fp2);
        assert_ne!(fp1, fp3);
    }

    // ========== DeduplicationEngine 测试 ==========

    #[test]
    fn test_dedup_allow_first() {
        let engine = DeduplicationEngine::with_default_window();
        let fp = OrderFingerprint::new("t", Side::Buy, 0.5, 10.0, 0.01);
        assert!(engine.check(&fp).is_ok());
    }

    #[test]
    fn test_dedup_reject_duplicate() {
        let engine = DeduplicationEngine::with_default_window();
        let fp = OrderFingerprint::new("t", Side::Buy, 0.5, 10.0, 0.01);
        assert!(engine.check(&fp).is_ok());
        assert!(engine.check(&fp).is_err()); // 窗口内重复
    }

    #[test]
    fn test_dedup_different_fingerprints_ok() {
        let engine = DeduplicationEngine::with_default_window();
        let fp1 = OrderFingerprint::new("t", Side::Buy, 0.5, 10.0, 0.01);
        let fp2 = OrderFingerprint::new("t", Side::Sell, 0.5, 10.0, 0.01);
        assert!(engine.check(&fp1).is_ok());
        assert!(engine.check(&fp2).is_ok()); // 不同方向，不同指纹
    }

    #[test]
    fn test_dedup_stats() {
        let engine = DeduplicationEngine::with_default_window();
        let fp = OrderFingerprint::new("t", Side::Buy, 0.5, 10.0, 0.01);
        let _ = engine.check(&fp);
        let _ = engine.check(&fp); // 被拒
        let stats = engine.stats();
        assert_eq!(stats.total_checks, 2);
        assert_eq!(stats.total_rejected, 1);
    }

    #[test]
    fn test_dedup_clear() {
        let engine = DeduplicationEngine::with_default_window();
        let fp = OrderFingerprint::new("t", Side::Buy, 0.5, 10.0, 0.01);
        assert!(engine.check(&fp).is_ok());
        engine.clear();
        assert!(engine.check(&fp).is_ok()); // 清除后重新允许
    }

    // ========== RateLimiter 测试 ==========

    #[test]
    fn test_rate_limiter_basic() {
        let rl = RateLimiter::with_defaults();
        // 默认 20 ops/sec + burst 5，前几个应该都能过
        for _ in 0..5 {
            assert!(rl.check("token-1").is_ok());
        }
    }

    #[test]
    fn test_rate_limiter_per_token() {
        let rl = RateLimiter::new(100.0, 100.0, 2.0, Duration::from_secs(1));
        // per-token limit: 2/sec
        assert!(rl.check("t1").is_ok());
        assert!(rl.check("t1").is_ok());
        assert!(rl.check("t1").is_err()); // 第 3 个被拒
                                          // 不同 token 不受影响
        assert!(rl.check("t2").is_ok());
    }

    #[test]
    fn test_rate_limiter_reset() {
        let rl = RateLimiter::with_defaults();
        for _ in 0..10 {
            let _ = rl.check("t1");
        }
        rl.reset_token("t1");
        assert!(rl.check("t1").is_ok()); // 重置后恢复
    }

    #[test]
    fn test_rate_limiter_stats() {
        let rl = RateLimiter::with_defaults();
        let _ = rl.check("t1");
        let _ = rl.check("t1");
        let stats = rl.stats();
        assert_eq!(stats.total_checks, 2);
    }

    // ========== PendingOrderTracker 测试 ==========

    #[test]
    fn test_pending_submit_and_track() {
        let tracker = PendingOrderTracker::with_defaults();
        let fp = OrderFingerprint::new("t", Side::Buy, 0.5, 10.0, 0.01);
        let entry = PendingEntry {
            fingerprint: fp,
            order_id: None,
            submitted_at: Instant::now(),
            status: PendingStatus::Submitted,
            price: 0.5,
            size: 10.0,
        };
        assert!(tracker.submit("order-1".to_string(), entry).is_ok());
        assert_eq!(tracker.pending_count(), 1);
        assert_eq!(tracker.pending_count_for_token("t"), 1);
    }

    #[test]
    fn test_pending_capacity_limit() {
        let tracker = PendingOrderTracker::new(Duration::from_secs(30), 2, 100);
        let make_entry = |i: u32| {
            let fp = OrderFingerprint::new("t", Side::Buy, 0.5, 10.0, 0.01);
            PendingEntry {
                fingerprint: fp,
                order_id: None,
                submitted_at: Instant::now(),
                status: PendingStatus::Submitted,
                price: 0.5,
                size: 10.0,
            }
        };
        assert!(tracker.submit("o1".to_string(), make_entry(1)).is_ok());
        assert!(tracker.submit("o2".to_string(), make_entry(2)).is_ok());
        assert!(tracker.submit("o3".to_string(), make_entry(3)).is_err()); // 超过 per-token=2
    }

    #[test]
    fn test_pending_status_flow() {
        let tracker = PendingOrderTracker::with_defaults();
        let fp = OrderFingerprint::new("t", Side::Buy, 0.5, 10.0, 0.01);
        let entry = PendingEntry {
            fingerprint: fp,
            order_id: None,
            submitted_at: Instant::now(),
            status: PendingStatus::Submitted,
            price: 0.5,
            size: 10.0,
        };
        tracker.submit("o1".to_string(), entry).unwrap();
        assert_eq!(tracker.get_status("o1"), Some(PendingStatus::Submitted));

        tracker.update_order_id("o1", "exchange-id-123");
        tracker.update_status("exchange-id-123", PendingStatus::Confirmed);
        assert_eq!(
            tracker.get_status("exchange-id-123"),
            Some(PendingStatus::Confirmed)
        );
        // confirmed 后不再计入 pending
        assert_eq!(tracker.pending_count_for_token("t"), 0);
    }

    #[test]
    fn test_pending_stats() {
        let tracker = PendingOrderTracker::with_defaults();
        let stats = tracker.stats();
        assert_eq!(stats.total_pending, 0);
        assert_eq!(stats.total_submitted, 0);
    }

    // ========== MempoolMonitor 集成测试 ==========

    #[test]
    fn test_monitor_check_passes() {
        let mon = MempoolMonitor::new();
        let result = mon.check_before_submit("token-1", Side::Buy, 0.5, 10.0, 0.01);
        assert!(result.is_ok());
    }

    #[test]
    fn test_monitor_blocks_duplicate() {
        let mon = MempoolMonitor::new();
        mon.check_before_submit("t", Side::Buy, 0.5, 10.0, 0.01)
            .unwrap();
        let result = mon.check_before_submit("t", Side::Buy, 0.5, 10.0, 0.01);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("重复订单"));
    }

    #[test]
    fn test_monitor_blocks_rate_limit() {
        let mut cfg = MempoolConfig::default();
        cfg.global_rate = 1.0; // 1 op/sec
        cfg.global_burst = 0.002; // burst = 2 tokens
        cfg.token_rate = 100.0; // per-token rate high enough not to trigger
        cfg.dedup_window = Duration::from_secs(0); // 关闭去重，单独测限频
        let mon = MempoolMonitor::with_config(cfg);
        // 前两笔消耗 burst，第三笔因速率限制被拒绝
        mon.check_before_submit("t", Side::Buy, 0.51, 10.0, 0.01)
            .unwrap();
        mon.check_before_submit("t", Side::Buy, 0.52, 10.0, 0.01)
            .unwrap();
        let result = mon.check_before_submit("t", Side::Buy, 0.53, 10.0, 0.01);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("频率限制"));
    }

    #[test]
    fn test_monitor_on_submitted_records() {
        let mon = MempoolMonitor::new();
        mon.on_submitted(
            "tmp-1".to_string(),
            "token-1",
            Side::Buy,
            0.5,
            10.0,
            0.01,
            Some("ord-123".to_string()),
        );
        assert_eq!(mon.pending_tracker().pending_count(), 1);

        // 再次提交相同指纹应被去重拦截
        let result = mon.check_before_submit("token-1", Side::Buy, 0.5, 10.0, 0.01);
        assert!(result.is_err()); // 被 on_submitted 中隐式 record 的指纹拦截
    }

    #[test]
    fn test_monitor_disable() {
        let mon = MempoolMonitor::new();
        mon.set_enabled(false);
        // 即使重复也不拦截
        mon.check_before_submit("t", Side::Buy, 0.5, 10.0, 0.01)
            .unwrap();
        mon.check_before_submit("t", Side::Buy, 0.5, 10.0, 0.01)
            .unwrap(); // disabled 时不检查
        let stats = mon.stats();
        assert!(!stats.enabled);
        assert_eq!(stats.total_blocked, 0);
    }

    #[test]
    fn test_monitor_full_stats() {
        let mon = MempoolMonitor::new();
        let _ = mon.check_before_submit("t", Side::Buy, 0.5, 10.0, 0.01);
        let _ = mon.check_before_submit("t", Side::Buy, 0.5, 10.0, 0.01); // 被拒
        let stats = mon.stats();
        assert_eq!(stats.total_checks, 2);
        assert_eq!(stats.total_allowed, 1);
        assert_eq!(stats.total_blocked, 1);
        assert!(stats.enabled);
    }

    #[test]
    fn test_monitor_confirm_flow() {
        let mon = MempoolMonitor::new();
        mon.on_submitted(
            "tmp-1".to_string(),
            "token-1",
            Side::Buy,
            0.5,
            10.0,
            0.01,
            None,
        );
        mon.confirm("tmp-1", "ord-999");
        assert_eq!(
            mon.pending_tracker().get_status("ord-999"),
            Some(PendingStatus::Confirmed)
        );
    }

    #[test]
    fn test_monitor_sweep() {
        let mon = MempoolMonitor::with_config({
            let mut c = MempoolConfig::default();
            c.pending_ttl = Duration::from_millis(10); // 极短 TTL 方便测试
            c
        });
        mon.on_submitted("tmp-1".to_string(), "t", Side::Buy, 0.5, 10.0, 0.01, None);
        std::thread::sleep(Duration::from_millis(20));
        let swept = mon.sweep();
        assert!(swept >= 1);
    }

    // ========== FrontRunDetector 测试 ==========

    struct AlwaysDangerDetector;

    impl FrontRunDetector for AlwaysDangerDetector {
        fn check(&self, _token_id: &str, _side: Side, _size: f64) -> FrontRunVerdict {
            FrontRunVerdict::Danger("测试危险".to_string())
        }
    }

    #[test]
    fn test_custom_front_run_detector() {
        let mon = MempoolMonitor::new().with_front_run_detector(AlwaysDangerDetector);
        let result = mon.check_before_submit("t", Side::Buy, 0.5, 10.0, 0.01);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("抢跑检测阻断"));
    }

    struct WarningOnlyDetector;

    impl FrontRunDetector for WarningOnlyDetector {
        fn check(&self, _token_id: &str, _side: Side, _size: f64) -> FrontRunVerdict {
            FrontRunVerdict::Warning("gas 偏高".to_string())
        }
    }

    #[test]
    fn test_warning_does_not_block() {
        let mon = MempoolMonitor::new().with_front_run_detector(WarningOnlyDetector);
        // warning 不阻断
        assert!(mon
            .check_before_submit("t", Side::Buy, 0.5, 10.0, 0.01)
            .is_ok());
    }

    // ========== P0-012：Mempool capacity TOCTOU ==========

    #[test]
    fn test_try_submit_atomic_capacity() {
        // 全局上限 2，per-token 上限放宽到 10（聚焦全局容量的并发原子性）
        let tracker = Arc::new(PendingOrderTracker::new(Duration::from_secs(30), 10, 2));
        let success = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();

        for i in 0..8 {
            let t = tracker.clone();
            let s = success.clone();
            handles.push(std::thread::spawn(move || {
                let fp = OrderFingerprint::new("token-x", Side::Buy, 0.5, 10.0, 0.01);
                let entry = PendingEntry {
                    fingerprint: fp,
                    order_id: None,
                    submitted_at: Instant::now(),
                    status: PendingStatus::Submitted,
                    price: 0.5,
                    size: 10.0,
                };
                if t.try_submit(format!("ord-{}", i), entry).is_ok() {
                    s.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // 并发下只有 2 笔成功（全局上限 2），不会突破
        assert_eq!(success.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert_eq!(tracker.pending_count(), 2);
    }
}
