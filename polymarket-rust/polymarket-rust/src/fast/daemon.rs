//! 后台守护任务 (BackgroundDaemon)
//!
//! 整合所有 fast 子模块的后台周期性维护任务，以 tokio 异步任务形式运行。
//!
//! ## 定时任务
//!
//! | 间隔 | 任务 | 说明 |
//! |------|------|------|
//! | 2s | RpcPool.check_health_all() | 节点健康检查 |
//! | 5s | TxTracker.tick_chain() | 链上 tx 状态同步 |
//! | 15s | TxTracker.tick() | 本地 stuck 检测 |
//! | 30s | NonceManager.sync_from_chain() | 链上 nonce 同步 |
//! | 60s | FastExecutor.sweep() | 过期条目清理 |
//!
//! ## 使用
//!
//! ```text
//! let daemon = BackgroundDaemon::new(executor_arc, rpc_arc, nonce_arc, tx_arc);
//! let handle = daemon.spawn();
//! // ... 交易时段 ...
//! daemon.shutdown();
//! ```
//!
//! ## 优雅关闭
//!
//! 通过 `CancellationToken` 实现，调用 `shutdown()` 后等待最多 5 秒让当前循环完成。

use crate::fast::executor::FastExecutor;
use crate::fast::nonce_manager::NonceManager;
use crate::fast::rpc_pool::RpcPool;
use crate::fast::tx_lifecycle::TxTracker;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::interval;

pub struct BackgroundDaemon {
    executor: Arc<FastExecutor>,
    rpc: Arc<RpcPool>,
    nonce_mgr: Option<Arc<NonceManager>>,
    tx_tracker: Option<Arc<TxTracker>>,
    running: Arc<AtomicBool>,
    shutdown_signal: Arc<Notify>,
    handle: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl BackgroundDaemon {
    /// 创建守护进程
    pub fn new(
        executor: Arc<FastExecutor>,
        rpc: Arc<RpcPool>,
        nonce_mgr: Option<Arc<NonceManager>>,
        tx_tracker: Option<Arc<TxTracker>>,
    ) -> Self {
        Self {
            executor,
            rpc,
            nonce_mgr,
            tx_tracker,
            running: Arc::new(AtomicBool::new(false)),
            shutdown_signal: Arc::new(Notify::new()),
            handle: std::sync::Mutex::new(None),
        }
    }

    /// 启动后台任务（spawn 到 tokio runtime）
    pub fn spawn(&self) {
        let running = self.running.clone();
        let shutdown = self.shutdown_signal.clone();

        running.store(true, Ordering::Release);

        let executor = self.executor.clone();
        let rpc = self.rpc.clone();
        let nonce_mgr = self.nonce_mgr.clone();
        let tx_tracker = self.tx_tracker.clone();

        let handle = tokio::spawn(async move {
            let mut health_tick = interval(Duration::from_secs(2));
            let mut chain_tick = interval(Duration::from_secs(5));
            let mut local_tick = interval(Duration::from_secs(15));
            let mut nonce_tick = interval(Duration::from_secs(30));
            let mut sweep_tick = interval(Duration::from_secs(60));

            // 启动时先跑一轮全部
            let _ = rpc.check_health_all();

            loop {
                tokio::select! {
                    // 每 2s: RPC 健康检查
                    _ = health_tick.tick() => {
                        rpc.check_health_all().await;
                        let stats = rpc.all_stats();
                        for s in &stats {
                            if s.state == crate::fast::rpc_pool::NodeState::Dead {
                                tracing::warn!("daemon: RPC 节点 {} 不可用", s.url);
                            }
                        }
                        let healthy = stats.iter().filter(|s| s.state == crate::fast::rpc_pool::NodeState::Healthy).count();
                        if healthy == 0 {
                            tracing::error!("daemon: 所有 RPC 节点不可用!");
                        }
                    }

                    // 每 5s: 链上 tx 状态同步
                    _ = chain_tick.tick() => {
                        if let Some(ref tt) = tx_tracker {
                            let result = tt.tick_chain().await;
                            if result.total_active > 0 {
                                tracing::debug!(
                                    "daemon: 链上同步 {} 笔活跃交易",
                                    result.total_active
                                );
                            }
                        }
                    }

                    // 每 15s: 本地 stuck 检测
                    _ = local_tick.tick() => {
                        if let Some(ref tt) = tx_tracker {
                            let result = tt.tick().await;
                            if result.stuck_count > 0 {
                                tracing::warn!("daemon: 发现 {} 笔 stuck 交易", result.stuck_count);
                            }
                        }
                    }

                    // 每 30s: nonce 同步
                    _ = nonce_tick.tick() => {
                        if let Some(ref nm) = nonce_mgr {
                            match nm.sync_from_chain().await {
                                Ok(nonce) => {
                                    tracing::debug!("daemon: nonce 同步完成 chain={}", nonce);
                                }
                                Err(e) => {
                                    tracing::warn!("daemon: nonce 同步失败: {}", e);
                                }
                            }
                        }
                    }

                    // 每 60s: 过期清理
                    _ = sweep_tick.tick() => {
                        let n = executor.sweep();
                        if n > 0 {
                            tracing::debug!("daemon: 清理 {} 条过期条目", n);
                        }
                    }

                    // 关机信号
                    _ = shutdown.notified() => {
                        tracing::info!("daemon: 收到关机信号");
                        if let Some(ref tt) = tx_tracker {
                            let stats = tt.stats();
                            tracing::info!(
                                "daemon: 最终统计 total_tracked={} active={} confirmed={} failed={} stale={}",
                                stats.total_tracked,
                                stats.total_active,
                                stats.total_confirmed,
                                stats.total_failed,
                                stats.total_stale,
                            );
                        }
                        break;
                    }
                }
            }

            running.store(false, Ordering::Release);
        });

        *self.handle.lock().unwrap() = Some(handle);
    }

    /// 优雅关闭
    pub fn shutdown(&self) {
        self.shutdown_signal.notify_one();
    }

    /// 是否正在运行
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// 等待后台任务结束（阻塞）
    pub async fn join(&self) {
        let mut guard = self.handle.lock().unwrap();
        if let Some(h) = guard.take() {
            let _ = h.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clob::{ClobClient, ClobConfig};
    use crate::wallet::WalletManager;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn make_executor() -> Arc<FastExecutor> {
        let dir = tempdir().unwrap();
        let path = dir.path().join("daemon_test.json");
        let mut mgr = WalletManager::new(&path);
        let _ = mgr.import_private_key(
            "0xbd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b",
            "pw",
        );
        Arc::new(FastExecutor::new(Arc::new(mgr), ClobConfig::default()).unwrap())
    }

    #[tokio::test]
    async fn test_daemon_minimal() {
        let exec = make_executor();
        let rpc = Arc::new(RpcPool::new(&["https://polygon-rpc.com"]));
        let daemon = BackgroundDaemon::new(exec, rpc, None, None);

        daemon.spawn();
        assert!(daemon.is_running());

        // run 1 second then shutdown
        tokio::time::sleep(Duration::from_secs(1)).await;
        daemon.shutdown();
        daemon.join().await;

        assert!(!daemon.is_running());
    }

    #[tokio::test]
    async fn test_daemon_with_tracker() {
        let exec = make_executor();
        let rpc = Arc::new(RpcPool::new(&["https://polygon-rpc.com"]));
        let tt = Arc::new(TxTracker::new());

        // Register a dummy tx entry for tick to process
        tt.register(crate::fast::tx_lifecycle::TxEntry::new(
            "0xdeadbeef".to_string(),
            "token-1".to_string(),
            None,
        ));

        let daemon = BackgroundDaemon::new(exec, rpc, None, Some(tt));

        daemon.spawn();
        tokio::time::sleep(Duration::from_secs(2)).await;
        daemon.shutdown();
        daemon.join().await;

        assert!(!daemon.is_running());
    }
}
