//! Nonce 管理 (NonceManager)
//!
//! 自动处理链上交易 nonce，支持 stuck tx 检测与 RBF（费用替换）。
//!
//! ## 背景
//!
//! Polymarket CLOB 使用 HTTP API 下单，但链上交互（如 deposit、claim rewards）
//! 需要在 Polygon (chain_id=137) 上发送交易。以太坊账户 nonce 是严格递增的，
//! 如果一笔 tx nonce=N 卡在 mempool，所有后续交易（N+1, N+2, ...）都会被阻塞。
//!
//! ## 设计
//!
//! ```text
//! NonceManager
//!   ├── NonceTracker   —— current + pending nonces，热/冷缓存
//!   ├── StuckTxDetector —— 检测超过 TTL 的 pending tx
//!   ├── RbfReplacer     —— gas × 1.125 加速替换 stuck tx
//!   └── RecoveryPolicy  —— 链上重同步 nonce（防止缓存偏移）
//! ```
//!
//! ## CLOB 订单 nonce 适配
//!
//! Polymarket `UserOrder.nonce` 是一个 on-chain cancel nonce（uint256），
//! 用于链上取消订单。NonceManager 不管理这个字段（由 CLOB 服务端处理），
//! 而是管理链上账户级别的 transaction nonce。
//!
//! ## 并发安全
//!
//! RwLock + AtomicU64，多线程安全。热路径 `next_nonce()` 只需读锁 +
//! fetch_add。

use crate::fast::rpc_pool::RpcPool;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

// ============================================================================
// 常量
// ============================================================================

/// 默认 nonce 缓存 TTL（链上 nonce 变化缓慢，30s 足够）
pub const DEFAULT_NONCE_CACHE_TTL: Duration = Duration::from_secs(30);

/// stuck tx 判定阈值：pending 超过此时间未确认
pub const DEFAULT_STUCK_TIMEOUT: Duration = Duration::from_secs(120);

/// RBF gas 增幅倍数（1.125 = 12.5%）
pub const RBF_GAS_BUMP: f64 = 1.125;

/// 最小 gas 增幅（1 gwei）
pub const MIN_GAS_BUMP_WEI: u64 = 1_000_000_000;

/// 最大重试次数（同一 nonce）
pub const DEFAULT_MAX_RETRIES: u32 = 5;

// ============================================================================
// NonceTracker
// ============================================================================

/// Nonce 生命周期状态（P0-011 状态机）
///
/// ```text
/// Allocated ──broadcast──> Pending ──confirmed──> 从 tracker 移除，推进 onchain watermark
///     │                        │
///     │ 发送前失败             │ 发送后失败 / RBF 耗尽
///     └───────────────────────┴──────────> 从 tracker 移除（mark_failed）
/// ```
///
/// - `Allocated`：`next_nonce()` 已分配，但尚未广播上链。
/// - `Pending`：已广播，等待链上确认。
/// - `Confirmed` / `Failed` 是终态，通过「从 `nonce_states` 移除」表达，不占 map 条目。
///
/// 说明：`broadcasted` 与 `Pending` 在链上语义中等价（广播即等待确认），
/// 故合并为同一状态；`failed-but-not-broadcasted` = `Allocated` 状态下直接
/// `mark_failed`，nonce 未上链可复用（分配游标不因此回退，由上层决定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonceState {
    /// 已分配，未广播
    Allocated,
    /// 已广播，等待确认
    Pending,
}

/// Nonce 追踪器
///
/// 维护链上账户的 nonce 状态：
/// - `onchain_nonce`: 链上已确认 nonce watermark（confirmed）
/// - `next_local`: 下一个待分配的 nonce 游标（原子递增，P0-010 并发安全）
/// - `nonce_states`: 已分配未确认的 nonce 及其状态（P0-011 状态机）
pub struct NonceTracker {
    /// 从链上读取的 confirmed nonce watermark
    onchain_nonce: AtomicU64,
    /// 下一个待分配的 nonce 游标（`fetch_add` 原子递增）
    next_local: AtomicU64,
    /// 已分配未确认的 nonce 状态 {nonce: NonceState}
    nonce_states: RwLock<HashMap<u64, NonceState>>,
    /// 缓存的过期时间
    cache_expires_at: RwLock<Instant>,
    /// 最后一次向链上同步结果
    last_sync: RwLock<Instant>,
    /// 缓存 TTL
    cache_ttl: Duration,
}

impl NonceTracker {
    /// 创建 nonce 追踪器
    pub fn new(onchain_nonce: u64) -> Self {
        Self {
            onchain_nonce: AtomicU64::new(onchain_nonce),
            next_local: AtomicU64::new(onchain_nonce),
            nonce_states: RwLock::new(HashMap::new()),
            cache_expires_at: RwLock::new(Instant::now() + DEFAULT_NONCE_CACHE_TTL),
            last_sync: RwLock::new(Instant::now()),
            cache_ttl: DEFAULT_NONCE_CACHE_TTL,
        }
    }

    /// 获取下一个可用 nonce（原子递增分配，P0-010 并发安全）。
    ///
    /// 分配游标 `next_local` 用 `fetch_add` 原子递增，与 pending 集合解耦，
    /// 并发调用各自拿到唯一 nonce，不会重复。
    pub fn next_nonce(&self) -> u64 {
        let next = self.next_local.fetch_add(1, Ordering::SeqCst);
        self.nonce_states
            .write()
            .unwrap()
            .insert(next, NonceState::Allocated);
        next
    }

    /// 标记 nonce 已广播（Allocated → Pending）。
    pub fn mark_pending(&self, nonce: u64) {
        if let Some(state) = self.nonce_states.write().unwrap().get_mut(&nonce) {
            *state = NonceState::Pending;
        }
    }

    /// 标记 nonce 为已确认（Pending → Confirmed：移除 + 推进 watermark）。
    pub fn confirm(&self, nonce: u64) {
        self.nonce_states.write().unwrap().remove(&nonce);
        // 推进 onchain watermark（只影响 confirmed 追踪，不影响分配游标）
        self.onchain_nonce.fetch_max(nonce + 1, Ordering::AcqRel);
    }

    /// 标记 nonce 为失败（Allocated/Pending → Failed：移除）。
    pub fn mark_failed(&self, nonce: u64) {
        self.nonce_states.write().unwrap().remove(&nonce);
    }

    /// 查询指定 nonce 的当前状态（终态 Confirmed/Failed 返回 None）。
    pub fn nonce_state(&self, nonce: u64) -> Option<NonceState> {
        self.nonce_states.read().unwrap().get(&nonce).copied()
    }

    /// 获取当前链上 nonce（已确认）
    pub fn onchain_nonce(&self) -> u64 {
        self.onchain_nonce.load(Ordering::Acquire)
    }

    /// 获取所有未确认（Allocated + Pending）nonce
    pub fn pending_nonces(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self
            .nonce_states
            .read()
            .unwrap()
            .keys()
            .copied()
            .collect();
        v.sort_unstable();
        v
    }

    /// 检查缓存是否过期（需要重新从链上同步）
    pub fn is_stale(&self) -> bool {
        Instant::now() > *self.cache_expires_at.read().unwrap()
    }

    /// 从链上强制同步 nonce（将本地状态重置为链上值）
    ///
    /// 应该在 is_stale() 返回 true 时调用。
    pub fn sync(&self, new_onchain_nonce: u64) {
        let old = self.onchain_nonce.load(Ordering::Acquire);
        let max_nonce = old.max(new_onchain_nonce);
        self.onchain_nonce.store(max_nonce, Ordering::Release);
        // 同步分配游标，避免分配到链上已确认的 nonce（只增不减）
        self.next_local.fetch_max(max_nonce, Ordering::SeqCst);
        *self.cache_expires_at.write().unwrap() = Instant::now() + self.cache_ttl;
        *self.last_sync.write().unwrap() = Instant::now();
        tracing::info!("nonce_manager: 同步链上 nonce {} → {}", old, max_nonce);
    }

    /// 重置追踪器状态（紧急恢复用）
    pub fn reset(&self, new_onchain_nonce: u64) {
        self.onchain_nonce
            .store(new_onchain_nonce, Ordering::Release);
        self.next_local.store(new_onchain_nonce, Ordering::SeqCst);
        self.nonce_states.write().unwrap().clear();
        *self.cache_expires_at.write().unwrap() = Instant::now() + self.cache_ttl;
        *self.last_sync.write().unwrap() = Instant::now();
    }

    /// 获取 pending 计数（未确认 nonce 数 = allocated + pending）
    pub fn pending_count(&self) -> usize {
        self.nonce_states.read().unwrap().len()
    }
}

// ============================================================================
// StuckTxEntry
// ============================================================================

/// Stuck 交易条目
#[derive(Debug, Clone)]
pub struct StuckTxEntry {
    /// 卡住的 nonce
    pub nonce: u64,
    /// 交易哈希
    pub tx_hash: String,
    /// 发送时刻
    pub sent_at: Instant,
    /// RBF 重试次数
    pub rbf_retries: u32,
    /// Gas 价格（wei）
    pub gas_price: u64,
}

impl StuckTxEntry {
    /// 当前 gas 经过 RBF 增幅后的价格
    pub fn rbf_gas_price(&self) -> u64 {
        let bumped = (self.gas_price as f64 * RBF_GAS_BUMP) as u64;
        if bumped < self.gas_price + MIN_GAS_BUMP_WEI {
            self.gas_price + MIN_GAS_BUMP_WEI
        } else {
            bumped
        }
    }

    /// 是否已达最大重试次数
    pub fn exhausted(&self, max_retries: u32) -> bool {
        self.rbf_retries >= max_retries
    }
}

// ============================================================================
// NonceManager
// ============================================================================

/// Nonce 管理器
///
/// 整合 nonce 追踪、stuck 检测和 RBF 替换。
///
/// # 典型使用
///
/// ```text
/// let mgr = NonceManager::new(42); // 链上当前 nonce=42
///
/// // 下单前获取 nonce
/// let nonce = mgr.next_nonce();
/// // ...发送交易...
/// mgr.register_tx(nonce, "0xabc", 30_000_000_000);
///
/// // 后台 tick 检测 stuck
/// let stuck = mgr.tick();
/// for entry in stuck {
///     // RBF 加速
///     let new_gas = entry.rbf_gas_price();
///     // ...重新签名并发送...
///     mgr.bump_nonce(entry.nonce, new_gas);
/// }
/// ```
pub struct NonceManager {
    /// Nonce 追踪器
    tracker: NonceTracker,
    /// stuck 交易追踪: HashMap<nonce, StuckTxEntry>
    stuck_txs: RwLock<HashMap<u64, StuckTxEntry>>,
    /// RPC 节点池（可选，用于链上同步）
    rpc: Option<Arc<RpcPool>>,
    /// 钱包地址（链上查询需要）
    wallet_address: Option<String>,
    /// 最大 RBF 重试次数
    max_retries: u32,
    /// stuck 判定超时
    stuck_timeout: Duration,
}

impl NonceManager {
    /// 创建 nonce 管理器
    pub fn new(onchain_nonce: u64) -> Self {
        Self {
            tracker: NonceTracker::new(onchain_nonce),
            stuck_txs: RwLock::new(HashMap::new()),
            rpc: None,
            wallet_address: None,
            max_retries: DEFAULT_MAX_RETRIES,
            stuck_timeout: DEFAULT_STUCK_TIMEOUT,
        }
    }

    /// 挂载 RPC 节点池（启用链上 nonce 同步）
    pub fn with_rpc_pool(mut self, rpc: Arc<RpcPool>, wallet_address: &str) -> Self {
        self.rpc = Some(rpc);
        self.wallet_address = Some(wallet_address.to_string());
        self
    }

    /// 设置 stuck 判定超时
    pub fn with_stuck_timeout(mut self, timeout: Duration) -> Self {
        self.stuck_timeout = timeout;
        self
    }

    /// 设置最大重试次数
    pub fn with_max_retries(mut self, max: u32) -> Self {
        self.max_retries = max;
        self
    }

    /// 获取下一个可用 nonce
    pub fn next_nonce(&self) -> u64 {
        self.tracker.next_nonce()
    }

    /// 标记 nonce 已广播（Allocated → Pending）。
    pub fn mark_pending(&self, nonce: u64) {
        self.tracker.mark_pending(nonce);
    }

    /// 查询指定 nonce 的当前状态（终态 Confirmed/Failed 返回 None）。
    pub fn nonce_state(&self, nonce: u64) -> Option<NonceState> {
        self.tracker.nonce_state(nonce)
    }

    /// 注册一笔已发送的交易（用于 stuck 检测），并把 nonce 标记为 Pending。
    pub fn register_tx(&self, nonce: u64, tx_hash: &str, gas_price: u64) {
        self.tracker.mark_pending(nonce);
        self.stuck_txs.write().unwrap().insert(
            nonce,
            StuckTxEntry {
                nonce,
                tx_hash: tx_hash.to_string(),
                sent_at: Instant::now(),
                rbf_retries: 0,
                gas_price,
            },
        );
    }

    /// 标记 nonce 已确认
    pub fn confirm(&self, nonce: u64) {
        self.tracker.confirm(nonce);
        self.stuck_txs.write().unwrap().remove(&nonce);
    }

    /// 标记 nonce 失败
    pub fn mark_failed(&self, nonce: u64) {
        self.tracker.mark_failed(nonce);
        self.stuck_txs.write().unwrap().remove(&nonce);
    }

    /// RBF 加速：为指定 nonce 的 tx 计算新的 gas price，
    /// 并更新 stuck 条目的重试计数
    pub fn bump_nonce(&self, nonce: u64, new_gas_price: u64) -> Option<u64> {
        let mut stuck = self.stuck_txs.write().unwrap();
        let entry = stuck.get_mut(&nonce)?;
        if entry.exhausted(self.max_retries) {
            return None; // 已达最大重试
        }
        let old_gas = entry.gas_price;
        entry.gas_price = new_gas_price.min(old_gas.saturating_mul(5)); // 最高 5x 原价
        entry.rbf_retries += 1;
        entry.sent_at = Instant::now();
        tracing::info!(
            "nonce_manager: RBF 加速 nonce={} gas={}→{} retry={}/{}",
            nonce,
            old_gas,
            entry.gas_price,
            entry.rbf_retries,
            self.max_retries
        );
        Some(entry.gas_price)
    }

    /// 周期性 tick：检测 stuck tx，返回需要 RBF 加速的列表
    pub fn tick(&self) -> Vec<StuckTxEntry> {
        let mut result = Vec::new();
        let now = Instant::now();
        let mut stuck = self.stuck_txs.write().unwrap();

        // 收集需要标记为 stuck 的条目
        let stuck_keys: Vec<u64> = stuck
            .iter()
            .filter(|(_, entry)| now.duration_since(entry.sent_at) > self.stuck_timeout)
            .map(|(k, _)| *k)
            .collect();

        for nonce in stuck_keys {
            // 先检查是否已耗尽（用不可变借用）
            let is_exhausted = stuck
                .get(&nonce)
                .map_or(true, |e| e.exhausted(self.max_retries));
            if is_exhausted {
                stuck.remove(&nonce);
                tracing::warn!("nonce_manager: 放弃 nonce={} (已重试达上限)", nonce,);
            } else if let Some(entry) = stuck.get(&nonce) {
                result.push(entry.clone());
            }
        }

        result
    }

    /// 获取所有活跃的 stuck 交易
    pub fn active_stuck_txs(&self) -> Vec<StuckTxEntry> {
        self.stuck_txs
            .read()
            .unwrap()
            .values()
            .filter(|e| !e.exhausted(self.max_retries))
            .cloned()
            .collect()
    }

    /// 检查 nonce 缓存是否过期
    pub fn is_stale(&self) -> bool {
        self.tracker.is_stale()
    }

    /// 强制从链上同步 nonce
    pub fn sync(&self, new_onchain_nonce: u64) {
        self.tracker.sync(new_onchain_nonce);
    }

    /// 紧急重置
    pub fn reset(&self, new_onchain_nonce: u64) {
        self.tracker.reset(new_onchain_nonce);
        self.stuck_txs.write().unwrap().clear();
    }

    /// 从链上 RPC 同步 nonce（需要已挂载 RpcPool）
    ///
    /// 调用 `eth_getTransactionCount(address, "latest")` 获取 confirmed nonce，
    /// 同时获取 pending nonce 检测 stuck tx。
    pub async fn sync_from_chain(&self) -> Result<u64, String> {
        let rpc = self
            .rpc
            .as_ref()
            .ok_or_else(|| "未挂载 RpcPool，无法从链上同步".to_string())?;
        let addr = self
            .wallet_address
            .as_ref()
            .ok_or_else(|| "未设置钱包地址".to_string())?;

        // 获取链上 confirmed nonce
        let onchain = rpc.get_transaction_count(addr).await?;

        // 获取 pending nonce（包含 mempool 中的 tx）
        let pending = rpc.get_pending_transaction_count(addr).await?;

        // 如果有 stuck tx (pending > onchain)，记录差异
        if pending > onchain {
            tracing::info!(
                "nonce_manager: 检测到 mempool 延迟: onchain={}, pending={} (delta={})",
                onchain,
                pending,
                pending - onchain
            );
        }

        self.tracker.sync(onchain);

        Ok(onchain)
    }

    /// 获取当前链上 nonce
    pub fn onchain_nonce(&self) -> u64 {
        self.tracker.onchain_nonce()
    }

    /// 获取 pending nonce 列表
    pub fn pending_nonces(&self) -> Vec<u64> {
        self.tracker.pending_nonces()
    }

    /// 统计报告
    pub fn stats(&self) -> NonceStats {
        let stuck = self.stuck_txs.read().unwrap();
        NonceStats {
            onchain_nonce: self.tracker.onchain_nonce(),
            pending_count: self.tracker.pending_count(),
            stuck_count: stuck.len(),
            stuck_entries: stuck.values().map(|e| (e.nonce, e.rbf_retries)).collect(),
        }
    }
}

// ============================================================================
// NonceStats
// ============================================================================

/// Nonce 管理器统计
#[derive(Debug, Clone)]
pub struct NonceStats {
    pub onchain_nonce: u64,
    pub pending_count: usize,
    pub stuck_count: usize,
    pub stuck_entries: Vec<(u64, u32)>, // (nonce, rbf_retries)
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========== NonceTracker 测试 ==========

    #[test]
    fn test_non_tracker_basic() {
        let tracker = NonceTracker::new(10);
        assert_eq!(tracker.next_nonce(), 10);
        assert_eq!(tracker.next_nonce(), 11);
        assert_eq!(tracker.next_nonce(), 12);
        assert_eq!(tracker.pending_count(), 3);
    }

    #[test]
    fn test_non_tracker_confirm() {
        let tracker = NonceTracker::new(10);
        let n1 = tracker.next_nonce(); // 10
        let n2 = tracker.next_nonce(); // 11
        tracker.confirm(n1);
        tracker.confirm(n2);
        assert_eq!(tracker.onchain_nonce(), 12); // n2+1
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn test_non_tracker_mark_failed() {
        let tracker = NonceTracker::new(10);
        let n = tracker.next_nonce();
        tracker.mark_failed(n);
        assert_eq!(tracker.pending_count(), 0);
        // 链上 nonce 不变（仍为 10）
        assert_eq!(tracker.onchain_nonce(), 10);
    }

    #[test]
    fn test_non_tracker_sync() {
        let tracker = NonceTracker::new(10);
        tracker.sync(20);
        assert_eq!(tracker.onchain_nonce(), 20);
        // 同步后获取 nonce 应基于新值
        let n = tracker.next_nonce();
        assert_eq!(n, 20);
    }

    #[test]
    fn test_non_tracker_reset() {
        let tracker = NonceTracker::new(10);
        let _ = tracker.next_nonce(); // 10
        let _ = tracker.next_nonce(); // 11
        tracker.reset(50);
        assert_eq!(tracker.onchain_nonce(), 50);
        assert_eq!(tracker.pending_count(), 0);
        assert_eq!(tracker.next_nonce(), 50);
    }

    #[test]
    fn test_non_tracker_pending_list() {
        let tracker = NonceTracker::new(5);
        let _ = tracker.next_nonce(); // 5
        let _ = tracker.next_nonce(); // 6
        let _ = tracker.next_nonce(); // 7
        let pending = tracker.pending_nonces();
        assert_eq!(pending, vec![5, 6, 7]);
    }

    // ========== StuckTxEntry 测试 ==========

    #[test]
    fn test_stuck_entry_rbf_gas() {
        let entry = StuckTxEntry {
            nonce: 1,
            tx_hash: "0xabc".to_string(),
            sent_at: Instant::now(),
            rbf_retries: 0,
            gas_price: 10_000_000_000u64, // 10 gwei
        };
        let bumped = entry.rbf_gas_price();
        assert!(bumped >= 11_000_000_000u64); // 10 * 1.125 = 11.25
    }

    #[test]
    fn test_stuck_entry_exhausted() {
        let entry = StuckTxEntry {
            nonce: 1,
            tx_hash: "0x".to_string(),
            sent_at: Instant::now(),
            rbf_retries: 5,
            gas_price: 10,
        };
        assert!(entry.exhausted(5));
        assert!(!entry.exhausted(6));
    }

    // ========== NonceManager 测试 ==========

    #[test]
    fn test_manager_basic_flow() {
        let mgr = NonceManager::new(100);
        let n = mgr.next_nonce();
        assert_eq!(n, 100);
        mgr.register_tx(n, "0x1", 30_000_000_000);
        mgr.confirm(n);
        assert_eq!(mgr.onchain_nonce(), 101);
    }

    #[test]
    fn test_manager_pending_nonces() {
        let mgr = NonceManager::new(200);
        let n1 = mgr.next_nonce();
        let n2 = mgr.next_nonce();
        assert_eq!(mgr.pending_nonces(), vec![200, 201]);
        mgr.confirm(n1);
        assert_eq!(mgr.pending_nonces(), vec![201]);
    }

    #[test]
    fn test_manager_rbf_bump() {
        let mgr = NonceManager::new(300);
        let n = mgr.next_nonce();
        mgr.register_tx(n, "0xrbf", 20_000_000_000);
        let new_gas = mgr.bump_nonce(n, 25_000_000_000);
        assert_eq!(new_gas, Some(25_000_000_000));

        let stats = mgr.stats();
        let entry = stats.stuck_entries.iter().find(|(nonce, _)| *nonce == n);
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().1, 1); // rbf_retries = 1
    }

    #[test]
    fn test_manager_rbf_exhausted() {
        let mgr = NonceManager::new(400).with_max_retries(3);
        let n = mgr.next_nonce();
        mgr.register_tx(n, "0xexh", 10_000_000_000);
        // bump 3 times (max retries)
        assert!(mgr.bump_nonce(n, 12_000_000_000).is_some());
        assert!(mgr.bump_nonce(n, 14_000_000_000).is_some());
        assert!(mgr.bump_nonce(n, 16_000_000_000).is_some());
        // 第4次返回 None
        assert!(mgr.bump_nonce(n, 18_000_000_000).is_none());
    }

    #[test]
    fn test_manager_tick_no_stuck() {
        let mgr = NonceManager::new(500).with_stuck_timeout(Duration::from_secs(3600));
        let n = mgr.next_nonce();
        mgr.register_tx(n, "0xnostuck", 10);
        let stuck = mgr.tick();
        assert!(stuck.is_empty());
    }

    #[test]
    fn test_manager_sync_and_reset() {
        let mgr = NonceManager::new(600);
        let _ = mgr.next_nonce();
        mgr.sync(700);
        assert_eq!(mgr.onchain_nonce(), 700);

        mgr.reset(999);
        assert_eq!(mgr.onchain_nonce(), 999);
        assert_eq!(mgr.pending_nonces().len(), 0);
    }

    #[test]
    fn test_manager_stats() {
        let mgr = NonceManager::new(800);
        let _ = mgr.next_nonce(); // 800
        mgr.register_tx(800, "0xstats", 10);
        let stats = mgr.stats();
        assert_eq!(stats.onchain_nonce, 800);
        assert_eq!(stats.pending_count, 1);
        assert_eq!(stats.stuck_count, 1);
    }

    // ========== P0-010：并发 nonce 分配原子 ==========

    #[test]
    fn test_nonce_concurrent_allocation_unique() {
        let tracker = Arc::new(NonceTracker::new(0));
        let results = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut handles = Vec::new();

        for _ in 0..8 {
            let t = tracker.clone();
            let r = results.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    let n = t.next_nonce();
                    r.lock().unwrap().push(n);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let mut all = results.lock().unwrap().clone();
        all.sort_unstable();
        let mut dedup = all.clone();
        dedup.dedup();
        assert_eq!(all.len(), dedup.len(), "并发分配出现重复 nonce");
        assert_eq!(all.len(), 800);
        assert_eq!(all.first(), Some(&0));
        assert_eq!(all.last(), Some(&799));
    }

    // ========== P0-011：Nonce 状态机 ==========

    #[test]
    fn test_nonce_state_machine_transitions() {
        let tracker = NonceTracker::new(10);
        let n = tracker.next_nonce(); // 10, Allocated
        assert_eq!(tracker.nonce_state(n), Some(NonceState::Allocated));

        tracker.mark_pending(n); // Allocated -> Pending
        assert_eq!(tracker.nonce_state(n), Some(NonceState::Pending));

        tracker.confirm(n); // Pending -> Confirmed（移除）
        assert_eq!(tracker.nonce_state(n), None);
        assert_eq!(tracker.onchain_nonce(), 11);
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn test_nonce_out_of_order_confirm_no_skip() {
        let tracker = NonceTracker::new(10);
        let n1 = tracker.next_nonce(); // 10
        let n2 = tracker.next_nonce(); // 11
        let n3 = tracker.next_nonce(); // 12

        // 乱序确认：先确认 n3，n1/n2 仍 pending
        tracker.confirm(n3);
        assert_eq!(tracker.pending_nonces(), vec![n1, n2]);

        // 分配游标与确认解耦：下一 nonce = 13，不跳过 pending、不重复已确认
        let n4 = tracker.next_nonce();
        assert_eq!(n4, 13);
        assert_eq!(tracker.pending_nonces(), vec![n1, n2, n4]);
    }

    #[test]
    fn test_nonce_sync_advances_allocator() {
        let tracker = NonceTracker::new(10);
        let _ = tracker.next_nonce(); // 10
        let _ = tracker.next_nonce(); // 11
        tracker.sync(100); // 链上前进到 100
        let n = tracker.next_nonce();
        assert!(n >= 100, "sync 后分配游标应同步到链上 nonce");
    }
}
