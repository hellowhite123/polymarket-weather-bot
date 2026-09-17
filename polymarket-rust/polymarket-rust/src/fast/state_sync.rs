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

/// CLOB `/data/trades` 单笔成交响应（P0-007B 恢复分类用）。
///
/// 字段采用 camelCase 为主、snake_case 兜底（CLOB 与 Data API 两套命名
/// 不一致），`price`/`size` 兼容字符串或数字。
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteTrade {
    #[serde(default)]
    pub id: String,

    #[serde(default, rename = "takerOrderId", alias = "taker_order_id")]
    pub taker_order_id: String,

    #[serde(default, rename = "makerOrderId", alias = "maker_order_id")]
    pub maker_order_id: String,

    #[serde(default)]
    pub side: String,

    #[serde(default)]
    pub price: serde_json::Value,

    #[serde(default)]
    pub size: serde_json::Value,

    #[serde(default, rename = "assetId", alias = "asset_id")]
    pub asset_id: String,
}

impl RemoteTrade {
    /// 成交价（字符串或数字 → f64）。
    pub fn price(&self) -> f64 {
        parse_json_f64(&self.price)
    }

    /// 成交量（字符串或数字 → f64）。
    pub fn size(&self) -> f64 {
        parse_json_f64(&self.size)
    }

    /// 该成交是否引用了指定订单（作为 taker 或 maker）。
    ///
    /// 用于判断一个从 open orders 消失的订单是否已经真实成交。
    pub fn references_order(&self, order_id: &str) -> bool {
        !order_id.is_empty()
            && (self.taker_order_id == order_id || self.maker_order_id == order_id)
    }
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

/// 仓位对账容差（token quantity）。与 `reconcile_positions` 现有容差保持一致。
pub const DEFAULT_POSITION_TOLERANCE: f64 = 1.0;

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
// 恢复 (Recovery) — P0-007B
// ============================================================================
// full_reconcile 原实现只「检测」不「修复」：消失的订单无法确认终态、
// 仓位差异只报告不重建。以下类型把「检测到的差异」转成「可执行的恢复动作」，
// 由调用方（ExecutionService::apply_recovery）落到本地状态。

/// 消失订单的终态分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderTerminalState {
    /// 已真实成交（有 trade 引用该订单）
    Filled,
    /// 已消失但无成交记录（取消 / 过期 / 其他非成交终态）
    NonFilled,
}

/// 单条恢复动作。
#[derive(Debug, Clone)]
pub enum RecoveryAction {
    /// 订单已成交：补记 fill（FillLedger + RiskEngine + OrderStore）
    MarkOrderFilled {
        order_id: String,
        token_id: String,
        side: String,
        price: f64,
        size: f64,
        trade_id: String,
    },
    /// 订单已消失但非成交：标记终态，清除 pending
    MarkOrderNonFilled { order_id: String },
    /// 持仓重建：以远端为权威覆盖本地仓位（qty=0 且 notional=0 表示删除）
    RebuildPosition {
        token_id: String,
        qty: f64,
        notional: f64,
    },
}

/// 一次对账产出的恢复计划。
#[derive(Debug, Clone, Default)]
pub struct RecoveryPlan {
    /// 待执行的恢复动作
    pub actions: Vec<RecoveryAction>,
    /// 无法安全分类的消失订单（trades 拉取失败等），必须 FAIL-CLOSED 保留 pending
    pub unresolved_orders: Vec<String>,
}

impl RecoveryPlan {
    pub fn has_actions(&self) -> bool {
        !self.actions.is_empty()
    }

    /// 是否存在无法分类的消失订单（true = 仍需人工介入）。
    pub fn has_unresolved(&self) -> bool {
        !self.unresolved_orders.is_empty()
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

    /// 拉取当前用户的成交记录（P0-007B 恢复分类用）。
    ///
    /// `/data/trades?user=<address>` 需要 L2 认证，响应为 `PaginatedResponse`
    /// 包装对象或顶层数组，与 `fetch_open_orders` 一致地双格式兜底。
    pub async fn fetch_trades(&self) -> Result<Vec<RemoteTrade>, String> {
        let query = format!("user={}", self.wallet_address);
        let url = format!("{}/data/trades?{}", self.clob_host, query);
        let request_path = format!("/data/trades?{}", query);

        let timestamp = now_secs();
        let l2_args = L2HeaderArgs {
            method: "GET".to_string(),
            request_path,
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
                "trades API request failed: status={}, body={}",
                status, body
            ));
        }

        let text = resp.text().await.map_err(|e| e.to_string())?;

        // 1) 优先：PaginatedResponse 包装对象（真实格式）
        if let Ok(p) = serde_json::from_str::<PaginatedResponse<RemoteTrade>>(&text) {
            return Ok(p.data);
        }
        // 2) 兜底：顶层数组（旧/兼容格式）
        serde_json::from_str::<Vec<RemoteTrade>>(&text).map_err(|e| {
            let preview = &text[..text.len().min(200)];
            format!(
                "trades body 既不是包装对象也不是数组: {} (body前200字符: {})",
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
    // 恢复计划构建（P0-007B）
    // ========================================================================

    /// 执行完整对账 + 自动恢复（P0-007B）。
    ///
    /// 在 `full_reconcile` 的检测基础上，额外拉取成交记录，为「从 open orders
    /// 消失」的订单分类终态（Filled / NonFilled），并为仓位差异生成重建动作。
    ///
    /// - trades 拉取失败不阻断对账，但消失订单会进入 `RecoveryPlan.unresolved_orders`
    ///   （FAIL-CLOSED 保留 pending，不擅自标记终态）。
    /// - 无消失订单时跳过 trades 拉取，避免无谓的网络往返。
    pub async fn full_reconcile_with_recovery(
        &self,
        local_positions: &HashMap<String, f64>,
        local_pending_orders: &std::collections::HashSet<String>,
    ) -> Result<
        (
            ReconciliationReport,
            ReconciliationReport,
            RecoveryPlan,
        ),
        String,
    > {
        let positions = self.fetch_positions().await?;
        let orders = self.fetch_open_orders().await?;

        let remote_open_ids: std::collections::HashSet<String> = orders
            .iter()
            .filter_map(|o| o.get("id").and_then(|id| id.as_str()).map(|s| s.to_string()))
            .collect();

        // 仅当存在「本地 pending 但远端已消失」的订单时才需要拉取 trades 分类终态
        let has_vanished = local_pending_orders
            .iter()
            .any(|id| !remote_open_ids.contains(id));

        let trades: Option<Vec<RemoteTrade>> = if has_vanished {
            match self.fetch_trades().await {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!(
                        "state_sync: 成交记录拉取失败，消失订单将无法分类（fail-closed）: {e}"
                    );
                    None
                }
            }
        } else {
            Some(Vec::new())
        };

        let pos_report = self.reconcile_positions(local_positions, &positions);
        let order_report = self.reconcile_orders(local_pending_orders, &orders);
        let recovery = build_recovery_plan(
            local_positions,
            &positions,
            local_pending_orders,
            &remote_open_ids,
            trades.as_deref(),
            DEFAULT_POSITION_TOLERANCE,
        );

        self.sync_count.fetch_add(1, Ordering::Relaxed);
        *self.last_sync.write().unwrap() = Instant::now();

        Ok((pos_report, order_report, recovery))
    }

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

/// 字符串或数字 JSON 值 → f64（兼容 Polymarket 两种价量编码）。
fn parse_json_f64(v: &serde_json::Value) -> f64 {
    match v {
        serde_json::Value::String(s) => s.parse().unwrap_or(0.0),
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    }
}

// ============================================================================
// 恢复计划构建（P0-007B）— 纯函数
// ============================================================================

/// 在成交列表里查找引用指定订单的成交（用于判断消失订单是否已成交）。
pub fn find_trade_for_order<'a>(order_id: &str, trades: &'a [RemoteTrade]) -> Option<&'a RemoteTrade> {
    trades.iter().find(|t| t.references_order(order_id))
}

/// 在成交列表里查找引用指定订单的**所有**成交（P1-030）。
///
/// 一个订单可能被多笔 partial fill 成交（多笔 trade），旧实现 `.find()`
/// 只取第一笔，导致恢复时只补记第一笔成交量、丢失后续 fill。
/// 返回全部引用该订单的 trade（按列表原始顺序）。
pub fn find_trades_for_order<'a>(
    order_id: &str,
    trades: &'a [RemoteTrade],
) -> Vec<&'a RemoteTrade> {
    trades
        .iter()
        .filter(|t| t.references_order(order_id))
        .collect()
}

/// 分类消失订单：有 trade 引用 → Filled；否则 → NonFilled。
pub fn classify_vanished_order(order_id: &str, trades: &[RemoteTrade]) -> OrderTerminalState {
    if find_trade_for_order(order_id, trades).is_some() {
        OrderTerminalState::Filled
    } else {
        OrderTerminalState::NonFilled
    }
}

/// P0-EXEC-005：恢复成交方向归一化到本方视角。
///
/// `RemoteTrade.side` 是顶层（taker）方向。若该成交通过 `taker_order_id` 引用
/// 我们的订单 → 我们是 taker，方向不变；若通过 `maker_order_id` 引用 → 我们是
/// maker，方向取反。其他情况保持原值（正常不会发生）。
fn own_recovery_side(order_id: &str, trade: &RemoteTrade) -> String {
    if trade.taker_order_id == order_id {
        trade.side.clone()
    } else if trade.maker_order_id == order_id {
        match trade.side.to_ascii_uppercase().as_str() {
            "BUY" => "SELL".to_string(),
            "SELL" => "BUY".to_string(),
            _ => trade.side.clone(),
        }
    } else {
        trade.side.clone()
    }
}

/// 由对账输入构建恢复计划。
///
/// - 订单恢复：本地 pending 但远端已消失的订单，若有 trade 引用 → `MarkOrderFilled`
///   （携带完整成交信息）；无 trade → `MarkOrderNonFilled`。
/// - trades 为 `None`（拉取失败）时，消失订单进入 `unresolved_orders`（FAIL-CLOSED）。
/// - 持仓恢复：以远端为权威，本地与远端数量差超过 `position_tolerance` → `RebuildPosition`；
///   本地有但远端没有的非零仓位 → `RebuildPosition`（qty=0 删除）。
pub fn build_recovery_plan(
    local_positions: &HashMap<String, f64>,
    remote_positions: &[RemotePosition],
    local_pending_order_ids: &std::collections::HashSet<String>,
    remote_open_ids: &std::collections::HashSet<String>,
    trades: Option<&[RemoteTrade]>,
    position_tolerance: f64,
) -> RecoveryPlan {
    let mut plan = RecoveryPlan::default();

    // 1) 订单恢复：本地 pending 但远端已消失 → 分类终态
    for order_id in local_pending_order_ids {
        if remote_open_ids.contains(order_id) {
            continue; // 仍在 open，无需恢复
        }
        match trades {
            Some(ts) => {
                // P1-030：取该订单的**全部** trades（多笔 partial fill），
                // 每笔生成一个 MarkOrderFilled 动作——恢复时由 `on_market_trade`
                // 的 `apply_fill` 增量累加 + `claim_trade` 按 trade_id 幂等去重，
                // 而非旧实现只取第一笔、丢失后续成交量。
                let matched = find_trades_for_order(order_id, ts);
                if matched.is_empty() {
                    plan.actions
                        .push(RecoveryAction::MarkOrderNonFilled { order_id: order_id.clone() });
                } else {
                    for t in matched {
                        if t.id.trim().is_empty() {
                            tracing::warn!(
                                "state_sync: 丢弃缺少 trade_id 的恢复成交 order_id={}",
                                order_id
                            );
                            continue;
                        }

                        plan.actions.push(RecoveryAction::MarkOrderFilled {
                            order_id: order_id.clone(),
                            token_id: t.asset_id.clone(),
                            // P0-EXEC-005：按本方角色归一化方向（maker 取反）
                            side: own_recovery_side(order_id, t),
                            price: t.price(),
                            size: t.size(),
                            trade_id: t.id.clone(),
                        });
                    }
                }
            }
            None => plan.unresolved_orders.push(order_id.clone()),
        }
    }

    // 2) 持仓恢复：以远端为权威重建
    for pos in remote_positions {
        let local = local_positions.get(&pos.asset).copied().unwrap_or(0.0);
        if (local - pos.size).abs() > position_tolerance {
            let notional = pos.size * pos.avg_price;
            plan.actions.push(RecoveryAction::RebuildPosition {
                token_id: pos.asset.clone(),
                qty: pos.size,
                notional,
            });
        }
    }

    // 3) 本地有但远端没有的非零仓位 → 清空
    for (token_id, local_qty) in local_positions {
        if local_qty.abs() > 0.0 && !remote_positions.iter().any(|p| &p.asset == token_id) {
            plan.actions.push(RecoveryAction::RebuildPosition {
                token_id: token_id.clone(),
                qty: 0.0,
                notional: 0.0,
            });
        }
    }

    plan
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

    // ========================================================================
    // P0-007B 恢复计划构建测试
    // ========================================================================

    fn remote_pos(asset: &str, size: f64, avg_price: f64) -> RemotePosition {
        RemotePosition {
            asset: asset.to_string(),
            condition_id: "c1".to_string(),
            size,
            avg_price,
            current_value: 0.0,
            cash_pnl: 0.0,
        }
    }

    fn remote_trade(id: &str, taker: &str, maker: &str, asset: &str, side: &str) -> RemoteTrade {
        RemoteTrade {
            id: id.to_string(),
            taker_order_id: taker.to_string(),
            maker_order_id: maker.to_string(),
            side: side.to_string(),
            price: serde_json::json!("0.5"),
            size: serde_json::json!("10"),
            asset_id: asset.to_string(),
        }
    }

    #[test]
    fn test_remote_trade_price_size_parse_string() {
        let t = remote_trade("t1", "o1", "", "tok-1", "BUY");
        assert!((t.price() - 0.5).abs() < 1e-9);
        assert!((t.size() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn test_remote_trade_price_size_parse_number() {
        let mut t = remote_trade("t1", "o1", "", "tok-1", "BUY");
        t.price = serde_json::json!(0.62);
        t.size = serde_json::json!(8);
        assert!((t.price() - 0.62).abs() < 1e-9);
        assert!((t.size() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn test_remote_trade_references_order() {
        let t = remote_trade("t1", "o-taker", "o-maker", "tok-1", "BUY");
        assert!(t.references_order("o-taker"));
        assert!(t.references_order("o-maker"));
        assert!(!t.references_order("o-other"));
        assert!(!t.references_order(""));
    }

    #[test]
    fn test_classify_vanished_order_filled_vs_nonfilled() {
        let trades = vec![remote_trade("t1", "o1", "", "tok-1", "BUY")];
        assert_eq!(
            classify_vanished_order("o1", &trades),
            OrderTerminalState::Filled
        );
        assert_eq!(
            classify_vanished_order("o2", &trades),
            OrderTerminalState::NonFilled
        );
    }

    #[test]
    fn test_build_recovery_plan_filled_order() {
        let local_pos: HashMap<String, f64> = HashMap::new();
        let remote_positions: Vec<RemotePosition> = Vec::new();
        let local_pending: std::collections::HashSet<String> = ["o1".to_string()].into();
        let remote_open: std::collections::HashSet<String> = std::collections::HashSet::new();
        let trades = vec![remote_trade("t1", "o1", "", "tok-1", "BUY")];

        let plan = build_recovery_plan(
            &local_pos,
            &remote_positions,
            &local_pending,
            &remote_open,
            Some(&trades),
            1.0,
        );

        assert_eq!(plan.actions.len(), 1);
        assert!(plan.unresolved_orders.is_empty());
        match &plan.actions[0] {
            RecoveryAction::MarkOrderFilled {
                order_id,
                token_id,
                side,
                price,
                size,
                trade_id,
            } => {
                assert_eq!(order_id, "o1");
                assert_eq!(token_id, "tok-1");
                assert_eq!(side, "BUY");
                assert!((price - 0.5).abs() < 1e-9);
                assert!((size - 10.0).abs() < 1e-9);
                assert_eq!(trade_id, "t1");
            }
            other => panic!("期望 MarkOrderFilled，得到 {:?}", other),
        }
    }

    /// P1-035：远端 trade 缺少 trade_id 时，恢复计划不得生成 MarkOrderFilled
    ///（fail-closed：既不补记成交，也不误标 NonFilled，订单保持未决）。
    #[test]
    fn test_build_recovery_plan_skips_empty_trade_id() {
        let local_pos: HashMap<String, f64> = HashMap::new();
        let remote_positions: Vec<RemotePosition> = Vec::new();
        let local_pending: std::collections::HashSet<String> = ["o1".to_string()].into();
        let remote_open: std::collections::HashSet<String> = std::collections::HashSet::new();
        let trades = vec![
            remote_trade("", "o1", "", "tok-1", "BUY"),
            remote_trade("   ", "o1", "", "tok-1", "BUY"),
        ];

        let plan = build_recovery_plan(
            &local_pos,
            &remote_positions,
            &local_pending,
            &remote_open,
            Some(&trades),
            1.0,
        );

        assert!(
            plan.actions.is_empty(),
            "空/空白 trade_id 不应生成任何恢复动作，实际 {:?}",
            plan.actions
        );
        assert!(plan.unresolved_orders.is_empty());
    }

    /// P0-EXEC-005：恢复路径中本方为 maker（订单通过 maker_order_id 引用）时方向取反。
    #[test]
    fn test_build_recovery_plan_maker_side_inverted() {
        let local_pos: HashMap<String, f64> = HashMap::new();
        let remote_positions: Vec<RemotePosition> = Vec::new();
        let local_pending: std::collections::HashSet<String> = ["o-mine".to_string()].into();
        let remote_open: std::collections::HashSet<String> = std::collections::HashSet::new();
        // taker side=SELL，我方是 maker（maker_order_id=o-mine）→ 本方应为 BUY
        let trades = vec![remote_trade("t1", "o-taker", "o-mine", "tok-1", "SELL")];

        let plan = build_recovery_plan(
            &local_pos,
            &remote_positions,
            &local_pending,
            &remote_open,
            Some(&trades),
            1.0,
        );

        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            RecoveryAction::MarkOrderFilled { side, .. } => {
                assert_eq!(side, "BUY", "maker 成交方向应取反")
            }
            other => panic!("期望 MarkOrderFilled，得到 {:?}", other),
        }
    }

    /// P0-EXEC-005：恢复路径中本方为 taker（订单通过 taker_order_id 引用）时方向不变。
    #[test]
    fn test_build_recovery_plan_taker_side_unchanged() {
        let local_pos: HashMap<String, f64> = HashMap::new();
        let remote_positions: Vec<RemotePosition> = Vec::new();
        let local_pending: std::collections::HashSet<String> = ["o-mine".to_string()].into();
        let remote_open: std::collections::HashSet<String> = std::collections::HashSet::new();
        let trades = vec![remote_trade("t1", "o-mine", "o-taker", "tok-1", "SELL")];

        let plan = build_recovery_plan(
            &local_pos,
            &remote_positions,
            &local_pending,
            &remote_open,
            Some(&trades),
            1.0,
        );

        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            RecoveryAction::MarkOrderFilled { side, .. } => {
                assert_eq!(side, "SELL", "taker 成交方向应保持不变")
            }
            other => panic!("期望 MarkOrderFilled，得到 {:?}", other),
        }
    }

    /// P1-030：find_trades_for_order 返回引用该订单的全部 trades（多笔 partial fill）。
    #[test]
    fn test_find_trades_for_order_returns_all() {
        let trades = vec![
            remote_trade("t1", "o1", "", "tok-1", "BUY"),
            remote_trade("t2", "o1", "", "tok-1", "BUY"),
            remote_trade("t3", "o2", "", "tok-1", "BUY"),
        ];
        let matched = find_trades_for_order("o1", &trades);
        assert_eq!(matched.len(), 2, "应返回引用 o1 的全部 2 笔 trade");
        assert_eq!(matched[0].id, "t1");
        assert_eq!(matched[1].id, "t2");
        // 无引用 → 空
        assert!(find_trades_for_order("o-none", &trades).is_empty());
    }

    /// P1-030：同订单多笔 partial fill → 每笔生成一个 MarkOrderFilled 动作，不丢失后续成交量。
    #[test]
    fn test_build_recovery_plan_multi_fill_generates_all_actions() {
        let local_pos: HashMap<String, f64> = HashMap::new();
        let remote_positions: Vec<RemotePosition> = Vec::new();
        let local_pending: std::collections::HashSet<String> = ["o1".to_string()].into();
        let remote_open: std::collections::HashSet<String> = std::collections::HashSet::new();
        // 同一订单 3 笔 partial fill（t1/t2/t3）
        let trades = vec![
            remote_trade("t1", "o1", "", "tok-1", "BUY"),
            remote_trade("t2", "o1", "", "tok-1", "BUY"),
            remote_trade("t3", "o1", "", "tok-1", "BUY"),
        ];

        let plan = build_recovery_plan(
            &local_pos,
            &remote_positions,
            &local_pending,
            &remote_open,
            Some(&trades),
            1.0,
        );

        // 3 笔 fill → 3 个 MarkOrderFilled 动作（旧实现只取第一笔 → 1 个）
        assert_eq!(plan.actions.len(), 3, "3 笔 partial fill 应生成 3 个恢复动作");
        let trade_ids: Vec<String> = plan
            .actions
            .iter()
            .filter_map(|a| match a {
                RecoveryAction::MarkOrderFilled { trade_id, .. } => Some(trade_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(trade_ids, vec!["t1", "t2", "t3"], "应包含全部 3 笔 trade_id");
        // 每笔 size 均为单笔成交量 10（累计由 apply_fill 增量完成）
        for a in &plan.actions {
            match a {
                RecoveryAction::MarkOrderFilled { size, .. } => {
                    assert!((size - 10.0).abs() < 1e-9);
                }
                _ => panic!("应全为 MarkOrderFilled"),
            }
        }
    }

    #[test]
    fn test_build_recovery_plan_nonfilled_order() {
        let local_pos: HashMap<String, f64> = HashMap::new();
        let remote_positions: Vec<RemotePosition> = Vec::new();
        let local_pending: std::collections::HashSet<String> = ["o2".to_string()].into();
        let remote_open: std::collections::HashSet<String> = std::collections::HashSet::new();
        let trades = vec![remote_trade("t1", "o1", "", "tok-1", "BUY")]; // o2 无成交

        let plan = build_recovery_plan(
            &local_pos,
            &remote_positions,
            &local_pending,
            &remote_open,
            Some(&trades),
            1.0,
        );

        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            RecoveryAction::MarkOrderNonFilled { order_id } => assert_eq!(order_id, "o2"),
            other => panic!("期望 MarkOrderNonFilled，得到 {:?}", other),
        }
    }

    #[test]
    fn test_build_recovery_plan_unresolved_when_trades_none() {
        let local_pos: HashMap<String, f64> = HashMap::new();
        let remote_positions: Vec<RemotePosition> = Vec::new();
        let local_pending: std::collections::HashSet<String> = ["o1".to_string()].into();
        let remote_open: std::collections::HashSet<String> = std::collections::HashSet::new();

        // trades 拉取失败（None）→ 消失订单进入 unresolved，不擅自标记终态
        let plan = build_recovery_plan(
            &local_pos,
            &remote_positions,
            &local_pending,
            &remote_open,
            None,
            1.0,
        );

        assert!(plan.actions.is_empty());
        assert!(plan.has_unresolved());
        assert_eq!(plan.unresolved_orders, vec!["o1".to_string()]);
    }

    #[test]
    fn test_build_recovery_plan_position_rebuild() {
        // 本地 150 vs 远端 100 → 超容差，重建
        let local_pos: HashMap<String, f64> = [("tok-1".to_string(), 150.0)].into();
        let remote_positions = vec![remote_pos("tok-1", 100.0, 0.5)];
        let local_pending: std::collections::HashSet<String> = std::collections::HashSet::new();
        let remote_open: std::collections::HashSet<String> = std::collections::HashSet::new();

        let plan = build_recovery_plan(
            &local_pos,
            &remote_positions,
            &local_pending,
            &remote_open,
            Some(&[]),
            1.0,
        );

        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            RecoveryAction::RebuildPosition {
                token_id,
                qty,
                notional,
            } => {
                assert_eq!(token_id, "tok-1");
                assert!((qty - 100.0).abs() < 1e-9);
                assert!((notional - 50.0).abs() < 1e-9); // 100 * 0.5
            }
            other => panic!("期望 RebuildPosition，得到 {:?}", other),
        }
    }

    #[test]
    fn test_build_recovery_plan_remove_local_only_position() {
        // 本地有非零仓位，远端没有 → 清空
        let local_pos: HashMap<String, f64> = [("tok-gone".to_string(), 50.0)].into();
        let remote_positions: Vec<RemotePosition> = Vec::new();
        let local_pending: std::collections::HashSet<String> = std::collections::HashSet::new();
        let remote_open: std::collections::HashSet<String> = std::collections::HashSet::new();

        let plan = build_recovery_plan(
            &local_pos,
            &remote_positions,
            &local_pending,
            &remote_open,
            Some(&[]),
            1.0,
        );

        assert_eq!(plan.actions.len(), 1);
        match &plan.actions[0] {
            RecoveryAction::RebuildPosition {
                token_id,
                qty,
                notional,
            } => {
                assert_eq!(token_id, "tok-gone");
                assert_eq!(*qty, 0.0);
                assert_eq!(*notional, 0.0);
            }
            other => panic!("期望 RebuildPosition(清空)，得到 {:?}", other),
        }
    }

    #[test]
    fn test_build_recovery_plan_no_diff_no_actions() {
        // 完全一致 → 无动作
        let local_pos: HashMap<String, f64> = [("tok-1".to_string(), 100.0)].into();
        let remote_positions = vec![remote_pos("tok-1", 100.0, 0.5)];
        let local_pending: std::collections::HashSet<String> = ["o1".to_string()].into();
        let remote_open: std::collections::HashSet<String> = ["o1".to_string()].into();

        let plan = build_recovery_plan(
            &local_pos,
            &remote_positions,
            &local_pending,
            &remote_open,
            Some(&[]),
            1.0,
        );

        assert!(!plan.has_actions());
        assert!(!plan.has_unresolved());
    }

    #[test]
    fn test_remote_trade_parse_camelcase_and_snakecase() {
        // camelCase（CLOB 真实形态）
        let camel = r#"{"id":"t1","takerOrderId":"o1","makerOrderId":"","side":"BUY","price":"0.5","size":"10","assetId":"tok-1"}"#;
        let t: RemoteTrade = serde_json::from_str(camel).unwrap();
        assert_eq!(t.taker_order_id, "o1");
        assert_eq!(t.asset_id, "tok-1");

        // snake_case（Data API 兼容形态）
        let snake = r#"{"id":"t1","taker_order_id":"o1","maker_order_id":"","side":"BUY","price":"0.5","size":"10","asset_id":"tok-1"}"#;
        let t: RemoteTrade = serde_json::from_str(snake).unwrap();
        assert_eq!(t.taker_order_id, "o1");
        assert_eq!(t.asset_id, "tok-1");
    }
}
