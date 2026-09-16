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
    pub fn new(
        client_order_id: String,
        token_id: String,
        side: String,
        requested_size: f64,
    ) -> Self {
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
    ///
    /// 绝对覆盖语义：调用方须传入「累计成交量」。用于下单路径 A——
    /// 下单响应的 making/taking 给出的是整单累计成交。
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

    /// 增量累加成交（P1-026）。
    ///
    /// 用于成交事件路径 B（`on_market_trade`）——每笔 trade 只有「单笔增量」，
    /// 直接调 `update_fill(trade.size)` 会绝对覆盖、丢掉跨笔 partial fill 的累计。
    /// 本方法把增量加到现有 `filled_size` 上，再复用 `update_fill` 的 clamp 与状态机。
    /// 非法增量（NaN / ≤0）直接忽略。
    pub fn apply_fill(&mut self, delta_size: f64) {
        if !delta_size.is_finite() || delta_size <= 0.0 {
            return;
        }
        self.update_fill(self.filled_size + delta_size);
    }

    /// 标记已取消。
    pub fn mark_cancelled(&mut self) {
        self.status = OrderStateStatus::Cancelled;
        self.updated_at = now_secs();
    }

    /// 标记已拒绝。
    pub fn mark_rejected(&mut self) {
        self.status = OrderStateStatus::Rejected;
        self.updated_at = now_secs();
    }

    /// 标记已过期。
    pub fn mark_expired(&mut self) {
        self.status = OrderStateStatus::Expired;
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

    /// 按交易所订单 ID 反向查找（P0-016）。
    ///
    /// 成交/恢复路径（`on_market_trade` / `apply_recovery`）只持有交易所分配的
    /// `exchange_order_id`，而本 store 以 `client_order_id` 为键，故需反向定位。
    /// 返回完整 `OrderState`（含 `client_order_id`），调用方继续 `upsert`
    /// 时仍按 `client_order_id` 键写回。
    pub fn get_by_exchange_order_id(&self, exchange_order_id: &str) -> Option<OrderState> {
        if exchange_order_id.is_empty() {
            return None;
        }
        self.orders
            .read()
            .unwrap()
            .values()
            .find(|s| s.exchange_order_id.as_deref() == Some(exchange_order_id))
            .cloned()
    }

    /// 写入/更新状态。
    pub fn upsert(&self, state: OrderState) {
        self.orders
            .write()
            .unwrap()
            .insert(state.client_order_id.clone(), state);
    }

    /// 新建 RECEIVED 状态并返回（用于 place_order 幂等占位）。
    pub fn create(
        &self,
        client_order_id: String,
        token_id: String,
        side: String,
        requested_size: f64,
    ) -> OrderState {
        let state = OrderState::new(client_order_id, token_id, side, requested_size);
        self.upsert(state.clone());
        state
    }

    /// 原子占位：`client_order_id` 不存在则创建并返回新状态；已存在则返回 `None`。
    ///
    /// 与 `create` 不同，本方法在**单次写锁**内完成「查重 + 插入」，
    /// 避免并发下 check-then-act 穿透导致重复下单（P0-004）。
    pub fn claim(
        &self,
        client_order_id: String,
        token_id: String,
        side: String,
        requested_size: f64,
    ) -> Option<OrderState> {
        let mut orders = self.orders.write().unwrap();
        if orders.contains_key(&client_order_id) {
            return None;
        }
        let state = OrderState::new(client_order_id, token_id, side, requested_size);
        orders.insert(state.client_order_id.clone(), state.clone());
        Some(state)
    }

    /// 标记拒绝（若存在）。用于下单失败路径清理占位，避免残留 `Received`
    /// 状态被 `pending_order_ids` 误报，或被幂等命中拦下导致无法重试。
    pub fn mark_rejected(&self, client_order_id: &str) {
        if let Some(mut st) = self.get(client_order_id) {
            st.mark_rejected();
            self.upsert(st);
        }
    }

    /// 幂等占位（支持重试）：`client_order_id` 不存在、或处于可重试终态
    /// （`Rejected` / `Expired` / `Cancelled`）时，创建/重置为 `Received` 并返回状态；
    /// 处于进行中或已成交则返回 `None`（拒绝重复下单）。
    ///
    /// 单次写锁内完成「查状态 + 插入/重置」，保证并发原子性（P0-004）。
    pub fn claim_or_retry(
        &self,
        client_order_id: String,
        token_id: String,
        side: String,
        requested_size: f64,
    ) -> Option<OrderState> {
        let mut orders = self.orders.write().unwrap();
        match orders.get(&client_order_id).map(|s| s.status) {
            None
            | Some(OrderStateStatus::Rejected)
            | Some(OrderStateStatus::Expired)
            | Some(OrderStateStatus::Cancelled) => {
                let state =
                    OrderState::new(client_order_id.clone(), token_id, side, requested_size);
                orders.insert(client_order_id, state.clone());
                Some(state)
            }
            _ => None,
        }
    }

    /// 订单总数。
    pub fn len(&self) -> usize {
        self.orders.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.orders.read().unwrap().is_empty()
    }
    /// 返回当前本地仍处于活动状态的订单 ID。
    ///
    /// 用于启动时 StateSync reconciliation。
    pub fn pending_order_ids(&self) -> std::collections::HashSet<String> {
        self.orders
            .read()
            .unwrap()
            .iter()
            .filter_map(|(id, state)| match state.status {
                OrderStateStatus::Received
                | OrderStateStatus::Submitted
                | OrderStateStatus::Live
                | OrderStateStatus::PartialFilled => Some(id.clone()),
                OrderStateStatus::Filled
                | OrderStateStatus::Cancelled
                | OrderStateStatus::Rejected
                | OrderStateStatus::Expired => None,
            })
            .collect()
    }
    /// 返回本地已经提交、且当前仍可能处于远端 open 状态的 exchange order ID。
    ///
    /// 用于 StateSync 与 CLOB open orders 做对账。
    pub fn pending_exchange_order_ids(&self) -> std::collections::HashSet<String> {
        self.orders
            .read()
            .unwrap()
            .values()
            .filter_map(|state| match state.status {
                OrderStateStatus::Submitted
                | OrderStateStatus::Live
                | OrderStateStatus::PartialFilled => state.exchange_order_id.clone(),

                OrderStateStatus::Received
                | OrderStateStatus::Filled
                | OrderStateStatus::Cancelled
                | OrderStateStatus::Rejected
                | OrderStateStatus::Expired => None,
            })
            .collect()
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

    /// P1-026：apply_fill 增量累加——多笔 partial fill 不互相覆盖。
    #[test]
    fn test_apply_fill_accumulates() {
        let mut s = OrderState::new("c-acc".to_string(), "t".to_string(), "BUY".to_string(), 100.0);
        s.apply_fill(30.0);
        assert_eq!(s.filled_size, 30.0);
        assert_eq!(s.status, OrderStateStatus::PartialFilled);

        s.apply_fill(40.0);
        assert_eq!(s.filled_size, 70.0, "第二笔应累加而非覆盖");
        assert_eq!(s.status, OrderStateStatus::PartialFilled);

        s.apply_fill(30.0);
        assert_eq!(s.filled_size, 100.0);
        assert_eq!(s.status, OrderStateStatus::Filled);
        assert_eq!(s.remaining_size, 0.0);
    }

    /// P1-026：apply_fill 忽略非法增量（NaN / <=0），并 clamp 到 requested。
    #[test]
    fn test_apply_fill_ignores_invalid_and_clamps() {
        let mut s = OrderState::new("c-clamp".to_string(), "t".to_string(), "BUY".to_string(), 100.0);
        s.apply_fill(0.0);
        s.apply_fill(-5.0);
        s.apply_fill(f64::NAN);
        assert_eq!(s.filled_size, 0.0, "非法增量不应改变 filled_size");

        s.apply_fill(120.0);
        assert_eq!(s.filled_size, 100.0, "累计超 requested 应 clamp 到 100");
        assert_eq!(s.status, OrderStateStatus::Filled);
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
    fn test_order_lifecycle_cancelled() {
        let mut s = OrderState::new("c3".to_string(), "t3".to_string(), "BUY".to_string(), 100.0);
        s.mark_submitted("ex3".to_string());
        s.mark_live();
        s.mark_cancelled();
        assert_eq!(s.status, OrderStateStatus::Cancelled);
    }

    #[test]
    fn test_order_lifecycle_rejected() {
        let mut s = OrderState::new("c4".to_string(), "t4".to_string(), "BUY".to_string(), 100.0);
        s.mark_rejected();
        assert_eq!(s.status, OrderStateStatus::Rejected);
    }

    #[test]
    fn test_order_lifecycle_expired() {
        let mut s = OrderState::new("c5".to_string(), "t5".to_string(), "BUY".to_string(), 100.0);
        s.mark_expired();
        assert_eq!(s.status, OrderStateStatus::Expired);
    }

    #[test]
    fn test_order_full_lifecycle_terminal_states() {
        // 完整生命周期：Received → Submitted → Live → PartialFilled → Filled
        let mut s = OrderState::new("c6".to_string(), "t6".to_string(), "BUY".to_string(), 100.0);
        assert_eq!(s.status, OrderStateStatus::Received);
        s.mark_submitted("ex6".to_string());
        assert_eq!(s.status, OrderStateStatus::Submitted);
        s.mark_live();
        assert_eq!(s.status, OrderStateStatus::Live);
        s.update_fill(50.0);
        assert_eq!(s.status, OrderStateStatus::PartialFilled);
        s.update_fill(100.0);
        assert_eq!(s.status, OrderStateStatus::Filled);
        assert_eq!(s.remaining_size, 0.0);
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

    /// P0-016：按 exchange_order_id 反向查找（成交/恢复路径只持有交易所订单 ID）。
    #[test]
    fn test_get_by_exchange_order_id() {
        let store = OrderStore::new();
        let mut st = store
            .create("client-1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
        st.mark_submitted("exchange-1".to_string());
        store.upsert(st);

        // 命中：通过 exchange_order_id 找回 client_order_id 键的订单
        let found = store.get_by_exchange_order_id("exchange-1");
        assert!(found.is_some());
        assert_eq!(found.unwrap().client_order_id, "client-1");

        // 未命中
        assert!(store.get_by_exchange_order_id("exchange-missing").is_none());
        // 空 id 直接返回 None（不误命中 exchange_order_id 为空串的订单）
        assert!(store.get_by_exchange_order_id("").is_none());
    }

    /// 并发去重：16 个线程同时 claim 同一 order_id，应只有一次成功（原子占位）。
    #[test]
    fn test_order_store_concurrent_dedup() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        let store = Arc::new(OrderStore::new());
        let id = "concurrent-1";
        let claimed = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(16));

        let mut handles = Vec::new();
        for _ in 0..16 {
            let store = Arc::clone(&store);
            let claimed = Arc::clone(&claimed);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                // 原子 claim：只有一个线程能成功占位
                if store
                    .claim(id.to_string(), "t1".to_string(), "BUY".to_string(), 10.0)
                    .is_some()
                {
                    claimed.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(store.len(), 1, "store 应只保留一条订单");
        assert_eq!(
            claimed.load(Ordering::SeqCst),
            1,
            "并发提交同一 order_id 应只有一次 claim 成功（原子占位）"
        );
    }

    /// claim_or_retry：下单失败标记 rejected 后，同 order_id 可重新占位（重试）。
    #[test]
    fn test_claim_retry_after_rejected() {
        let store = OrderStore::new();

        // 首次占位成功
        assert!(store
            .claim_or_retry("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0)
            .is_some());

        // 进行中：重复 claim_or_retry 返回 None（幂等去重）
        assert!(store
            .claim_or_retry("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0)
            .is_none());

        // 下单失败 → 标记 rejected
        store.mark_rejected("o1");
        assert_eq!(
            store.get("o1").map(|s| s.status),
            Some(OrderStateStatus::Rejected)
        );

        // rejected 后可重试：claim_or_retry 重新占位成功
        assert!(store
            .claim_or_retry("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0)
            .is_some());
    }

    /// 失败路径 mark_rejected 后，订单不再进入 pending（避免对账误报）。
    #[test]
    fn test_mark_rejected_clears_pending() {
        let store = OrderStore::new();
        store.create("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
        assert!(store.pending_order_ids().contains("o1"));

        store.mark_rejected("o1");
        assert!(!store.pending_order_ids().contains("o1"));
    }

    /// claim_or_retry 对已成交订单去重（不可重复下单）。
    #[test]
    fn test_claim_or_retry_rejects_filled() {
        let store = OrderStore::new();
        store.create("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
        if let Some(mut st) = store.get("o1") {
            st.update_fill(10.0); // Filled
            store.upsert(st);
        }
        assert!(store
            .claim_or_retry("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0)
            .is_none());
    }
}
