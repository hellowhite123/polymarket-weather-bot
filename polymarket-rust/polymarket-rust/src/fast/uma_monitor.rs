//! UMA 预言机监控 (UMA Monitor)
//!
//! 追踪 UMA Optimistic Oracle 在 Polygon 上的核心事件，
//! 通过 RpcPool HTTP 轮询 `eth_getLogs` 实现，不依赖 ethers WebSocket。
//!
//! ## 背景
//!
//! Polymarket 使用 UMA Optimistic Oracle (OO) 结算二元预测市场。
//! 结算流程:
//!   1. ProposePrice — 提议结算价格
//!   2. 等待挑战窗口 (~2 小时)
//!   3. 若无争议 → 自动结算；有争议 → UMA 投票
//!   4. PriceSettled — 价格最终确定
//!
//! ## 监听的合约
//!
//! - UMA OO v3: 0xeE3Afe347D5C74317041E2618C49534dAf887c24 (Polygon)
//! - Polymarket CTF: 0x4D97DCd97eC945f40cF65F87097ACe5EA0476045
//!
//! ## 集成
//!
//! UMA 事件 → OracleDisputeMonitor (risk_engine.rs) → 预警/争议建议

use crate::fast::rpc_pool::RpcPool;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

// ============================================================================
// 常量
// ============================================================================

/// UMA Optimistic Oracle V3 on Polygon
pub const UMA_OO_POLYGON: &str = "0xeE3Afe347D5C74317041E2618C49534dAf887c24";

/// Polymarket CTF contract on Polygon
pub const POLYMARKET_CTF_POLYGON: &str = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045";

/// 默认轮询间隔
pub const DEFAULT_UMA_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// 查询的块范围（每次向前追溯）
pub const DEFAULT_BLOCK_RANGE: u64 = 5000;

// ============================================================================
// UMA 事件类型
// ============================================================================

/// UMA 预言机事件
#[derive(Debug, Clone)]
pub enum UmaEvent {
    /// 价格提议：有人提交了结算价格
    PriceProposed(PriceProposedEvent),
    /// 价格争议：有人对提议价格提出质疑
    PriceDisputed(PriceDisputedEvent),
    /// 结算完成：价格已最终确定
    PriceSettled(PriceSettledEvent),
}

/// 价格提议事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceProposedEvent {
    pub tx_hash: String,
    pub block_number: u64,
    /// 请求者地址（Polymarket 的合约）
    pub requester: String,
    /// 标识符（如 "YES_OR_NO_QUERY"）
    pub identifier: String,
    /// UMA 内部时间戳
    pub timestamp: u64,
    /// 提议者
    pub proposer: String,
    /// 提议价格（链上整数格式，需 /1e18）
    pub proposed_price: String,
    /// 过期时间
    pub expiration_timestamp: u64,
}

/// 价格争议事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceDisputedEvent {
    pub tx_hash: String,
    pub block_number: u64,
    pub requester: String,
    pub identifier: String,
    pub timestamp: u64,
    pub disputer: String,
}

/// 结算完成事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceSettledEvent {
    pub tx_hash: String,
    pub block_number: u64,
    pub requester: String,
    pub identifier: String,
    pub timestamp: u64,
    pub price: String,
}

// ============================================================================
// UmaMonitor
// ============================================================================

/// UMA 预言机监控器
///
/// 通过 RpcPool HTTP 轮询 `eth_getLogs` 获取 UMA OO 事件。
/// 维护：
/// - 当前活跃的提议
/// - 争议中的提议
/// - 已结算的提议
pub struct UmaMonitor {
    /// RPC 节点池
    rpc: Arc<RpcPool>,
    /// 上次查询的块号
    last_block: RwLock<u64>,
    /// 未知的结算请求（需外部映射 token_id）
    /// 活跃提议: HashMap<identifier, PriceProposedEvent>
    active_proposals: RwLock<HashMap<String, PriceProposedEvent>>,
    /// 争议历史
    disputes: RwLock<Vec<PriceDisputedEvent>>,
    /// 统计
    polls: AtomicU64,
    proposals_seen: AtomicU64,
    disputes_seen: AtomicU64,
    settlements_seen: AtomicU64,
    /// UMA 合约地址（可配置）
    uma_contract: String,
    /// 查看的块范围
    block_range: u64,
}

impl UmaMonitor {
    /// 创建监控器
    pub fn new(rpc: Arc<RpcPool>) -> Self {
        Self {
            rpc,
            last_block: RwLock::new(0),
            active_proposals: RwLock::new(HashMap::new()),
            disputes: RwLock::new(Vec::new()),
            polls: AtomicU64::new(0),
            proposals_seen: AtomicU64::new(0),
            disputes_seen: AtomicU64::new(0),
            settlements_seen: AtomicU64::new(0),
            uma_contract: UMA_OO_POLYGON.to_string(),
            block_range: DEFAULT_BLOCK_RANGE,
        }
    }

    /// 设置自定义 UMA 合约地址
    pub fn with_contract(mut self, address: &str) -> Self {
        self.uma_contract = address.to_string();
        self
    }

    // ========================================================================
    // 核心轮询
    // ========================================================================

    /// 执行一次事件轮询
    ///
    /// 查询自上次轮询以来 UMA OO 上的新事件。
    pub async fn poll(&self) -> Result<Vec<UmaEvent>, String> {
        self.polls.fetch_add(1, Ordering::Relaxed);

        let current_block = self.rpc.block_number().await?;
        let mut last = self.last_block.write().unwrap();

        let from_block = if *last == 0 {
            current_block.saturating_sub(self.block_range)
        } else {
            *last + 1
        };
        let to_block = current_block;

        if from_block > to_block {
            return Ok(Vec::new());
        }

        let events = self.fetch_uma_logs(from_block, to_block).await?;
        *last = to_block;

        Ok(events)
    }

    // ========================================================================
    // eth_getLogs 查询
    // ========================================================================

    async fn fetch_uma_logs(&self, from: u64, to: u64) -> Result<Vec<UmaEvent>, String> {
        let mut events = Vec::new();

        // === ProposePrice 事件 ===
        // keccak256("ProposePrice(address,bytes32,uint256,bytes,address,uint256,uint256,address)")
        let propose_topic = "0x9a97c27ef7eb41d57c2bafed28a934460bda276ae798b23e133748eb6d242061";

        let result = self
            .rpc
            .rpc_call(
                "eth_getLogs",
                serde_json::json!([{
                    "address": self.uma_contract,
                    "fromBlock": format!("0x{:x}", from),
                    "toBlock": format!("0x{:x}", to),
                    "topics": [propose_topic],
                }]),
            )
            .await?;

        if let Some(logs) = result.as_array() {
            for log in logs {
                if let Ok(event) = self.parse_propose_event(log) {
                    self.proposals_seen.fetch_add(1, Ordering::Relaxed);
                    self.active_proposals
                        .write()
                        .unwrap()
                        .insert(event.identifier.clone(), event.clone());
                    events.push(UmaEvent::PriceProposed(event));
                }
            }
        }

        // === DisputePrice 事件 ===
        let dispute_topic = "0x34efe5c1ccb02c70ab625b41848ac7f2c7bd2eafc61c9fb98e76a479c3ecaf52";

        let result = self
            .rpc
            .rpc_call(
                "eth_getLogs",
                serde_json::json!([{
                    "address": self.uma_contract,
                    "fromBlock": format!("0x{:x}", from),
                    "toBlock": format!("0x{:x}", to),
                    "topics": [dispute_topic],
                }]),
            )
            .await?;

        if let Some(logs) = result.as_array() {
            for log in logs {
                if let Ok(event) = self.parse_dispute_event(log) {
                    self.disputes_seen.fetch_add(1, Ordering::Relaxed);
                    self.disputes.write().unwrap().push(event.clone());
                    events.push(UmaEvent::PriceDisputed(event));
                }
            }
        }

        Ok(events)
    }

    // ========================================================================
    // 事件解析
    // ========================================================================

    fn parse_propose_event(&self, log: &serde_json::Value) -> Result<PriceProposedEvent, String> {
        let tx_hash = log["transactionHash"].as_str().unwrap_or("").to_string();
        let block = hex_to_u64(log["blockNumber"].as_str().unwrap_or("0x0"));
        let topics = log["topics"].as_array().ok_or("no topics")?;
        let data = log["data"].as_str().unwrap_or("0x");

        // topics[1] = requester (indexed), topics[2] = identifier (indexed)
        let requester = topic_to_address(topics.get(1));
        let identifier = topic_to_bytes32(topics.get(2));

        // data = timestamp(uint256) + ancillaryData(offset, bytes) + proposer(address) + proposedPrice(uint256) + expirationTimestamp(uint256) + currency(address)
        let data_clean = data.trim_start_matches("0x");
        if data_clean.len() < 64 {
            return Err("data too short".into());
        }

        // First word: timestamp (uint256)
        let timestamp = u64::from_str_radix(&data_clean[0..64], 16).unwrap_or(0);

        Ok(PriceProposedEvent {
            tx_hash,
            block_number: block,
            requester,
            identifier,
            timestamp,
            proposer: String::new(),
            proposed_price: String::new(),
            expiration_timestamp: 0,
        })
    }

    fn parse_dispute_event(&self, log: &serde_json::Value) -> Result<PriceDisputedEvent, String> {
        let tx_hash = log["transactionHash"].as_str().unwrap_or("").to_string();
        let block = hex_to_u64(log["blockNumber"].as_str().unwrap_or("0x0"));
        let topics = log["topics"].as_array().ok_or("no topics")?;

        let requester = topic_to_address(topics.get(1));
        let identifier = topic_to_bytes32(topics.get(2));
        let disputer = topic_to_address(topics.get(3));

        let data = log["data"].as_str().unwrap_or("0x");
        let data_clean = data.trim_start_matches("0x");
        let timestamp = if data_clean.len() >= 64 {
            u64::from_str_radix(&data_clean[0..64], 16).unwrap_or(0)
        } else {
            0
        };

        Ok(PriceDisputedEvent {
            tx_hash,
            block_number: block,
            requester,
            identifier,
            timestamp,
            disputer,
        })
    }

    // ========================================================================
    // 查询
    // ========================================================================

    /// 获取所有活跃提议
    pub fn active_proposals(&self) -> Vec<PriceProposedEvent> {
        self.active_proposals
            .read()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// 获取争议历史
    pub fn disputes(&self) -> Vec<PriceDisputedEvent> {
        self.disputes.read().unwrap().clone()
    }

    /// 检查指定 identifier 是否有活跃提议
    pub fn has_active_proposal(&self, identifier: &str) -> bool {
        self.active_proposals
            .read()
            .unwrap()
            .contains_key(identifier)
    }

    /// 检查指定 identifier 是否有争议
    pub fn has_dispute(&self, identifier: &str) -> bool {
        self.disputes
            .read()
            .unwrap()
            .iter()
            .any(|d| d.identifier == identifier)
    }

    /// 统计报告
    pub fn stats(&self) -> UmaStats {
        UmaStats {
            polls: self.polls.load(Ordering::Relaxed),
            proposals_seen: self.proposals_seen.load(Ordering::Relaxed),
            disputes_seen: self.disputes_seen.load(Ordering::Relaxed),
            settlements_seen: self.settlements_seen.load(Ordering::Relaxed),
            active_count: self.active_proposals.read().unwrap().len(),
            dispute_count: self.disputes.read().unwrap().len(),
            last_block: *self.last_block.read().unwrap(),
        }
    }
}

// ============================================================================
// UmaStats
// ============================================================================

#[derive(Debug, Clone, Default)]
pub struct UmaStats {
    pub polls: u64,
    pub proposals_seen: u64,
    pub disputes_seen: u64,
    pub settlements_seen: u64,
    pub active_count: usize,
    pub dispute_count: usize,
    pub last_block: u64,
}

// ============================================================================
// 工具
// ============================================================================

fn hex_to_u64(hex: &str) -> u64 {
    u64::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap_or(0)
}

fn topic_to_address(topic: Option<&serde_json::Value>) -> String {
    match topic.and_then(|t| t.as_str()) {
        Some(s) if s.len() >= 42 => {
            let raw = s.trim_start_matches("0x");
            format!("0x{}", &raw[24..64]) // last 20 bytes = address
        }
        _ => String::new(),
    }
}

fn topic_to_bytes32(topic: Option<&serde_json::Value>) -> String {
    topic.and_then(|t| t.as_str()).unwrap_or("").to_string()
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_to_u64() {
        assert_eq!(hex_to_u64("0x0"), 0);
        assert_eq!(hex_to_u64("0x1"), 1);
        assert_eq!(hex_to_u64("0xFF"), 255);
    }

    #[test]
    fn test_topic_to_address() {
        let topic =
            serde_json::json!("0x000000000000000000000000ee3afe347d5c74317041e2618c49534daf887c24");
        let addr = topic_to_address(Some(&topic));
        assert_eq!(
            addr.to_lowercase(),
            "0xee3afe347d5c74317041e2618c49534daf887c24"
        );
    }

    #[test]
    fn test_monitor_creation() {
        let pool = Arc::new(RpcPool::new(&["https://polygon-rpc.com"]));
        let mon = UmaMonitor::new(pool);
        assert_eq!(mon.stats().polls, 0);
        assert_eq!(mon.stats().active_count, 0);
    }

    #[test]
    fn test_monitor_parse_empty_log() {
        let pool = Arc::new(RpcPool::new(&["https://polygon-rpc.com"]));
        let mon = UmaMonitor::new(pool);
        let log = serde_json::json!({
            "transactionHash": "0xabc",
            "blockNumber": "0x1",
            "topics": ["0x0"],
            "data": "0x"
        });
        // Should not panic
        let _ = mon.parse_propose_event(&log);
        let _ = mon.parse_dispute_event(&log);
    }
}
