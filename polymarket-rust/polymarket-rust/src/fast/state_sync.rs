//! 状态同步器 (StateSync)
//!
//! 本地订单簿与链上/CLOB 状态定期对账，断线后自动恢复。
//!
//! ## 功能
//!
//! - **持仓对账**：对比本地 position_tracker vs CLOB API `/balance-allowance` + positions
//! - **订单对账**：对比 pending orders vs CLOB `/data/orders`
//! - **断线恢复**：重连后全量拉取并重新对齐
//! - **差异报告**：自动生成 reconciliation report
//!
//! ## 与现有模块集成
//!
//! ```text
//! StateSync
//!   ├─ CLOB API (reqwest) → 获取链下状态
//!   ├─ RiskEngine → 获取本地风控持仓
//!   └─ TxTracker → 获取 pending tx 状态
//! ```

use crate::types::clob_types::Position;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing;

// ============================================================================
// 常量
// ============================================================================

pub const DEFAULT_SYNC_INTERVAL: Duration = Duration::from_secs(30);
pub const DEFAULT_RECONNECT_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
pub const DEFAULT_RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(60);

// ============================================================================
// 对账报告
// ============================================================================

/// 对账差异项
#[derive(Debug, Clone)]
pub struct DiffItem {
    pub token_id: String,
    pub field: String,
    pub local_value: String,
    pub remote_value: String,
    pub severity: DiffSeverity,
}

/// 差异严重度
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffSeverity {
    Info,
    Warning,
    Critical,
}

/// 对账报告
#[derive(Debug, Clone, Default)]
pub struct ReconciliationReport {
    pub timestamp: u64,
    pub in_sync: bool,
    pub diffs: Vec<DiffItem>,
    pub positions_checked: usize,
    pub orders_checked: usize,
}

impl ReconciliationReport {
    pub fn has_critical_diff(&self) -> bool {
        self.diffs.iter().any(|d| matches!(d.severity, DiffSeverity::Critical))
    }
}

// ============================================================================
// SyncState
// ============================================================================

/// 连接状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Disconnected { since: Instant },
    Reconnecting { attempt: u32, next_retry: Instant },
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionState::Connected => write!(f, "connected"),
            ConnectionState::Disconnected { .. } => write!(f, "disconnected"),
            ConnectionState::Reconnecting { attempt, .. } => write!(f, "reconnecting(#{})", attempt),
        }
    }
}

// ============================================================================
// StateSync
// ============================================================================

/// 状态同步器
///
/// 通过 CLOB API 拉取远程状态，与本地追踪器对比，检测差异。
pub struct StateSync {
    /// HTTP 客户端
    client: reqwest::Client,
    /// CLOB host
    host: String,
    /// 连接状态
    conn_state: RwLock<ConnectionState>,
    /// 上次成功同步时间
    last_sync: RwLock<Instant>,
    /// 累计同步次数
    sync_count: AtomicU64,
    /// 累计差异数
    total_diffs: AtomicU64,
}

impl StateSync {
    /// 创建状态同步器
    pub fn new(host: &str) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .tcp_nodelay(true)
                .build()
                .expect("Failed to build HTTP client"),
            host: host.to_string(),
            conn_state: RwLock::new(ConnectionState::Connected),
            last_sync: RwLock::new(Instant::now()),
            sync_count: AtomicU64::new(0),
            total_diffs: AtomicU64::new(0),
        }
    }

    /// 当前连接状态
    pub fn connection_state(&self) -> ConnectionState {
        *self.conn_state.read().unwrap()
    }

    /// 标记断开
    pub fn mark_disconnected(&self) {
        let mut state = self.conn_state.write().unwrap();
        *state = ConnectionState::Disconnected { since: Instant::now() };
        tracing::warn!("state_sync: 连接断开");
    }

    /// 标记重连中
    pub fn mark_reconnecting(&self, attempt: u32) {
        let mut state = self.conn_state.write().unwrap();
        let backoff = (DEFAULT_RECONNECT_BACKOFF_INITIAL.as_secs() * 2u64.pow(attempt.min(5)))
            .min(DEFAULT_RECONNECT_BACKOFF_MAX.as_secs());
        *state = ConnectionState::Reconnecting {
            attempt,
            next_retry: Instant::now() + Duration::from_secs(backoff),
        };
        tracing::info!("state_sync: 重连 attempt={} backoff={}s", attempt, backoff);
    }

    /// 标记已连接（重连成功）
    pub fn mark_connected(&self) {
        *self.conn_state.write().unwrap() = ConnectionState::Connected;
        tracing::info!("state_sync: 连接恢复");
    }

    // ========================================================================
    // 数据拉取
    // ========================================================================

    /// 拉取 CLOB 仓位
    pub async fn fetch_positions(&self) -> Result<Vec<Position>, String> {
        let url = format!("{}/balance-allowance", self.host);
        let resp = self.client.get(&url).send().await.map_err(|e| e.to_string())?;
        let data: Vec<Position> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data)
    }

    /// 拉取 open orders
    pub async fn fetch_open_orders(&self) -> Result<Vec<serde_json::Value>, String> {
        let url = format!("{}/data/orders", self.host);
        let resp = self.client.get(&url).send().await.map_err(|e| e.to_string())?;
        let data: Vec<serde_json::Value> = resp.json().await.map_err(|e| e.to_string())?;
        Ok(data)
    }

    // ========================================================================
    // 对账
    // ========================================================================

    /// 持仓对账：对比本地 vs CLOB
    ///
    /// `local_positions`: {(token_id, notional_usdc)}
    pub fn reconcile_positions(
        &self,
        local_positions: &HashMap<String, f64>,
        remote_positions: &[Position],
    ) -> ReconciliationReport {
        let mut report = ReconciliationReport {
            timestamp: now_secs(),
            in_sync: true,
            ..Default::default()
        };

        for pos in remote_positions {
            report.positions_checked += 1;

            let remote_notional: f64 = pos.size.parse().unwrap_or(0.0);
            let local = local_positions.get(&pos.asset_id).copied().unwrap_or(0.0);

            // 忽略微小差异（四舍五入误差）
            let diff = (local - remote_notional).abs();
            if diff > 1.0 {
                report.in_sync = false;
                let severity = if diff > 100.0 { DiffSeverity::Critical } else { DiffSeverity::Warning };
                report.diffs.push(DiffItem {
                    token_id: pos.asset_id.clone(),
                    field: "position".to_string(),
                    local_value: format!("{:.2}", local),
                    remote_value: format!("{:.2}", remote_notional),
                    severity,
                });
                tracing::warn!(
                    "state_sync: 仓位差异 {} local={:.2} remote={:.2}",
                    pos.asset_id, local, remote_notional
                );
            }
        }

        // 检查本地有但远程没有的仓位
        for (token_id, local_notional) in local_positions {
            if *local_notional > 0.0 && !remote_positions.iter().any(|p| &p.asset_id == token_id) {
                report.in_sync = false;
                report.diffs.push(DiffItem {
                    token_id: token_id.clone(),
                    field: "missing_remote".to_string(),
                    local_value: format!("{:.2}", local_notional),
                    remote_value: "0.00".to_string(),
                    severity: DiffSeverity::Critical,
                });
            }
        }

        self.total_diffs.fetch_add(report.diffs.len() as u64, Ordering::Relaxed);
        report
    }

    /// 订单对账：对比本地 pending orders vs CLOB open orders
    ///
    /// `local_pending_order_ids`: 本地已提交但未确认的订单 ID 集合
    pub fn reconcile_orders(
        &self,
        local_pending_order_ids: &std::collections::HashSet<String>,
        remote_open_orders: &[serde_json::Value],
    ) -> ReconciliationReport {
        let mut report = ReconciliationReport {
            timestamp: now_secs(),
            in_sync: true,
            ..Default::default()
        };

        let remote_ids: std::collections::HashSet<String> = remote_open_orders
            .iter()
            .filter_map(|o| o.get("id").and_then(|id| id.as_str()).map(|s| s.to_string()))
            .collect();

        report.orders_checked = local_pending_order_ids.len().max(remote_ids.len());

        // 本地有但远程没有 → 可能已成交/取消
        for local_id in local_pending_order_ids {
            if !remote_ids.contains(local_id) {
                report.diffs.push(DiffItem {
                    token_id: "".to_string(),
                    field: "order_resolved".to_string(),
                    local_value: local_id.clone(),
                    remote_value: "not_found".to_string(),
                    severity: DiffSeverity::Info, // 这是正常现象
                });
            }
        }

        self.total_diffs.fetch_add(report.diffs.len() as u64, Ordering::Relaxed);
        report
    }

    // ========================================================================
    // 完整对账流程
    // ========================================================================

    /// 执行完整对账（持仓 + 订单）
    pub async fn full_reconcile(
        &self,
        local_positions: &HashMap<String, f64>,
        local_pending_orders: &std::collections::HashSet<String>,
    ) -> Result<(ReconciliationReport, ReconciliationReport), String> {
        let positions = self.fetch_positions().await?;
        let orders = self.fetch_open_orders().await?;

        let pos_report = self.reconcile_positions(local_positions, &positions);
        let order_report = self.reconcile_orders(local_pending_orders, &orders);

        self.sync_count.fetch_add(1, Ordering::Relaxed);
        *self.last_sync.write().unwrap() = Instant::now();

        Ok((pos_report, order_report))
    }

    /// 断线恢复：全量重同步
    ///
    /// 重连后调用，拉取最新状态并重置所有计数器。
    pub async fn on_reconnect(
        &self,
        local_positions: &HashMap<String, f64>,
        local_pending_orders: &std::collections::HashSet<String>,
    ) -> Result<bool, String> {
        tracing::info!("state_sync: 开始断线恢复...");
        let (pos, orders) = self.full_reconcile(local_positions, local_pending_orders).await?;

        let ok = !pos.has_critical_diff() && !orders.has_critical_diff();
        if ok {
            self.mark_connected();
        }
        Ok(ok)
    }

    /// 统计
    pub fn stats(&self) -> SyncStats {
        SyncStats {
            connection: self.connection_state(),
            sync_count: self.sync_count.load(Ordering::Relaxed),
            total_diffs: self.total_diffs.load(Ordering::Relaxed),
            last_sync: *self.last_sync.read().unwrap(),
        }
    }
}

// ============================================================================
// SyncStats
// ============================================================================

#[derive(Debug, Clone)]
pub struct SyncStats {
    pub connection: ConnectionState,
    pub sync_count: u64,
    pub total_diffs: u64,
    pub last_sync: Instant,
}

// ============================================================================
// 断线恢复策略
// ============================================================================

/// 断线恢复策略
pub struct ReconnectPolicy {
    initial_backoff: Duration,
    max_backoff: Duration,
    max_attempts: u32,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_backoff: DEFAULT_RECONNECT_BACKOFF_INITIAL,
            max_backoff: DEFAULT_RECONNECT_BACKOFF_MAX,
            max_attempts: 10,
        }
    }
}

impl ReconnectPolicy {
    /// 计算第 n 次重试的退避时间
    pub fn backoff_for(&self, attempt: u32) -> Option<Duration> {
        if attempt >= self.max_attempts {
            return None; // 放弃
        }
        let d = self.initial_backoff.as_secs_f64() * 2.0_f64.powi(attempt as i32);
        Some(Duration::from_secs_f64(d.min(self.max_backoff.as_secs_f64())))
    }
}

// ============================================================================
// 工具
// ============================================================================

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
    fn test_initial_state_connected() {
        let sync = StateSync::new("https://clob.polymarket.com");
        assert!(matches!(sync.connection_state(), ConnectionState::Connected));
    }

    #[test]
    fn test_mark_disconnected() {
        let sync = StateSync::new("https://example.com");
        sync.mark_disconnected();
        assert!(matches!(sync.connection_state(), ConnectionState::Disconnected { .. }));
    }

    #[test]
    fn test_reconnect_flow() {
        let sync = StateSync::new("https://example.com");
        sync.mark_disconnected();
        sync.mark_reconnecting(1);
        let state = sync.connection_state();
        assert!(matches!(state, ConnectionState::Reconnecting { attempt: 1, .. }));

        sync.mark_connected();
        assert!(matches!(sync.connection_state(), ConnectionState::Connected));
    }

    #[test]
    fn test_reconcile_positions_in_sync() {
        let sync = StateSync::new("https://example.com");
        let local: HashMap<String, f64> = [("token-1".to_string(), 100.0)].into();
        let remote = vec![Position {
            market: "m1".to_string(),
            asset_id: "token-1".to_string(),
            size: "100.0".to_string(),
            cash_pnl: "0".to_string(),
        }];
        let report = sync.reconcile_positions(&local, &remote);
        assert!(report.in_sync);
        assert!(report.diffs.is_empty());
    }

    #[test]
    fn test_reconcile_positions_diff() {
        let sync = StateSync::new("https://example.com");
        let local: HashMap<String, f64> = [("token-1".to_string(), 150.0)].into();
        let remote = vec![Position {
            market: "m1".to_string(),
            asset_id: "token-1".to_string(),
            size: "100.0".to_string(),
            cash_pnl: "0".to_string(),
        }];
        let report = sync.reconcile_positions(&local, &remote);
        assert!(!report.in_sync);
        assert!(!report.diffs.is_empty());
        assert!(matches!(report.diffs[0].severity, DiffSeverity::Warning)); // 50 diff < 100 threshold
    }

    #[test]
    fn test_reconcile_positions_ignore_small_diff() {
        let sync = StateSync::new("https://example.com");
        let local: HashMap<String, f64> = [("token-1".to_string(), 100.5)].into();
        let remote = vec![Position {
            market: "m1".to_string(),
            asset_id: "token-1".to_string(),
            size: "100.0".to_string(),
            cash_pnl: "0".to_string(),
        }];
        let report = sync.reconcile_positions(&local, &remote);
        assert!(report.in_sync); // <1.0 difference is ignored
    }

    #[test]
    fn test_reconcile_orders() {
        let sync = StateSync::new("https://example.com");
        let local: std::collections::HashSet<String> = ["ord-1".to_string()].into();
        let remote: Vec<serde_json::Value> = vec![serde_json::json!({"id": "ord-2"})];
        let report = sync.reconcile_orders(&local, &remote);
        // ord-1 not in remote → resolved
        assert!(!report.diffs.is_empty());
    }

    #[test]
    fn test_reconnect_policy_backoff() {
        let policy = ReconnectPolicy::default();
        let b0 = policy.backoff_for(0).unwrap();
        let b3 = policy.backoff_for(3).unwrap();
        assert!(b3 > b0);
        assert!(b3 <= DEFAULT_RECONNECT_BACKOFF_MAX);
        assert!(policy.backoff_for(10).is_none()); // exhausted
    }

    #[test]
    fn test_sync_stats() {
        let sync = StateSync::new("https://example.com");
        let stats = sync.stats();
        assert_eq!(stats.sync_count, 0);
        assert!(matches!(stats.connection, ConnectionState::Connected));
    }
}
