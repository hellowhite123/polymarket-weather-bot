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

use crate::clob::auth::{create_l2_headers, L2HeaderArgs};
use crate::types::clob_types::ApiKeyCreds;
use ethers::signers::LocalWallet;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};
use tracing;

/// Polymarket Data API 远程仓位响应。
///
/// 与 crate::types::clob_types::Position 分离：
/// - Position：现有 CLOB / 内部兼容模型
/// - RemotePosition：Data API /positions 的远程响应模型
#[derive(Debug, Clone, Deserialize)]
pub struct RemotePosition {
    pub asset: String,

    #[serde(rename = "conditionId")]
    pub condition_id: String,

    pub size: f64,

    #[serde(rename = "avgPrice")]
    pub avg_price: f64,

    #[serde(rename = "currentValue")]
    pub current_value: f64,

    #[serde(rename = "cashPnl")]
    pub cash_pnl: f64,
}

// ============================================================================
// PaginatedResponse — Polymarket CLOB /data/* 端点的统一分页包装
// ============================================================================
// 官方 Rust SDK (docs.rs/polymarket-rs-sdk) 明确列出 `PaginatedResponse` 是
// CLOB /data/orders、/data/trades 等端点的响应包装。典型形态：
//
//   { "limit": 500, "count": 2, "data": [ ... ] }
//
// 旧实现按「顶层数组」解析（TS 文档示例），真实服务返回包装对象，
// 导致 `error decoding response body`。本类型用于先按包装对象解析，
// 失败时再兜底尝试顶层数组（兼容旧/特殊路径）。

/// 泛型分页包装：`data` 字段为具体元素类型
#[derive(Debug, Clone, Deserialize)]
pub struct PaginatedResponse<T> {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub count: Option<u32>,
    pub data: Vec<T>,
}

/// 宽松版分页包装：`data` 字段为任意 JSON，用于「先试包装、失败兜底数组」
#[derive(Debug, Deserialize)]
struct PaginatedResponseRaw {
    #[allow(dead_code)]
    #[serde(default)]
    limit: Option<u32>,
    #[allow(dead_code)]
    #[serde(default)]
    count: Option<u32>,
    data: Vec<serde_json::Value>,
}

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
        self.diffs
            .iter()
            .any(|d| matches!(d.severity, DiffSeverity::Critical))
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
            ConnectionState::Reconnecting { attempt, .. } => {
                write!(f, "reconnecting(#{})", attempt)
            }
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
    /// HTTP客户端
    client: reqwest::Client,

    /// CLOB API host
    clob_host: String,

    /// Data API host
    data_api_host: String,

    /// 钱包地址
    wallet_address: String,

    /// L2 API认证凭证
    api_creds: ApiKeyCreds,

    /// 钱包签名器
    wallet: LocalWallet,

    /// 连接状态
    conn_state: RwLock<ConnectionState>,

    /// 上次成功同步时间
    last_sync: RwLock<Instant>,

    /// 同步次数
    sync_count: AtomicU64,

    /// 差异数量
    total_diffs: AtomicU64,
}

impl StateSync {
    /// 创建状态同步器
    pub fn new(
        clob_host: &str,
        data_api_host: &str,
        wallet_address: &str,
        wallet: LocalWallet,
        api_creds: ApiKeyCreds,
    ) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .tcp_nodelay(true)
                .build()
                .expect("Failed to build HTTP client"),

            clob_host: clob_host.to_string(),

            data_api_host: data_api_host.to_string(),

            wallet_address: wallet_address.to_string(),

            wallet,

            api_creds,

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
        *state = ConnectionState::Disconnected {
            since: Instant::now(),
        };
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

    /// 拉取 Data API 当前仓位。
    ///
    /// `size` 表示 token quantity；远程 DTO 与现有 CLOB `Position` 分离。
    ///
    /// 先尝试 `PaginatedResponse`（包装对象），失败兜底顶层数组，兼容
    /// Data API 与 CLOB `/data/*` 两种响应形态。错误信息保留原始 body
    /// 前 200 字符，便于诊断。
    pub async fn fetch_positions(&self) -> Result<Vec<RemotePosition>, String> {
        let url = format!(
            "{}/positions?user={}",
            self.data_api_host, self.wallet_address
        );

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "positions API request failed: status={}, body={}",
                status, body
            ));
        }

        let text = resp.text().await.map_err(|e| e.to_string())?;

        // 1) 优先：PaginatedResponse 包装对象（真实格式）
        if let Ok(p) = serde_json::from_str::<PaginatedResponse<RemotePosition>>(&text) {
            return Ok(p.data);
        }
        // 2) 兜底：顶层数组（旧/兼容格式）
        serde_json::from_str::<Vec<RemotePosition>>(&text).map_err(|e| {
            let preview = &text[..text.len().min(200)];
            format!(
                "positions body 既不是包装对象也不是数组: {} (body前200字符: {})",
                e, preview
            )
        })
    }

    /// 拉取用户当前未结订单。
    ///
    /// `/data/orders` 需要 L2 认证：携带 POLY_ADDRESS / POLY_SIGNATURE /
    /// POLY_TIMESTAMP / POLY_API_KEY / POLY_PASSPHRASE 头。
    ///
    /// 响应为 `PaginatedResponse` 包装对象（`{"limit":...,"count":...,"data":[...]}`）；
    /// 旧实现按顶层数组解析导致 `error decoding response body`。这里先按包装
    /// 对象解析，失败兜底顶层数组，并保留 body 前 200 字符便于诊断。
    pub async fn fetch_open_orders(&self) -> Result<Vec<serde_json::Value>, String> {
        let url = format!("{}/data/orders", self.clob_host);

        let timestamp = now_secs();
        let l2_args = L2HeaderArgs {
            method: "GET".to_string(),
            request_path: "/data/orders".to_string(),
            body: None,
        };
        let headers = create_l2_headers(&self.wallet, &self.api_creds, &l2_args, timestamp)
            .map_err(|e| e.to_string())?;

        let mut req = self.client.get(&url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "open orders API request failed: status={}, body={}",
                status, body
            ));
        }

        let text = resp.text().await.map_err(|e| e.to_string())?;

        // 1) 优先：PaginatedResponse 包装对象（真实格式）
        if let Ok(p) = serde_json::from_str::<PaginatedResponseRaw>(&text) {
            return Ok(p.data);
        }
        // 2) 兜底：顶层数组（旧/兼容格式）
        serde_json::from_str::<Vec<serde_json::Value>>(&text).map_err(|e| {
            let preview = &text[..text.len().min(200)];
            format!(
                "open orders body 既不是包装对象也不是数组: {} (body前200字符: {})",
                e, preview
            )
        })
    }
    // ========================================================================
    // 对账
    // ========================================================================

    /// 持仓对账：对比本地 quantity vs Data API quantity。
    ///
    /// `local_positions`: {(token_id, quantity)}
    pub fn reconcile_positions(
        &self,
        local_positions: &HashMap<String, f64>,
        remote_positions: &[RemotePosition],
    ) -> ReconciliationReport {
        let mut report = ReconciliationReport {
            timestamp: now_secs(),
            in_sync: true,
            ..Default::default()
        };

        for pos in remote_positions {
            report.positions_checked += 1;

            // Data API `size` 表示 token quantity，不是 USDC notional。
            let remote_quantity = pos.size;
            let local_quantity = local_positions.get(&pos.asset).copied().unwrap_or(0.0);

            let diff = (local_quantity - remote_quantity).abs();

            // 暂时沿用原有 1.0 token 容差；本轮不修改容差模型。
            if diff > 1.0 {
                report.in_sync = false;

                let severity = if diff > 100.0 {
                    DiffSeverity::Critical
                } else {
                    DiffSeverity::Warning
                };

                report.diffs.push(DiffItem {
                    token_id: pos.asset.clone(),
                    field: "position_quantity".to_string(),
                    local_value: format!("{:.6}", local_quantity),
                    remote_value: format!("{:.6}", remote_quantity),
                    severity,
                });

                tracing::warn!(
                    "state_sync: 仓位数量差异 {} local={:.6} remote={:.6}",
                    pos.asset,
                    local_quantity,
                    remote_quantity
                );
            }
        }

        // 检查本地有但远程没有的非零仓位。
        for (token_id, local_quantity) in local_positions {
            if local_quantity.abs() > 0.0 && !remote_positions.iter().any(|p| &p.asset == token_id)
            {
                report.in_sync = false;
                report.diffs.push(DiffItem {
                    token_id: token_id.clone(),
                    field: "missing_remote".to_string(),
                    local_value: format!("{:.6}", local_quantity),
                    remote_value: "0.000000".to_string(),
                    severity: DiffSeverity::Critical,
                });
            }
        }

        self.total_diffs
            .fetch_add(report.diffs.len() as u64, Ordering::Relaxed);
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
            .filter_map(|o| {
                o.get("id")
                    .and_then(|id| id.as_str())
                    .map(|s| s.to_string())
            })
            .collect();

        report.orders_checked = local_pending_order_ids.len().max(remote_ids.len());

        // 本地认为仍处于 open 状态，但远端不存在。
        // 可能已经成交 / 取消 / 过期，但当前对账接口无法确认最终状态。
        // 启动阶段必须 FAIL-CLOSED，不能直接放行交易。
        for local_id in local_pending_order_ids {
            if !remote_ids.contains(local_id) {
                report.in_sync = false;

                report.diffs.push(DiffItem {
                    token_id: String::new(),
                    field: "order_missing_remote".to_string(),
                    local_value: local_id.clone(),
                    remote_value: "not_found".to_string(),
                    severity: DiffSeverity::Critical,
                });

                tracing::warn!(
                    "state_sync: 本地活动订单在远端不存在，暂无法确认最终状态: {}",
                    local_id
                );
            }
        }

        // 远端存在、本地不存在。
        // 说明交易所存在本地未知的活动订单，启动阶段必须 FAIL-CLOSED。
        for remote_id in &remote_ids {
            if !local_pending_order_ids.contains(remote_id) {
                report.in_sync = false;

                report.diffs.push(DiffItem {
                    token_id: String::new(),
                    field: "order_missing_local".to_string(),
                    local_value: "not_found".to_string(),
                    remote_value: remote_id.clone(),
                    severity: DiffSeverity::Critical,
                });

                tracing::error!("state_sync: 发现本地未知的远端 open order: {}", remote_id);
            }
        }

        self.total_diffs
            .fetch_add(report.diffs.len() as u64, Ordering::Relaxed);
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
        let (pos, orders) = self
            .full_reconcile(local_positions, local_pending_orders)
            .await?;

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
        Some(Duration::from_secs_f64(
            d.min(self.max_backoff.as_secs_f64()),
        ))
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

    /// 构造一个仅用于状态机/对账逻辑测试的 StateSync（不联网）。
    fn test_sync() -> StateSync {
        let wallet: LocalWallet =
            "bd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b"
                .parse()
                .unwrap();
        let creds = ApiKeyCreds {
            key: "k".into(),
            secret: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
            passphrase: "p".into(),
        };
        StateSync::new(
            "https://example.com",
            "https://data-api.polymarket.com",
            "0x0000000000000000000000000000000000000000",
            wallet,
            creds,
        )
    }

    #[test]
    fn test_initial_state_connected() {
        let sync = test_sync();
        assert!(matches!(
            sync.connection_state(),
            ConnectionState::Connected
        ));
    }

    #[test]
    fn test_mark_disconnected() {
        let sync = test_sync();
        sync.mark_disconnected();
        assert!(matches!(
            sync.connection_state(),
            ConnectionState::Disconnected { .. }
        ));
    }

    #[test]
    fn test_reconnect_flow() {
        let sync = test_sync();
        sync.mark_disconnected();
        sync.mark_reconnecting(1);
        let state = sync.connection_state();
        assert!(matches!(
            state,
            ConnectionState::Reconnecting { attempt: 1, .. }
        ));

        sync.mark_connected();
        assert!(matches!(
            sync.connection_state(),
            ConnectionState::Connected
        ));
    }

    #[test]
    fn test_reconcile_positions_in_sync() {
        let sync = test_sync();
        let local: HashMap<String, f64> = [("token-1".to_string(), 100.0)].into();
        let remote = vec![RemotePosition {
            asset: "token-1".to_string(),
            condition_id: "m1".to_string(),
            size: 100.0,
            avg_price: 0.5,
            current_value: 50.0,
            cash_pnl: 0.0,
        }];
        let report = sync.reconcile_positions(&local, &remote);
        assert!(report.in_sync);
        assert!(report.diffs.is_empty());
    }

    #[test]
    fn test_reconcile_positions_diff() {
        let sync = test_sync();
        let local: HashMap<String, f64> = [("token-1".to_string(), 150.0)].into();
        let remote = vec![RemotePosition {
            asset: "token-1".to_string(),
            condition_id: "m1".to_string(),
            size: 100.0,
            avg_price: 0.5,
            current_value: 50.0,
            cash_pnl: 0.0,
        }];
        let report = sync.reconcile_positions(&local, &remote);
        assert!(!report.in_sync);
        assert!(!report.diffs.is_empty());
        assert!(matches!(report.diffs[0].severity, DiffSeverity::Warning)); // 50 diff < 100 threshold
    }

    #[test]
    fn test_reconcile_positions_ignore_small_diff() {
        let sync = test_sync();
        let local: HashMap<String, f64> = [("token-1".to_string(), 100.5)].into();
        let remote = vec![RemotePosition {
            asset: "token-1".to_string(),
            condition_id: "m1".to_string(),
            size: 100.0,
            avg_price: 0.5,
            current_value: 50.0,
            cash_pnl: 0.0,
        }];
        let report = sync.reconcile_positions(&local, &remote);
        assert!(report.in_sync); // <1.0 difference is ignored
    }

    #[test]
    fn test_reconcile_orders() {
        let sync = test_sync();
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
        let sync = test_sync();
        let stats = sync.stats();
        assert_eq!(stats.sync_count, 0);
        assert!(matches!(stats.connection, ConnectionState::Connected));
    }

    // ========================================================================
    // PaginatedResponse 双格式解析（2026-08-25 修复的回归测试）
    // ========================================================================

    #[test]
    fn test_paginated_orders_parse_wrapped() {
        // 真实形态：PaginatedResponse 包装对象（官方 Rust SDK 文档）
        let body = r#"{"limit":500,"count":1,"data":[{"id":"o1","market":"0x..","asset_id":"1","owner":"0x..","maker_address":"0x..","side":"BUY","price":"0.5","original_size":"10","size_matched":"0","outcome":"Yes","order_type":"GTC","status":"LIVE","associate_trades":[],"created_at":1748779200,"expiration":"0"}]}"#;
        let p: PaginatedResponseRaw = serde_json::from_str(body).unwrap();
        assert_eq!(p.data.len(), 1);
        assert_eq!(p.data[0]["id"], "o1");
    }

    #[test]
    fn test_paginated_orders_parse_empty_wrapped() {
        // 空单：包装对象 + 空 data
        let body = r#"{"limit":500,"count":0,"data":[]}"#;
        let p: PaginatedResponseRaw = serde_json::from_str(body).unwrap();
        assert!(p.data.is_empty());
    }

    #[test]
    fn test_paginated_orders_parse_bare_array() {
        // 兜底：顶层裸数组（TS 文档示例 / 兼容路径）
        let body = r#"[{"id":"o1","status":"LIVE"},{"id":"o2","status":"LIVE"}]"#;
        let v: Vec<serde_json::Value> = serde_json::from_str(body).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0]["id"], "o1");
    }

    #[test]
    fn test_paginated_positions_parse_wrapped() {
        // /positions 若返回包装对象也能正确解出 data
        let body = r#"{"limit":500,"count":1,"data":[{"asset":"tok-1","conditionId":"c1","size":100.0,"avgPrice":0.5,"currentValue":50.0,"cashPnl":0.0}]}"#;
        let p: PaginatedResponse<RemotePosition> = serde_json::from_str(body).unwrap();
        assert_eq!(p.data.len(), 1);
        assert_eq!(p.data[0].asset, "tok-1");
        assert_eq!(p.data[0].size, 100.0);
    }

    #[test]
    fn test_paginated_positions_parse_bare_array() {
        // /positions 若返回裸数组也能正确解析
        let body = r#"[{"asset":"tok-1","conditionId":"c1","size":100.0,"avgPrice":0.5,"currentValue":50.0,"cashPnl":0.0}]"#;
        let v: Vec<RemotePosition> = serde_json::from_str(body).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].asset, "tok-1");
    }

    #[test]
    fn test_paginated_invalid_body_error_preview() {
        // 既不是包装对象（缺 data 字段）也不是数组 → 两种解析都必须失败
        let body = r#"{"error":"some html or unexpected shape"}"#;
        assert!(
            serde_json::from_str::<PaginatedResponseRaw>(body).is_err(),
            "缺 data 字段的 body 不应被当作包装对象解析成功"
        );
        assert!(
            serde_json::from_str::<Vec<serde_json::Value>>(body).is_err(),
            "对象 body 不应被当作数组解析成功"
        );
    }
}
