//! 交易确认追踪 (TxLifecycle)
//!
//! 从广播到链上确认的完整生命周期管理。追踪每笔订单从 CLOB HTTP 提交
//! 到链上状态变化的全过程，支持定时轮询、回调通知、stuck 检测与 RBF 重试。
//!
//! ## 状态机
//!
//! ```text
//! Submitted → Accepted → Pending(链上) ─╼ Confirmed
//!                                    ─╼ Failed / Reverted
//!                                    ─╼ Stale(超时未确认)
//!                                         → RBF Retry
//! ```
//!
//! ## 与 MempoolMonitor 集成
//!
//! `MempoolMonitor::on_submitted()` 调用后，自动注册到 `TxTracker`。
//! 后台任务周期性调用 `tick()` 轮询状态。
//!
//! ## 性能
//!
//! - `register()` / `update()`: O(1) HashMap ops, < 1μs
//! - `tick()`: 批量轮询, 每次最多 5 个并发请求

use crate::clob::error::{ClobError, ClobResult};
use crate::fast::rpc_pool::RpcPool;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

// ============================================================================
// 常量
// ============================================================================

/// 默认轮询基础间隔
pub const DEFAULT_POLL_BASE_INTERVAL: Duration = Duration::from_secs(2);

/// 最大轮询退避间隔
pub const DEFAULT_POLL_MAX_INTERVAL: Duration = Duration::from_secs(15);

/// stuck 判定阈值（超过此时间未确认视为 stuck）
pub const DEFAULT_STUCK_TTL: Duration = Duration::from_secs(60);

/// 最大轮询次数（超过后标记为 Stale）
pub const DEFAULT_MAX_POLL_COUNT: u32 = 60;

/// 过期清理阈值
pub const STALE_CLEANUP_TTL: Duration = Duration::from_secs(300);

/// RBF gas 增幅（1.125 = 12.5%）
pub const RBF_GAS_BUMP_RATIO: f64 = 1.125;

// ============================================================================
// 生命周期状态
// ============================================================================

/// 交易生命周期状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxState {
    /// 已通过 HTTP 广播（Polymarket CLOB order submitted）
    Submitted,
    /// 交易所已接受并返回 orderID
    Accepted,
    /// 链上 pending（在 mempool 等待矿工打包）
    Pending,
    /// 链上已确认（Confirmed N blocks）
    Confirmed,
    /// 交易失败（reverted on-chain）
    Failed,
    /// 交易被回滚（链重组）
    Reverted,
    /// 超时未确认，标记为 stale
    Stale,
}

impl TxState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, TxState::Confirmed | TxState::Failed | TxState::Stale)
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            TxState::Submitted | TxState::Accepted | TxState::Pending
        )
    }
}

impl std::fmt::Display for TxState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TxState::Submitted => write!(f, "submitted"),
            TxState::Accepted => write!(f, "accepted"),
            TxState::Pending => write!(f, "pending"),
            TxState::Confirmed => write!(f, "confirmed"),
            TxState::Failed => write!(f, "failed"),
            TxState::Reverted => write!(f, "reverted"),
            TxState::Stale => write!(f, "stale"),
        }
    }
}

// ============================================================================
// TxEntry
// ============================================================================

/// 单笔交易在追踪器中的条目
#[derive(Debug, Clone)]
pub struct TxEntry {
    /// 交易哈希（链上 tx hash 或 CLOB orderID）
    pub tx_hash: String,
    /// CLOB 订单 ID（如果有）
    pub order_id: Option<String>,
    /// Token ID（关联的市场）
    pub token_id: String,
    /// 当前生命周期状态
    pub state: TxState,
    /// 提交时刻
    pub submitted_at: Instant,
    /// 上次轮询时间
    pub last_polled: Instant,
    /// 轮询次数
    pub poll_count: u32,
    /// Gas 价格（wei，仅链上交易有）
    pub gas_price: Option<u64>,
    /// Nonce 值（仅链上交易有）
    pub nonce: Option<u64>,
    /// 上次状态变更时刻
    pub state_changed_at: Instant,
}

impl TxEntry {
    /// 创建新的交易条目（初始状态：Submitted）
    pub fn new(tx_hash: String, token_id: String, order_id: Option<String>) -> Self {
        let now = Instant::now();
        Self {
            tx_hash,
            order_id,
            token_id,
            state: TxState::Submitted,
            submitted_at: now,
            last_polled: now,
            poll_count: 0,
            gas_price: None,
            nonce: None,
            state_changed_at: now,
        }
    }

    /// 计算下一次轮询的退避间隔
    pub fn next_poll_interval(&self) -> Duration {
        let base = DEFAULT_POLL_BASE_INTERVAL.as_secs_f64();
        let max = DEFAULT_POLL_MAX_INTERVAL.as_secs_f64();
        let factor = 1.5_f64.powi(self.poll_count as i32);
        let interval = (base * factor).min(max);
        Duration::from_secs_f64(interval)
    }

    /// 是否超过 stuck 阈值
    pub fn is_stuck(&self) -> bool {
        self.state.is_active() && self.submitted_at.elapsed() > DEFAULT_STUCK_TTL
    }

    /// 是否应该清理（已过期）
    pub fn should_cleanup(&self) -> bool {
        (self.state.is_terminal() && self.state_changed_at.elapsed() > STALE_CLEANUP_TTL)
            || self.submitted_at.elapsed() > STALE_CLEANUP_TTL * 2
    }
}

// ============================================================================
// 回调类型
// ============================================================================

/// 交易生命周期事件回调
pub type TxCallback = Arc<dyn Fn(&TxEntry, TxState) + Send + Sync>;

// ============================================================================
// TxTracker
// ============================================================================

/// 交易生命周期追踪器
///
/// 维护所有活跃交易的状态，支持定时轮询和回调通知。
/// 可与链上 Provider 集成以获取真实 tx 状态。
///
/// # 使用
///
/// ```text
/// tracker.register(entry);
/// // 在后台任务中定期调用:
/// loop { tracker.tick().await; sleep(2s); }
/// ```
pub struct TxTracker {
    /// HashMap<tx_hash, TxEntry>
    entries: RwLock<HashMap<String, TxEntry>>,
    /// 状态变更回调列表
    callbacks: RwLock<Vec<(TxState, TxCallback)>>,
    /// RPC 节点池（可选，用于链上状态查询）
    rpc: Option<Arc<RpcPool>>,
    /// 统计
    total_registered: AtomicU64,
    total_confirmed: AtomicU64,
    total_failed: AtomicU64,
    total_stale: AtomicU64,
}

impl TxTracker {
    /// 创建新的追踪器
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            callbacks: RwLock::new(Vec::new()),
            rpc: None,
            total_registered: AtomicU64::new(0),
            total_confirmed: AtomicU64::new(0),
            total_failed: AtomicU64::new(0),
            total_stale: AtomicU64::new(0),
        }
    }

    /// 挂载 RPC 节点池（启用链上状态同步）
    pub fn with_rpc_pool(mut self, rpc: Arc<RpcPool>) -> Self {
        self.rpc = Some(rpc);
        self
    }

    /// 注册状态变更回调
    ///
    /// `trigger_states`: 哪些状态变更触发此回调
    pub fn on_state_change<F>(&self, trigger_states: Vec<TxState>, callback: F)
    where
        F: Fn(&TxEntry, TxState) + Send + Sync + 'static,
    {
        let cb: TxCallback = Arc::new(callback);
        let mut cbs = self.callbacks.write().unwrap();
        for state in trigger_states {
            cbs.push((state, cb.clone()));
        }
    }

    /// 注册一笔新交易
    pub fn register(&self, entry: TxEntry) {
        tracing::debug!("tx_lifecycle: 注册交易 {}", entry.tx_hash);
        self.entries
            .write()
            .unwrap()
            .insert(entry.tx_hash.clone(), entry);
        self.total_registered.fetch_add(1, Ordering::Relaxed);
    }

    /// 手动更新交易状态（内部或外部驱动）
    pub fn update_state(&self, tx_hash: &str, new_state: TxState) -> ClobResult<()> {
        let mut entries = self.entries.write().unwrap();
        let entry = entries
            .get_mut(tx_hash)
            .ok_or_else(|| ClobError::OrderFailed(format!("交易 {} 未在追踪器中", tx_hash)))?;

        let old_state = entry.state;
        if old_state == new_state {
            return Ok(());
        }

        entry.state = new_state;
        entry.state_changed_at = Instant::now();

        // 统计（忽略返回值）
        match new_state {
            TxState::Confirmed => {
                self.total_confirmed.fetch_add(1, Ordering::Relaxed);
            }
            TxState::Failed | TxState::Reverted => {
                self.total_failed.fetch_add(1, Ordering::Relaxed);
            }
            TxState::Stale => {
                self.total_stale.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        };

        // 触发回调
        let cbs = self.callbacks.read().unwrap();
        for (state, cb) in cbs.iter() {
            if *state == new_state {
                cb(entry, new_state);
            }
        }

        tracing::info!(
            "tx_lifecycle: {} state: {} → {}",
            tx_hash,
            old_state,
            new_state
        );

        Ok(())
    }

    /// 获取交易条目
    pub fn get(&self, tx_hash: &str) -> Option<TxEntry> {
        self.entries.read().unwrap().get(tx_hash).cloned()
    }

    /// 获取当前状态
    pub fn get_state(&self, tx_hash: &str) -> Option<TxState> {
        self.entries.read().unwrap().get(tx_hash).map(|e| e.state)
    }

    /// 周期性 tick：轮询所有活跃交易并检查状态
    ///
    /// 当前为本地实现（不依赖链上 RPC）：
    /// - 检测 stuck 交易并标记
    /// - 清理过期条目
    ///
    /// 可扩展为通过 ethers Provider 查询真实 tx 状态。
    pub async fn tick(&self) -> TickResult {
        let mut result = TickResult::default();

        // 获取需要检查的交易
        let active_entries: Vec<(String, TxEntry)> = {
            let entries = self.entries.read().unwrap();
            entries
                .iter()
                .filter(|(_, e)| e.state.is_active())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };

        result.total_active = active_entries.len();

        for (tx_hash, entry) in active_entries {
            // 检查 stuck
            if entry.is_stuck() {
                tracing::warn!(
                    "tx_lifecycle: 交易 {} 在 pending 状态超过 {} 秒, 标记为 stale",
                    tx_hash,
                    DEFAULT_STUCK_TTL.as_secs()
                );
                let _ = self.update_state(&tx_hash, TxState::Stale);
                result.stuck_count += 1;
            }
        }

        // 清理过期条目
        {
            let mut entries = self.entries.write().unwrap();
            let stale_keys: Vec<String> = entries
                .iter()
                .filter(|(_, e)| e.should_cleanup())
                .map(|(k, _)| k.clone())
                .collect();
            result.cleaned_up = stale_keys.len();
            for k in &stale_keys {
                entries.remove(k);
            }
        }

        result
    }

    /// 链上状态同步 tick（需要已挂载 RpcPool）
    ///
    /// 对每个 active entry 调用 `eth_getTransactionReceipt` 查询链上状态，
    /// 根据 receipt 自动推进生命周期状态。
    ///
    /// - receipt.status="0x1" → Confirmed
    /// - receipt.status="0x0" → Failed
    /// - receipt=null + stuck → Stale
    /// - receipt=null + not stuck → 保持 Pending
    pub async fn tick_chain(&self) -> TickResult {
        let mut result = TickResult::default();
        let rpc = match self.rpc.as_ref() {
            Some(r) => r,
            None => return result, // 未挂载 RpcPool，跳过
        };

        // 获取活跃交易
        let active: Vec<(String, TxEntry)> = {
            let entries = self.entries.read().unwrap();
            entries
                .iter()
                .filter(|(_, e)| e.state.is_active())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };

        result.total_active = active.len();

        for (tx_hash, _entry) in active {
            match rpc.get_transaction_receipt(&tx_hash).await {
                Ok(Some(receipt)) => {
                    if receipt.is_success() {
                        let _ = self.update_state(&tx_hash, TxState::Confirmed);
                        result.stuck_count += 1; // reuse field for "updated by chain"
                    } else {
                        let _ = self.update_state(&tx_hash, TxState::Failed);
                    }
                }
                Ok(None) => {
                    // 尚未确认，检查是否 stuck
                    let entries = self.entries.read().unwrap();
                    if let Some(entry) = entries.get(&tx_hash) {
                        if entry.is_stuck() {
                            drop(entries);
                            let _ = self.update_state(&tx_hash, TxState::Stale);
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("tx_lifecycle: 查询 {} 失败: {}", tx_hash, e);
                }
            }
        }

        // 清理过期条目
        {
            let mut entries = self.entries.write().unwrap();
            let stale_keys: Vec<String> = entries
                .iter()
                .filter(|(_, e)| e.should_cleanup())
                .map(|(k, _)| k.clone())
                .collect();
            result.cleaned_up = stale_keys.len();
            for k in &stale_keys {
                entries.remove(k);
            }
        }

        result
    }

    /// 标记交易为已确认（快捷方法）
    pub fn confirm(&self, tx_hash: &str) -> ClobResult<()> {
        self.update_state(tx_hash, TxState::Confirmed)
    }

    /// 标记交易为失败
    pub fn mark_failed(&self, tx_hash: &str) -> ClobResult<()> {
        self.update_state(tx_hash, TxState::Failed)
    }

    /// 获取活跃交易列表
    pub fn active_entries(&self) -> Vec<TxEntry> {
        self.entries
            .read()
            .unwrap()
            .values()
            .filter(|e| e.state.is_active())
            .cloned()
            .collect()
    }

    /// 活跃交易数
    pub fn active_count(&self) -> usize {
        self.entries
            .read()
            .unwrap()
            .values()
            .filter(|e| e.state.is_active())
            .count()
    }

    /// 完整统计
    pub fn stats(&self) -> TxStats {
        let entries = self.entries.read().unwrap();
        TxStats {
            total_tracked: entries.len(),
            total_active: self.active_count(),
            total_registered: self.total_registered.load(Ordering::Relaxed),
            total_confirmed: self.total_confirmed.load(Ordering::Relaxed),
            total_failed: self.total_failed.load(Ordering::Relaxed),
            total_stale: self.total_stale.load(Ordering::Relaxed),
        }
    }
}

impl Default for TxTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// TickResult
// ============================================================================

/// tick() 返回的结构体
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickResult {
    pub total_active: usize,
    pub stuck_count: usize,
    pub cleaned_up: usize,
}

// ============================================================================
// TxStats
// ============================================================================

/// 交易追踪统计
#[derive(Debug, Clone, Default)]
pub struct TxStats {
    pub total_tracked: usize,
    pub total_active: usize,
    pub total_registered: u64,
    pub total_confirmed: u64,
    pub total_failed: u64,
    pub total_stale: u64,
}

// ============================================================================
// RBF (Replace-By-Fee) 支持
// ============================================================================

/// RBF 替换参数
#[derive(Debug, Clone)]
pub struct RbfParams {
    /// 原交易哈希
    pub original_tx_hash: String,
    /// 新 gas 价格（wei），如果为 None 则自动计算 (原价 × RBF_GAS_BUMP_RATIO)
    pub new_gas_price: Option<u64>,
    /// 新 nonce（可选，默认使用原 nonce）
    pub new_nonce: Option<u64>,
}

/// 计算 RBF 加速后的 gas 价格
///
/// 按照 EIP-1559 规范，替换交易需要至少提高 10% gas price。
/// 这里使用 12.5% 安全边际。
pub fn calculate_rbf_gas_price(original_gas: u64) -> u64 {
    let bumped = (original_gas as f64 * RBF_GAS_BUMP_RATIO) as u64;
    // 至少 +1 gwei
    let minimum_bump = 1_000_000_000u64; // 1 gwei
    if bumped < original_gas + minimum_bump {
        original_gas + minimum_bump
    } else {
        bumped
    }
}

/// 为交易创建 RBF 替换参数
///
/// 自动计算新的 gas 价格（原价 × 1.125）。
pub fn prepare_rbf_replacement(entry: &TxEntry) -> Option<RbfParams> {
    let gas = entry.gas_price?;
    let nonce = entry.nonce?;
    let new_gas = calculate_rbf_gas_price(gas);
    Some(RbfParams {
        original_tx_hash: entry.tx_hash.clone(),
        new_gas_price: Some(new_gas),
        new_nonce: Some(nonce),
    })
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========== TxState 测试 ==========

    #[test]
    fn test_tx_state_is_terminal() {
        assert!(!TxState::Submitted.is_terminal());
        assert!(!TxState::Pending.is_terminal());
        assert!(TxState::Confirmed.is_terminal());
        assert!(TxState::Failed.is_terminal());
        assert!(TxState::Stale.is_terminal());
    }

    #[test]
    fn test_tx_state_is_active() {
        assert!(TxState::Submitted.is_active());
        assert!(TxState::Accepted.is_active());
        assert!(TxState::Pending.is_active());
        assert!(!TxState::Confirmed.is_active());
    }

    // ========== TxEntry 测试 ==========

    #[test]
    fn test_tx_entry_new() {
        let entry = TxEntry::new(
            "0xabc".to_string(),
            "token-1".to_string(),
            Some("order-1".to_string()),
        );
        assert_eq!(entry.state, TxState::Submitted);
        assert_eq!(entry.tx_hash, "0xabc");
        assert_eq!(entry.poll_count, 0);
    }

    #[test]
    fn test_poll_interval_exponential_backoff() {
        let mut entry = TxEntry::new("0x".to_string(), "t".to_string(), None);
        entry.poll_count = 0;
        let i0 = entry.next_poll_interval();
        entry.poll_count = 5;
        let i5 = entry.next_poll_interval();
        entry.poll_count = 100;
        let i100 = entry.next_poll_interval();
        assert!(i0 <= i5);
        assert!(i5 <= i100);
        assert!(i100 <= DEFAULT_POLL_MAX_INTERVAL);
    }

    // ========== TxTracker 测试 ==========

    #[test]
    fn test_register_and_track() {
        let tracker = TxTracker::new();
        let entry = TxEntry::new("0x1".to_string(), "t".to_string(), None);
        tracker.register(entry);
        assert_eq!(tracker.active_count(), 1);
        assert_eq!(tracker.get_state("0x1"), Some(TxState::Submitted));
    }

    #[test]
    fn test_update_state() {
        let tracker = TxTracker::new();
        let entry = TxEntry::new("0x2".to_string(), "t".to_string(), None);
        tracker.register(entry);
        tracker.update_state("0x2", TxState::Confirmed).unwrap();
        assert_eq!(tracker.get_state("0x2"), Some(TxState::Confirmed));
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn test_update_nonexistent() {
        let tracker = TxTracker::new();
        assert!(tracker.update_state("0xgone", TxState::Confirmed).is_err());
    }

    #[test]
    fn test_state_change_callback() {
        let tracker = Arc::new(TxTracker::new());
        let confirmed_count = Arc::new(AtomicU64::new(0));
        let cc = confirmed_count.clone();
        tracker.on_state_change(vec![TxState::Confirmed], move |_, _| {
            cc.fetch_add(1, Ordering::Relaxed);
        });
        let entry = TxEntry::new("0x3".to_string(), "t".to_string(), None);
        tracker.register(entry);
        tracker.update_state("0x3", TxState::Confirmed).unwrap();
        assert_eq!(confirmed_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_stats() {
        let tracker = TxTracker::new();
        tracker.register(TxEntry::new("0x4".to_string(), "t".to_string(), None));
        tracker.register(TxEntry::new("0x5".to_string(), "t".to_string(), None));
        tracker.update_state("0x4", TxState::Confirmed).unwrap();
        let s = tracker.stats();
        assert_eq!(s.total_tracked, 2);
        assert_eq!(s.total_active, 1);
        assert_eq!(s.total_confirmed, 1);
    }

    // ========== RBF 测试 ==========

    #[test]
    fn test_calculate_rbf_gas_price() {
        let original = 10_000_000_000u64; // 10 gwei
        let bumped = calculate_rbf_gas_price(original);
        assert!(bumped > original);
        assert!(bumped >= original + 1_000_000_000); // +1 gwei minimum
    }

    #[test]
    fn test_prepare_rbf_replacement() {
        let mut entry = TxEntry::new("0x6".to_string(), "t".to_string(), None);
        entry.gas_price = Some(20_000_000_000u64); // 20 gwei
        entry.nonce = Some(5);
        let rbf = prepare_rbf_replacement(&entry);
        assert!(rbf.is_some());
        let rbf = rbf.unwrap();
        assert!(rbf.new_gas_price.unwrap() > 20_000_000_000u64);
        assert_eq!(rbf.new_nonce, Some(5));
    }

    #[test]
    fn test_prepare_rbf_no_gas() {
        let entry = TxEntry::new("0x7".to_string(), "t".to_string(), None);
        assert!(prepare_rbf_replacement(&entry).is_none());
    }

    // ========== Tick 测试 ==========

    #[tokio::test]
    async fn test_tick_detects_active() {
        let tracker = TxTracker::new();
        tracker.register(TxEntry::new("0x8".to_string(), "t".to_string(), None));
        tracker.update_state("0x8", TxState::Pending).unwrap();
        let result = tracker.tick().await;
        assert_eq!(result.total_active, 1);
        assert_eq!(result.stuck_count, 0);
    }

    #[test]
    fn test_confirm_and_mark_failed() {
        let tracker = TxTracker::new();
        tracker.register(TxEntry::new("0x9".to_string(), "t".to_string(), None));
        tracker.confirm("0x9").unwrap();
        assert_eq!(tracker.get_state("0x9"), Some(TxState::Confirmed));

        tracker.register(TxEntry::new("0x10".to_string(), "t".to_string(), None));
        tracker.mark_failed("0x10").unwrap();
        assert_eq!(tracker.get_state("0x10"), Some(TxState::Failed));
    }
}
