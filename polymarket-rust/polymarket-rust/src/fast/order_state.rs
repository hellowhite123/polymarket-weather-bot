//! 订单状态存储 (OrderStore) — R2-P0-05/06 修复
//!
//! 建立订单状态机 + 幂等去重：
//!   - `OrderState`：完整订单状态（client_order_id / exchange_order_id /
//!     requested_size / filled_size / remaining_size / status / 时间戳）
//!   - `OrderStore`：内存 HashMap 存储，`place_order` 前查重（幂等），
//!     提交后记录状态，fill 时更新。
//!
//! 当前为内存实现；生产需持久化（SQLite/append-only JSONL），见 R2-P2。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// 当前 unix 秒。
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ============================================================================
// 订单状态
// ============================================================================

/// 订单状态（R2-P0-05 状态机）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStateStatus {
    Received,
    Submitted,
    Live,
    PartialFilled,
    Filled,
    Cancelled,
    Rejected,
    Expired,
}

impl std::fmt::Display for OrderStateStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            OrderStateStatus::Received => "received",
            OrderStateStatus::Submitted => "submitted",
            OrderStateStatus::Live => "live",
            OrderStateStatus::PartialFilled => "partial_filled",
            OrderStateStatus::Filled => "filled",
            OrderStateStatus::Cancelled => "cancelled",
            OrderStateStatus::Rejected => "rejected",
            OrderStateStatus::Expired => "expired",
        };
        write!(f, "{}", s)
    }
}

/// 单笔订单的完整状态（R2-P0-05）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderState {
    pub client_order_id: String,
    pub exchange_order_id: Option<String>,
    pub token_id: String,
    pub side: String, // BUY / SELL
    pub requested_size: f64,
    pub filled_size: f64,
    pub remaining_size: f64,
    pub status: OrderStateStatus,
    pub created_at: u64,
    pub updated_at: u64,
}

impl OrderState {
    /// 新建 RECEIVED 状态。
    pub fn new(client_order_id: String, token_id: String, side: String, requested_size: f64) -> Self {
        let now = now_secs();
        Self {
            client_order_id,
            exchange_order_id: None,
            token_id,
            side,
            requested_size,
            filled_size: 0.0,
            remaining_size: requested_size,
            status: OrderStateStatus::Received,
            created_at: now,
            updated_at: now,
        }
    }

    /// 记录提交（获得 exchange_order_id）。
    pub fn mark_submitted(&mut self, exchange_order_id: String) {
        self.exchange_order_id = Some(exchange_order_id);
        self.status = OrderStateStatus::Submitted;
        self.updated_at = now_secs();
    }

    /// 记录 live（挂单，未成交）。
    pub fn mark_live(&mut self) {
        self.status = OrderStateStatus::Live;
        self.updated_at = now_secs();
    }

    /// 更新成交数量（partial fill 时 filled < requested）。
    pub fn update_fill(&mut self, filled_size: f64) {
        self.filled_size = filled_size.max(0.0).min(self.requested_size);
        self.remaining_size = (self.requested_size - self.filled_size).max(0.0);
        self.status = if self.filled_size >= self.requested_size {
            OrderStateStatus::Filled
        } else if self.filled_size > 0.0 {
            OrderStateStatus::PartialFilled
        } else {
            OrderStateStatus::Live
        };
        self.updated_at = now_secs();
    }
}

// ============================================================================
// OrderStore
// ============================================================================

/// 订单状态存储（线程安全，内存实现）。
#[derive(Debug, Default)]
pub struct OrderStore {
    orders: RwLock<HashMap<String, OrderState>>,
}

impl OrderStore {
    pub fn new() -> Self {
        Self {
            orders: RwLock::new(HashMap::new()),
        }
    }

    /// 幂等查询：order_id 已存在则返回已有状态（R2-P0-06）。
    pub fn get(&self, client_order_id: &str) -> Option<OrderState> {
        self.orders.read().unwrap().get(client_order_id).cloned()
    }

    /// 写入/更新状态。
    pub fn upsert(&self, state: OrderState) {
        self.orders
            .write()
            .unwrap()
            .insert(state.client_order_id.clone(), state);
    }

    /// 新建 RECEIVED 状态并返回（用于 place_order 幂等占位）。
    pub fn create(&self, client_order_id: String, token_id: String, side: String, requested_size: f64) -> OrderState {
        let state = OrderState::new(client_order_id, token_id, side, requested_size);
        self.upsert(state.clone());
        state
    }

    /// 订单总数。
    pub fn len(&self) -> usize {
        self.orders.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.orders.read().unwrap().is_empty()
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_order_state_lifecycle() {
        let mut s = OrderState::new("c1".to_string(), "t1".to_string(), "BUY".to_string(), 100.0);
        assert_eq!(s.status, OrderStateStatus::Received);
        assert_eq!(s.requested_size, 100.0);
        assert_eq!(s.filled_size, 0.0);

        s.mark_submitted("ex1".to_string());
        assert_eq!(s.exchange_order_id.as_deref(), Some("ex1"));

        s.mark_live();
        assert_eq!(s.status, OrderStateStatus::Live);

        // partial fill：100 请求，37 成交
        s.update_fill(37.0);
        assert_eq!(s.filled_size, 37.0);
        assert_eq!(s.remaining_size, 63.0);
        assert_eq!(s.status, OrderStateStatus::PartialFilled);

        // 全额成交
        s.update_fill(100.0);
        assert_eq!(s.status, OrderStateStatus::Filled);
        assert_eq!(s.remaining_size, 0.0);
    }

    #[test]
    fn test_partial_fill_not_full() {
        let mut s = OrderState::new("c2".to_string(), "t2".to_string(), "BUY".to_string(), 100.0);
        s.update_fill(37.0);
        // R2-P0-03：matched 不等于全额，partial 时 filled=37, remaining=63
        assert_eq!(s.filled_size, 37.0);
        assert_eq!(s.remaining_size, 63.0);
        assert_eq!(s.status, OrderStateStatus::PartialFilled);
    }

    #[test]
    fn test_order_store_idempotency() {
        let store = OrderStore::new();
        assert!(store.is_empty());

        store.create("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
        assert_eq!(store.len(), 1);

        // 幂等：同 order_id 返回已有状态，不新建
        let existing = store.get("o1");
        assert!(existing.is_some());
        assert_eq!(store.len(), 1);

        assert!(store.get("missing").is_none());
    }
}
