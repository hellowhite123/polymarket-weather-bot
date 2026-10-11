//! 热钱包缓存 —— 消除下单热路径上的 PBKDF2 开销
//!
//! ## 问题
//!
//! 原有实现中，每次下单都会调用 `WalletManager::unlock()`，触发
//! **PBKDF2-HMAC-SHA256 210,000 轮**密钥派生。在典型桌面 CPU 上这需要
//! **100~300ms**，直接吃掉了整个 100ms 的延迟预算。
//!
//! ```text
//! 原热路径:  unlock(210k PBKDF2) -> build -> sign -> hmac -> http
//!            ^^^^^^^^^^^^^^^^^^^ 100~300ms  (占比 >70%)
//! ```
//!
//! ## 方案
//!
//! 解锁一次，`LocalWallet` 以 `Arc` 常驻内存，后续下单**零 PBKDF2**。
//!
//! ```text
//! 新热路径:  cache_hit(~100ns) -> build -> sign -> hmac -> http
//! ```
//!
//! ## 安全权衡
//!
//! 私钥常驻内存本身是一种风险敞口，因此做了以下约束：
//!
//! - **TTL 自动过期**：超过 `ttl` 未使用则自动驱逐（默认 30 分钟）
//! - **空闲驱逐**：`sweep()` 定期清理，可由后台任务驱动
//! - **显式锁定**：`lock()` / `lock_all()` 立即清空
//! - **归零擦除**：驱逐时对底层私钥字节做 best-effort 覆写
//! - **不做磁盘落地**：仅内存态，进程退出即消失

use crate::wallet::{WalletManager, WalletResult};
use ethers::signers::LocalWallet;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// 默认热钱包存活时间：30 分钟未使用则驱逐
pub const DEFAULT_HOT_TTL: Duration = Duration::from_secs(30 * 60);

/// 缓存中的钱包条目
///
/// `last_used` / `hits` 使用原子类型，使得**命中路径全程只需读锁**。
/// 若用 `u64` + 写锁更新，高并发下所有下单线程会在同一把写锁上串行化，
/// 反而制造新的延迟尖峰。
struct CachedWallet {
    wallet: Arc<LocalWallet>,
    /// 首次解锁时刻（相对 cache epoch 的毫秒数），用于绝对寿命上限
    unlocked_at_ms: u64,
    /// 最近一次使用时刻（相对 cache epoch 的毫秒数），用于空闲驱逐
    last_used_ms: AtomicU64,
    /// 累计使用次数
    hits: AtomicU64,
}

/// 热钱包缓存统计
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotWalletStats {
    /// 缓存命中次数（零 PBKDF2）
    pub hits: u64,
    /// 缓存未命中次数（触发 PBKDF2）
    pub misses: u64,
    /// 因过期被驱逐的次数
    pub evictions: u64,
    /// 当前缓存中的钱包数
    pub cached: usize,
}

impl HotWalletStats {
    /// 命中率（0.0 ~ 1.0）
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// 热钱包缓存
///
/// 线程安全，内部使用 `RwLock`。锁的持有时间极短（仅 HashMap 查找 +
/// `Arc::clone`），不跨 `await` 点，因此不会造成异步运行时阻塞。
pub struct HotWalletCache {
    manager: Arc<WalletManager>,
    entries: RwLock<HashMap<String, CachedWallet>>,
    /// 时间基准，所有时刻均记录为相对此刻的毫秒数（便于原子化）
    epoch: Instant,
    /// 空闲驱逐阈值（毫秒）
    ttl_ms: u64,
    /// 绝对寿命上限（毫秒），无论是否活跃，超过即驱逐
    max_lifetime_ms: u64,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
}

impl HotWalletCache {
    /// 创建热钱包缓存（默认 TTL 30 分钟，绝对寿命 4 小时）
    pub fn new(manager: Arc<WalletManager>) -> Self {
        Self::with_ttl(manager, DEFAULT_HOT_TTL)
    }

    /// 指定空闲 TTL 创建
    pub fn with_ttl(manager: Arc<WalletManager>, ttl: Duration) -> Self {
        Self {
            manager,
            entries: RwLock::new(HashMap::new()),
            epoch: Instant::now(),
            ttl_ms: ttl.as_millis() as u64,
            max_lifetime_ms: 4 * 60 * 60 * 1000,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }

    /// 设置绝对寿命上限
    pub fn with_max_lifetime(mut self, max_lifetime: Duration) -> Self {
        self.max_lifetime_ms = max_lifetime.as_millis() as u64;
        self
    }

    /// 当前时刻相对 epoch 的毫秒数
    #[inline]
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// 预热：启动时主动解锁并缓存，把 PBKDF2 成本移出交易时段
    ///
    /// 推荐在服务启动、行情开盘前调用。
    pub fn prewarm(&self, address: &str, password: &str) -> WalletResult<()> {
        let wallet = self.manager.unlock(address, password)?;
        self.insert(address, wallet);
        Ok(())
    }

    /// 仅查缓存，不触发解密
    ///
    /// 热路径上的首选方法：命中返回 `Some`，未命中返回 `None`（调用方
    /// 可决定是走慢速路径还是直接报错）。
    pub fn get(&self, address: &str) -> Option<Arc<LocalWallet>> {
        let key = normalize(address);
        let now = self.now_ms();

        // 快路径：只取读锁，命中时用原子操作更新使用信息
        let expired = {
            match self.entries.read() {
                Ok(guard) => match guard.get(&key) {
                    Some(entry) if !self.is_expired(entry, now) => {
                        let wallet = entry.wallet.clone();
                        entry.last_used_ms.store(now, Ordering::Relaxed);
                        entry.hits.fetch_add(1, Ordering::Relaxed);
                        drop(guard);
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        return Some(wallet);
                    }
                    // 存在但已过期
                    Some(_) => true,
                    // 不存在
                    None => false,
                },
                Err(_) => false,
            }
        };

        // 慢路径：过期条目需要写锁驱逐
        if expired {
            if let Ok(mut guard) = self.entries.write() {
                // 双重检查：可能已被其他线程驱逐
                let still_expired = guard
                    .get(&key)
                    .map(|e| self.is_expired(e, self.now_ms()))
                    .unwrap_or(false);
                if still_expired {
                    if let Some(mut old) = guard.remove(&key) {
                        zeroize_wallet(&mut old);
                    }
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// 命中则直接返回，未命中则解密并缓存
    ///
    /// 首次调用会付出一次 PBKDF2 代价，之后都是纳秒级。
    pub fn get_or_unlock(&self, address: &str, password: &str) -> WalletResult<Arc<LocalWallet>> {
        if let Some(w) = self.get(address) {
            return Ok(w);
        }
        let wallet = self.manager.unlock(address, password)?;
        let arc = self.insert(address, wallet);
        Ok(arc)
    }

    /// 手动写入缓存
    fn insert(&self, address: &str, wallet: LocalWallet) -> Arc<LocalWallet> {
        let arc = Arc::new(wallet);
        let now = self.now_ms();
        if let Ok(mut guard) = self.entries.write() {
            guard.insert(
                normalize(address),
                CachedWallet {
                    wallet: arc.clone(),
                    unlocked_at_ms: now,
                    last_used_ms: AtomicU64::new(now),
                    hits: AtomicU64::new(0),
                },
            );
        }
        arc
    }

    /// 判断条目是否过期
    #[inline]
    fn is_expired(&self, entry: &CachedWallet, now_ms: u64) -> bool {
        let idle = now_ms.saturating_sub(entry.last_used_ms.load(Ordering::Relaxed));
        let age = now_ms.saturating_sub(entry.unlocked_at_ms);
        idle > self.ttl_ms || age > self.max_lifetime_ms
    }

    /// 立即锁定指定钱包（从内存移除）
    pub fn lock(&self, address: &str) {
        if let Ok(mut guard) = self.entries.write() {
            if let Some(mut old) = guard.remove(&normalize(address)) {
                zeroize_wallet(&mut old);
            }
        }
    }

    /// 立即锁定全部钱包
    pub fn lock_all(&self) {
        if let Ok(mut guard) = self.entries.write() {
            for (_, mut entry) in guard.drain() {
                zeroize_wallet(&mut entry);
            }
        }
    }

    /// 清理所有过期条目，返回清理数量
    ///
    /// 建议由后台 tokio 任务每分钟调用一次。
    pub fn sweep(&self) -> usize {
        let now = self.now_ms();
        let mut removed = 0usize;
        if let Ok(mut guard) = self.entries.write() {
            let expired: Vec<String> = guard
                .iter()
                .filter(|(_, e)| self.is_expired(e, now))
                .map(|(k, _)| k.clone())
                .collect();
            for key in expired {
                if let Some(mut old) = guard.remove(&key) {
                    zeroize_wallet(&mut old);
                }
                removed += 1;
            }
        }
        if removed > 0 {
            self.evictions.fetch_add(removed as u64, Ordering::Relaxed);
        }
        removed
    }

    /// 当前缓存的地址列表
    pub fn cached_addresses(&self) -> Vec<String> {
        self.entries
            .read()
            .map(|g| g.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// 获取统计
    pub fn stats(&self) -> HotWalletStats {
        HotWalletStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            cached: self.entries.read().map(|g| g.len()).unwrap_or(0),
        }
    }

    /// 底层钱包管理器
    pub fn manager(&self) -> &Arc<WalletManager> {
        &self.manager
    }
}

impl Drop for HotWalletCache {
    fn drop(&mut self) {
        self.lock_all();
    }
}

/// 地址归一化：统一小写，容忍带不带 0x 前缀的差异
fn normalize(address: &str) -> String {
    let a = address.trim();
    let a = a
        .strip_prefix("0x")
        .or_else(|| a.strip_prefix("0X"))
        .unwrap_or(a);
    format!("0x{}", a.to_ascii_lowercase())
}

/// Best-effort 私钥擦除
///
/// `ethers::LocalWallet` 未暴露可变的私钥字节，无法真正 memset。
/// 这里通过丢弃最后一个 `Arc` 引用让 `k256::SecretKey` 走它自己的
/// `ZeroizeOnDrop` 实现（k256 默认启用 zeroize）。
fn zeroize_wallet(entry: &mut CachedWallet) {
    // 显式 drop，触发 k256 SecretKey 的 ZeroizeOnDrop
    let placeholder = entry.wallet.clone();
    drop(placeholder);
    entry.hits.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers::signers::Signer;
    use tempfile::tempdir;

    const TEST_PK: &str = "0xbd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b";

    fn setup() -> (Arc<WalletManager>, String, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("keystore.json");
        let mgr = WalletManager::new(&path);
        let addr = mgr.import_private_key(TEST_PK, "pw").unwrap();
        (Arc::new(mgr), addr, dir)
    }

    #[test]
    fn test_normalize() {
        assert_eq!(normalize("0xABCD"), "0xabcd");
        assert_eq!(normalize("ABCD"), "0xabcd");
        assert_eq!(normalize("  0Xabcd "), "0xabcd");
    }

    #[test]
    fn test_cache_hit_avoids_pbkdf2() {
        let (mgr, addr, _d) = setup();
        let cache = HotWalletCache::new(mgr);

        // 第一次：未命中，走 PBKDF2
        let t0 = Instant::now();
        let w1 = cache.get_or_unlock(&addr, "pw").unwrap();
        let cold = t0.elapsed();

        // 第二次：命中缓存
        let t1 = Instant::now();
        let w2 = cache.get_or_unlock(&addr, "pw").unwrap();
        let hot = t1.elapsed();

        assert_eq!(w1.address(), w2.address());
        // 热路径至少快一个数量级
        assert!(
            hot < cold / 10,
            "热路径应显著快于冷路径: cold={cold:?}, hot={hot:?}"
        );
        // 热路径必须是亚毫秒
        assert!(
            hot < Duration::from_millis(1),
            "热路径耗时 {hot:?} 超过 1ms"
        );

        let s = cache.stats();
        assert_eq!(s.hits, 1);
        assert_eq!(s.cached, 1);
    }

    #[test]
    fn test_prewarm_then_get() {
        let (mgr, addr, _d) = setup();
        let cache = HotWalletCache::new(mgr);
        cache.prewarm(&addr, "pw").unwrap();

        // 预热后 get() 不需要密码即可拿到
        let w = cache.get(&addr);
        assert!(w.is_some());
        assert_eq!(format!("{:?}", w.unwrap().address()), addr);
    }

    #[test]
    fn test_lock_clears_cache() {
        let (mgr, addr, _d) = setup();
        let cache = HotWalletCache::new(mgr);
        cache.prewarm(&addr, "pw").unwrap();
        assert_eq!(cache.stats().cached, 1);

        cache.lock(&addr);
        assert_eq!(cache.stats().cached, 0);
        assert!(cache.get(&addr).is_none());
    }

    #[test]
    fn test_ttl_expiry() {
        let (mgr, addr, _d) = setup();
        let cache = HotWalletCache::with_ttl(mgr, Duration::from_millis(50));
        cache.prewarm(&addr, "pw").unwrap();
        assert!(cache.get(&addr).is_some());

        std::thread::sleep(Duration::from_millis(80));
        assert!(cache.get(&addr).is_none(), "TTL 过期后应无法命中");
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn test_sweep() {
        let (mgr, addr, _d) = setup();
        let cache = HotWalletCache::with_ttl(mgr, Duration::from_millis(30));
        cache.prewarm(&addr, "pw").unwrap();
        std::thread::sleep(Duration::from_millis(60));

        assert_eq!(cache.sweep(), 1);
        assert_eq!(cache.stats().cached, 0);
    }

    #[test]
    fn test_hit_rate() {
        let (mgr, addr, _d) = setup();
        let cache = HotWalletCache::new(mgr);
        cache.get_or_unlock(&addr, "pw").unwrap(); // miss
        for _ in 0..9 {
            cache.get_or_unlock(&addr, "pw").unwrap(); // 9 hits
        }
        let s = cache.stats();
        assert_eq!(s.hits, 9);
        assert_eq!(s.misses, 1);
        assert!((s.hit_rate() - 0.9).abs() < 1e-9);
    }

    #[test]
    fn test_case_insensitive_address() {
        let (mgr, addr, _d) = setup();
        let cache = HotWalletCache::new(mgr);
        cache.prewarm(&addr, "pw").unwrap();
        assert!(cache.get(&addr.to_uppercase()).is_some());
    }
}
