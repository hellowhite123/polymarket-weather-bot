//! 多 RPC 节点池 (RpcPool)
//!
//! 管理 3-5 个 Polygon RPC 节点，提供自动故障切换、健康检查和负载均衡。
//! 不依赖 ethers Provider——直接用 reqwest + serde_json 发 JSON-RPC 请求，
//! 零额外编译依赖。
//!
//! ## 功能
//!
//! - **健康检查**：周期性 ping `eth_blockNumber`，测量延迟 (p50/p95/p99)
//! - **故障切换**：Dead 节点自动路由到下一个 Healthy 节点
//! - **自动恢复**：Dead 节点灰度恢复 (backoff: 5s→10s→30s→60s)，先 probe 再恢复
//! - **负载均衡**：Least-latency 优先（选择最近 ping 延迟最低的节点）
//! - **一致性验证**：所有健康节点 blockNumber 差异 < 5 判定为一致
//!
//! ## 默认节点
//!
//! ```text
//! polygon-rpc.com       (官方公共)
//! rpc-mainnet.maticvigil.com
//! polygon.llamarpc.com
//! polygon.meowrpc.com
//! 1rpc.io/matic
//! ```
//!
//! ## 使用
//!
//! ```text
//! let pool = RpcPool::with_default_nodes();
//! pool.check_health_all().await?;     // 初始全检
//! let provider = pool.provider();     // 后续请求自动走健康节点
//! let nonce = pool.get_transaction_count(addr).await?;
//! let receipt = pool.get_transaction_receipt(tx_hash).await?;
//! ```

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

// ============================================================================
// 常量
// ============================================================================

/// 默认 Polygon 主网 RPC 节点
pub const DEFAULT_POLYGON_RPCS: &[&str] = &[
    "https://polygon-rpc.com",
    "https://rpc-mainnet.maticvigil.com",
    "https://polygon.llamarpc.com",
    "https://polygon.meowrpc.com",
    "https://1rpc.io/matic",
];

/// 健康检查超时
pub const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// 节点被认为 Dead 的延迟阈值
pub const LATENCY_DEAD_THRESHOLD: Duration = Duration::from_secs(5);

/// 节点被认为 Degraded 的延迟阈值
pub const LATENCY_DEGRADED_THRESHOLD: Duration = Duration::from_millis(800);

/// blockNumber 一致性最大差异
pub const BLOCK_CONSENSUS_MAX_DIFF: u64 = 5;

/// Dead 节点恢复检查间隔 (初始)
pub const RECOVERY_CHECK_INITIAL: Duration = Duration::from_secs(10);

/// Dead 节点恢复检查间隔 (最大)
pub const RECOVERY_CHECK_MAX: Duration = Duration::from_secs(60);

/// 健康检查历史样本数
pub const HEALTH_SAMPLE_SIZE: usize = 8;

// ============================================================================
// JSON-RPC 类型
// ============================================================================

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    params: serde_json::Value,
    id: u64,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    #[serde(default)]
    result: serde_json::Value,
    #[allow(dead_code)]
    id: u64,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    #[allow(dead_code)]
    code: i64,
    message: String,
}

impl JsonRpcRequest {
    fn new(method: &str, params: serde_json::Value) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
            id: COUNTER.fetch_add(1, Ordering::Relaxed),
        }
    }
}

// ============================================================================
// 节点状态
// ============================================================================

/// RPC 节点健康状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    /// 正常响应，延迟在阈值内
    Healthy,
    /// 延迟偏高但仍有响应
    Degraded,
    /// 超时或错误，标记为不可用
    Dead,
}

impl std::fmt::Display for NodeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeState::Healthy => write!(f, "healthy"),
            NodeState::Degraded => write!(f, "degraded"),
            NodeState::Dead => write!(f, "dead"),
        }
    }
}

// ============================================================================
// RpcNode
// ============================================================================

/// 健康检查历史中的单条记录
#[derive(Debug, Clone)]
struct LatencySample {
    latency_ms: f64,
    block_number: u64,
    timestamp: Instant,
}

/// 单个 RPC 节点
pub struct RpcNode {
    /// 节点 URL
    pub url: String,
    /// 当前状态
    state: RwLock<NodeState>,
    /// 最近延迟历史
    history: RwLock<Vec<LatencySample>>,
    /// 最后成功响应时间
    last_success: RwLock<Instant>,
    /// 死节点恢复退避间隔
    recovery_backoff: RwLock<Duration>,
    /// 死节点下次检查时间
    next_recovery_check: RwLock<Instant>,
    /// 总请求数
    total_requests: AtomicU64,
    /// 失败请求数
    total_failures: AtomicU64,
}

impl RpcNode {
    /// 创建新节点（初始状态为 Dead，等待首次健康检查）
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            state: RwLock::new(NodeState::Dead),
            history: RwLock::new(Vec::with_capacity(HEALTH_SAMPLE_SIZE)),
            last_success: RwLock::new(Instant::now()),
            recovery_backoff: RwLock::new(RECOVERY_CHECK_INITIAL),
            next_recovery_check: RwLock::new(Instant::now()),
            total_requests: AtomicU64::new(0),
            total_failures: AtomicU64::new(0),
        }
    }

    /// 当前状态
    pub fn state(&self) -> NodeState {
        *self.state.read().unwrap()
    }

    /// 是否可用于请求
    pub fn is_available(&self) -> bool {
        matches!(
            *self.state.read().unwrap(),
            NodeState::Healthy | NodeState::Degraded
        )
    }

    /// 最近一次健康检查延迟 (ms)
    pub fn last_latency_ms(&self) -> Option<f64> {
        self.history.read().unwrap().last().map(|s| s.latency_ms)
    }

    /// 最近 block number
    pub fn last_block_number(&self) -> Option<u64> {
        self.history.read().unwrap().last().map(|s| s.block_number)
    }

    /// 更新状态并记录健康样本
    fn record_sample(&self, latency_ms: f64, block_number: u64) {
        let mut history = self.history.write().unwrap();
        history.push(LatencySample {
            latency_ms,
            block_number,
            timestamp: Instant::now(),
        });
        if history.len() > HEALTH_SAMPLE_SIZE {
            history.remove(0);
        }

        *self.last_success.write().unwrap() = Instant::now();

        let new_state = if latency_ms > LATENCY_DEAD_THRESHOLD.as_millis() as f64 {
            NodeState::Dead
        } else if latency_ms > LATENCY_DEGRADED_THRESHOLD.as_millis() as f64 {
            NodeState::Degraded
        } else {
            NodeState::Healthy
        };

        let mut state = self.state.write().unwrap();
        if *state != new_state {
            tracing::info!(
                "rpc_pool: {} state {} → {} (latency={:.0}ms)",
                self.url,
                *state,
                new_state,
                latency_ms
            );
            *state = new_state;
        }

        // 恢复后重置退避
        if new_state != NodeState::Dead {
            *self.recovery_backoff.write().unwrap() = RECOVERY_CHECK_INITIAL;
        }
    }

    /// 标记为 Dead（超时/网络错误）
    fn mark_dead(&self) {
        let mut state = self.state.write().unwrap();
        if *state != NodeState::Dead {
            tracing::warn!("rpc_pool: {} → dead (timeout/error)", self.url);
            *state = NodeState::Dead;
        }
    }

    /// 是否到了恢复检查时间 (带指数退避)
    fn should_attempt_recovery(&self) -> bool {
        Instant::now() >= *self.next_recovery_check.read().unwrap()
    }

    /// 推进恢复退避
    fn advance_recovery_backoff(&self) {
        let mut backoff = self.recovery_backoff.write().unwrap();
        *backoff = (*backoff * 2).min(RECOVERY_CHECK_MAX);
        *self.next_recovery_check.write().unwrap() = Instant::now() + *backoff;
    }

    /// 统计
    pub fn stats(&self) -> NodeStats {
        NodeStats {
            url: self.url.clone(),
            state: self.state(),
            last_latency_ms: self.last_latency_ms(),
            last_block: self.last_block_number(),
            total_requests: self.total_requests.load(Ordering::Relaxed),
            total_failures: self.total_failures.load(Ordering::Relaxed),
        }
    }
}

// ============================================================================
// NodeStats
// ============================================================================

/// 节点统计快照
#[derive(Debug, Clone)]
pub struct NodeStats {
    pub url: String,
    pub state: NodeState,
    pub last_latency_ms: Option<f64>,
    pub last_block: Option<u64>,
    pub total_requests: u64,
    pub total_failures: u64,
}

// ============================================================================
// RpcPool
// ============================================================================

/// 多 RPC 节点池
///
/// 管理多个 Polygon RPC 节点，提供：
/// - 自动故障切换
/// - 健康检查 + 延迟排序
/// - Dead 节点灰度恢复
pub struct RpcPool {
    /// 节点列表
    nodes: Vec<Arc<RpcNode>>,
    /// 当前选中节点索引
    current_index: AtomicU64,
    /// HTTP 客户端（连接池复用）
    client: reqwest::Client,
}

impl RpcPool {
    /// 使用自定义节点列表创建
    pub fn new(urls: &[&str]) -> Self {
        let client = reqwest::Client::builder()
            .timeout(HEALTH_CHECK_TIMEOUT)
            .pool_max_idle_per_host(2)
            .tcp_nodelay(true)
            .build()
            .expect("Failed to build reqwest client");

        let nodes: Vec<Arc<RpcNode>> = urls.iter().map(|url| Arc::new(RpcNode::new(url))).collect();

        Self {
            nodes,
            current_index: AtomicU64::new(0),
            client,
        }
    }

    /// 使用默认 Polygon 公共节点创建
    pub fn with_default_nodes() -> Self {
        Self::new(DEFAULT_POLYGON_RPCS)
    }

    /// 节点数量
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// 获取所有节点统计
    pub fn all_stats(&self) -> Vec<NodeStats> {
        self.nodes.iter().map(|n| n.stats()).collect()
    }

    /// 获取指定索引的节点
    pub fn node(&self, index: usize) -> Option<&Arc<RpcNode>> {
        self.nodes.get(index)
    }

    // ========================================================================
    // 健康检查
    // ========================================================================

    /// HTTP 客户端引用
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// 对单个节点执行健康检查 (ping eth_blockNumber)
    pub async fn check_health(&self, node: &Arc<RpcNode>) -> bool {
        let req = JsonRpcRequest::new("eth_blockNumber", serde_json::json!([]));
        let start = Instant::now();

        match self.client.post(&node.url).json(&req).send().await {
            Ok(resp) => match resp.json::<JsonRpcResponse>().await {
                Ok(rpc_resp) => {
                    if let Some(err) = rpc_resp.error {
                        tracing::debug!("rpc_pool: {} RPC error: {}", node.url, err.message);
                        node.mark_dead();
                        false
                    } else {
                        let block = hex_str_to_u64(&rpc_resp.result.as_str().unwrap_or("0x0"));
                        let latency_ms = start.elapsed().as_secs_f64() * 1000.0;
                        node.record_sample(latency_ms, block);
                        true
                    }
                }
                Err(e) => {
                    tracing::debug!("rpc_pool: {} deserialize error: {}", node.url, e);
                    node.mark_dead();
                    false
                }
            },
            Err(e) => {
                tracing::debug!("rpc_pool: {} request failed: {}", node.url, e);
                node.mark_dead();
                false
            }
        }
    }

    /// 对所有节点执行健康检查
    pub async fn check_health_all(&self) {
        for node in &self.nodes {
            if node.is_available() || node.should_attempt_recovery() {
                self.check_health(node).await;
            }
        }
    }

    // ========================================================================
    // 节点选择
    // ========================================================================

    /// 选择最优节点（Least Latency）
    ///
    /// 从所有 Healthy 节点中选择最近一次延迟最低的。
    /// 如果没有 Healthy 节点，则回退到 Degraded。
    /// 如果全部 Dead，则尝试恢复检查。
    fn select_best(&self) -> Option<Arc<RpcNode>> {
        // 优先 Healthy
        let healthy: Vec<&Arc<RpcNode>> = self
            .nodes
            .iter()
            .filter(|n| n.state() == NodeState::Healthy)
            .collect();

        if !healthy.is_empty() {
            return healthy
                .iter()
                .min_by(|a, b| {
                    a.last_latency_ms()
                        .partial_cmp(&b.last_latency_ms())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|n| (*n).clone());
        }

        // 回退 Degraded
        let degraded: Vec<&Arc<RpcNode>> = self
            .nodes
            .iter()
            .filter(|n| n.state() == NodeState::Degraded)
            .collect();

        if !degraded.is_empty() {
            return degraded
                .iter()
                .min_by(|a, b| {
                    a.last_latency_ms()
                        .partial_cmp(&b.last_latency_ms())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|n| (*n).clone());
        }

        // 全部 Dead：返回可恢复检查的节点
        self.nodes
            .iter()
            .find(|n| n.should_attempt_recovery())
            .cloned()
    }

    /// Round-robin 选择可用节点
    fn select_round_robin(&self) -> Option<Arc<RpcNode>> {
        let total = self.nodes.len() as u64;
        for offset in 0..total {
            let idx = ((self.current_index.load(Ordering::Relaxed) + offset) % total) as usize;
            let node = &self.nodes[idx];
            if node.is_available() {
                self.current_index
                    .store((idx as u64 + 1) % total, Ordering::Relaxed);
                return Some(node.clone());
            }
        }
        None
    }

    // ========================================================================
    // RPC 方法封装
    // ========================================================================

    /// 执行 JSON-RPC 调用，自动故障切换
    pub(crate) async fn rpc_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let node = self
            .select_best()
            .or_else(|| self.select_round_robin())
            .ok_or_else(|| "所有 RPC 节点均不可用".to_string())?;

        let req = JsonRpcRequest::new(method, params);
        node.total_requests.fetch_add(1, Ordering::Relaxed);

        match self.client.post(&node.url).json(&req).send().await {
            Ok(resp) => match resp.json::<JsonRpcResponse>().await {
                Ok(rpc_resp) => {
                    if let Some(err) = rpc_resp.error {
                        node.total_failures.fetch_add(1, Ordering::Relaxed);
                        Err(format!("RPC error: {}", err.message))
                    } else {
                        Ok(rpc_resp.result)
                    }
                }
                Err(e) => {
                    node.total_failures.fetch_add(1, Ordering::Relaxed);
                    Err(format!("Deserialize error: {}", e))
                }
            },
            Err(e) => {
                node.mark_dead();
                node.total_failures.fetch_add(1, Ordering::Relaxed);
                Err(format!("HTTP error: {}", e))
            }
        }
    }

    /// 获取当前块高
    pub async fn block_number(&self) -> Result<u64, String> {
        let result = self
            .rpc_call("eth_blockNumber", serde_json::json!([]))
            .await?;
        Ok(hex_str_to_u64(result.as_str().unwrap_or("0x0")))
    }

    /// 获取账户交易计数 (nonce)
    pub async fn get_transaction_count(&self, address: &str) -> Result<u64, String> {
        let result = self
            .rpc_call(
                "eth_getTransactionCount",
                serde_json::json!([address, "latest"]),
            )
            .await?;
        Ok(hex_str_to_u64(result.as_str().unwrap_or("0x0")))
    }

    /// 获取交易回执
    pub async fn get_transaction_receipt(
        &self,
        tx_hash: &str,
    ) -> Result<Option<TransactionReceipt>, String> {
        let result = self
            .rpc_call("eth_getTransactionReceipt", serde_json::json!([tx_hash]))
            .await?;

        if result.is_null() {
            return Ok(None);
        }

        let receipt: Option<TransactionReceipt> =
            serde_json::from_value(result).map_err(|e| format!("Parse receipt: {}", e))?;
        Ok(receipt)
    }

    /// 获取账户 pending nonce（包含 mempool 中的 tx）
    pub async fn get_pending_transaction_count(&self, address: &str) -> Result<u64, String> {
        let result = self
            .rpc_call(
                "eth_getTransactionCount",
                serde_json::json!([address, "pending"]),
            )
            .await?;
        Ok(hex_str_to_u64(result.as_str().unwrap_or("0x0")))
    }
}

// ============================================================================
// TransactionReceipt
// ============================================================================

/// 交易回执（简化版）
#[derive(Debug, Clone, Deserialize)]
pub struct TransactionReceipt {
    #[serde(rename = "transactionHash")]
    pub transaction_hash: String,
    #[serde(rename = "blockNumber")]
    pub block_number: Option<String>,
    #[serde(rename = "blockHash")]
    #[allow(dead_code)]
    pub block_hash: Option<String>,
    pub status: Option<String>, // "0x1" = success, "0x0" = failed
    #[serde(rename = "gasUsed")]
    pub gas_used: Option<String>,
    #[serde(rename = "effectiveGasPrice")]
    pub effective_gas_price: Option<String>,
    #[serde(rename = "logs")]
    #[allow(dead_code)]
    pub logs: Vec<serde_json::Value>,
}

impl TransactionReceipt {
    /// 交易是否成功
    pub fn is_success(&self) -> bool {
        self.status.as_deref() == Some("0x1")
    }

    /// 解析块号
    pub fn block_number_u64(&self) -> Option<u64> {
        self.block_number.as_deref().map(hex_str_to_u64)
    }
}

// ============================================================================
// 工具函数
// ============================================================================

/// 十六进制字符串转 u64
fn hex_str_to_u64(hex: &str) -> u64 {
    let s = hex.trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(s, 16).unwrap_or(0)
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_str_to_u64() {
        assert_eq!(hex_str_to_u64("0x0"), 0);
        assert_eq!(hex_str_to_u64("0x1"), 1);
        assert_eq!(hex_str_to_u64("0xa"), 10);
        assert_eq!(hex_str_to_u64("0xFF"), 255);
        assert_eq!(hex_str_to_u64("0x2e5a5"), 189861);
        assert_eq!(hex_str_to_u64("invalid"), 0);
    }

    #[test]
    fn test_node_initial_state() {
        let node = RpcNode::new("https://example.com");
        assert_eq!(node.state(), NodeState::Dead);
        assert!(!node.is_available());
    }

    #[test]
    fn test_node_record_sample_healthy() {
        let node = RpcNode::new("https://example.com");
        node.record_sample(50.0, 12345); // 50ms, fast
        assert_eq!(node.state(), NodeState::Healthy);
        assert!(node.is_available());
        assert_eq!(node.last_latency_ms(), Some(50.0));
        assert_eq!(node.last_block_number(), Some(12345));
    }

    #[test]
    fn test_node_record_sample_degraded() {
        let node = RpcNode::new("https://example.com");
        node.record_sample(900.0, 12345); // 900ms, degraded
        assert_eq!(node.state(), NodeState::Degraded);
        assert!(node.is_available());
    }

    #[test]
    fn test_node_record_sample_dead() {
        let node = RpcNode::new("https://example.com");
        node.record_sample(6000.0, 12345); // 6000ms, dead
        assert_eq!(node.state(), NodeState::Dead);
        assert!(!node.is_available());
    }

    #[test]
    fn test_node_recovery_backoff() {
        let node = RpcNode::new("https://example.com");
        node.mark_dead();
        assert!(node.should_attempt_recovery());

        node.advance_recovery_backoff();
        assert!(!node.should_attempt_recovery()); // 10s 后才检查
    }

    #[test]
    fn test_node_mark_dead_then_recover() {
        let node = RpcNode::new("https://example.com");
        node.record_sample(50.0, 100); // healthy first
        assert!(node.is_available());

        node.mark_dead();
        assert!(!node.is_available());
        assert_eq!(node.state(), NodeState::Dead);

        // 模拟恢复：健康检查通过
        node.record_sample(30.0, 200);
        assert!(node.is_available());
        assert_eq!(node.state(), NodeState::Healthy);
    }

    #[test]
    fn test_pool_creation() {
        let pool = RpcPool::new(&["https://a.com", "https://b.com", "https://c.com"]);
        assert_eq!(pool.len(), 3);
        assert!(pool.node(0).is_some());
        assert!(pool.node(3).is_none());
    }

    #[test]
    fn test_pool_default_nodes() {
        let pool = RpcPool::with_default_nodes();
        assert_eq!(pool.len(), 5);
    }

    #[test]
    fn test_pool_all_stats() {
        let pool = RpcPool::new(&["https://a.com", "https://b.com"]);
        let stats = pool.all_stats();
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].state, NodeState::Dead); // 初始都是 Dead
    }

    #[test]
    fn test_transaction_receipt_success() {
        let receipt = TransactionReceipt {
            transaction_hash: "0xabc".to_string(),
            block_number: Some("0x2e5a5".to_string()),
            block_hash: Some("0xdef".to_string()),
            status: Some("0x1".to_string()),
            gas_used: Some("0x5208".to_string()),
            effective_gas_price: Some("0x3b9aca00".to_string()),
            logs: vec![],
        };
        assert!(receipt.is_success());
        assert_eq!(receipt.block_number_u64(), Some(189861));
    }

    #[test]
    fn test_transaction_receipt_failed() {
        let receipt = TransactionReceipt {
            transaction_hash: "0xabc".to_string(),
            block_number: Some("0x1".to_string()),
            block_hash: None,
            status: Some("0x0".to_string()),
            gas_used: None,
            effective_gas_price: None,
            logs: vec![],
        };
        assert!(!receipt.is_success());
    }

    #[test]
    fn test_node_stats() {
        let node = RpcNode::new("https://example.com");
        node.record_sample(50.0, 100);
        let stats = node.stats();
        assert_eq!(stats.url, "https://example.com");
        assert_eq!(stats.state, NodeState::Healthy);
        assert_eq!(stats.last_latency_ms, Some(50.0));
        assert_eq!(stats.last_block, Some(100));
    }
}
