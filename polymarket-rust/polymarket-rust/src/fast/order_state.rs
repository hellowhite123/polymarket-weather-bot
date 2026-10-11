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
    /// G4 / P1-RISK-006：下单价（limit price），用于取消时按 remaining×price 释放预留。
    pub limit_price: f64,
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
            limit_price: 0.0,
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

    /// 按交易所订单 ID 定位订单，并在**单次写锁内**增量累加成交（P1-027）。
    ///
    /// 并发 fill 场景下，`get_by_exchange_order_id → clone → apply_fill → upsert`
    /// 是读-改-写非原子：两笔 fill 各自读到旧的 `filled_size`，各自累加后
    /// `upsert`，后写覆盖先写，造成 lost update。
    ///
    /// 本方法在单次写锁内完成「定位 + 增量 + 写回」，并发 fill 串行化。
    /// 返回更新后的 `OrderState`（命中时）；未命中或非法增量（NaN / ≤0）返回 `None`。
    pub fn apply_fill_by_exchange_order_id(
        &self,
        exchange_order_id: &str,
        delta_size: f64,
    ) -> Option<OrderState> {
        if exchange_order_id.is_empty() || !delta_size.is_finite() || delta_size <= 0.0 {
            return None;
        }
        let mut orders = self.orders.write().unwrap();
        let mut updated: Option<OrderState> = None;
        for state in orders.values_mut() {
            if state.exchange_order_id.as_deref() == Some(exchange_order_id) {
                state.apply_fill(delta_size);
                updated = Some(state.clone());
                break;
            }
        }
        updated
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

    /// 原子标记拒绝（P1-028）。
    ///
    /// 用于下单失败路径清理占位，避免残留 `Received` 状态被 `pending_order_ids`
    /// 误报，或被幂等命中拦下导致无法重试。
    ///
    /// 原实现 `get → mark_rejected → upsert` 是读-改-写非原子：并发下，已成交单
    /// （`Filled`）可能被并发的 `mark_rejected` 覆盖为 `Rejected`（状态倒退）。
    /// 本方法在**单次写锁内**完成「定位 + 状态转换」，且只允许从非终态转换：
    /// 已是终态（`Filled`/`Cancelled`/`Rejected`/`Expired`）则跳过，不覆盖。
    pub fn mark_rejected(&self, client_order_id: &str) {
        let mut orders = self.orders.write().unwrap();
        if let Some(st) = orders.get_mut(client_order_id) {
            match st.status {
                OrderStateStatus::Received
                | OrderStateStatus::Submitted
                | OrderStateStatus::Live
                | OrderStateStatus::PartialFilled => {
                    st.mark_rejected();
                }
                // 终态不覆盖（尤其 Filled 不能被 reject 倒退）
                OrderStateStatus::Filled
                | OrderStateStatus::Cancelled
                | OrderStateStatus::Rejected
                | OrderStateStatus::Expired => {}
            }
        }
    }

    /// G4 / P1-RISK-006：记录下单价（limit price），供取消时按
    /// `remaining_size × limit_price` 计算并释放剩余预留。
    pub fn set_limit_price(&self, client_order_id: &str, price: f64) {
        if !price.is_finite() || price < 0.0 {
            return;
        }
        let mut orders = self.orders.write().unwrap();
        if let Some(st) = orders.get_mut(client_order_id) {
            st.limit_price = price;
        }
    }

    /// G4 / P1-RISK-006：原子标记取消（终态保护，不覆盖已 `Filled` 等终态）。
    ///
    /// 与 `mark_rejected` 同样在单次写锁内完成「定位 + 状态转换」，避免
    /// `get → mark_cancelled → upsert` 的读-改-写竞争把并发成交覆盖回 Cancelled。
    pub fn mark_cancelled(&self, client_order_id: &str) -> Option<OrderState> {
        let mut orders = self.orders.write().unwrap();
        let st = orders.get_mut(client_order_id)?;
        match st.status {
            OrderStateStatus::Received
            | OrderStateStatus::Submitted
            | OrderStateStatus::Live
            | OrderStateStatus::PartialFilled => {
                st.mark_cancelled();
                Some(st.clone())
            }
            OrderStateStatus::Filled
            | OrderStateStatus::Cancelled
            | OrderStateStatus::Rejected
            | OrderStateStatus::Expired => None,
        }
    }

    /// G4 / P1-RISK-006：按交易所订单 ID 原子标记取消（终态保护）。
    pub fn mark_cancelled_by_exchange_order_id(
        &self,
        exchange_order_id: &str,
    ) -> Option<OrderState> {
        if exchange_order_id.is_empty() {
            return None;
        }
        let mut orders = self.orders.write().unwrap();
        for st in orders.values_mut() {
            if st.exchange_order_id.as_deref() == Some(exchange_order_id) {
                return match st.status {
                    OrderStateStatus::Received
                    | OrderStateStatus::Submitted
                    | OrderStateStatus::Live
                    | OrderStateStatus::PartialFilled => {
                        st.mark_cancelled();
                        Some(st.clone())
                    }
                    OrderStateStatus::Filled
                    | OrderStateStatus::Cancelled
                    | OrderStateStatus::Rejected
                    | OrderStateStatus::Expired => None,
                };
            }
        }
        None
    }

    /// G4 / P1-RISK-006：返回所有活动（非终态）订单快照，供 `cancel_all`
    /// 逐一释放预留并标记取消。
    pub fn active_orders(&self) -> Vec<OrderState> {
        self.orders
            .read()
            .unwrap()
            .values()
            .filter(|s| {
                matches!(
                    s.status,
                    OrderStateStatus::Received
                        | OrderStateStatus::Submitted
                        | OrderStateStatus::Live
                        | OrderStateStatus::PartialFilled
                )
            })
            .cloned()
            .collect()
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

    /// B2 / P1-009：返回 in-doubt（下单结果未知）的本地订单快照。
    ///
    /// in-doubt 定义：`status == Received` 且尚无 `exchange_order_id`
    /// （submit 超时 / 5xx / 连接中断，无法得知交易所是否已受理）。
    /// 这类订单**没有**交易所订单 ID，无法参与 `pending_exchange_order_ids`
    /// 的按 ID 对账（P1-009 根因），必须单独作为 reconciliation 候选，
    /// 由 `state_sync::resolve_in_doubt_orders` 匹配远端 open order。
    pub fn in_doubt_orders(&self) -> Vec<OrderState> {
        self.orders
            .read()
            .unwrap()
            .values()
            .filter(|s| s.status == OrderStateStatus::Received && s.exchange_order_id.is_none())
            .cloned()
            .collect()
    }

    /// B2：把 in-doubt 订单绑定到远端唯一命中的 open order（Received → Submitted）。
    ///
    /// - 幂等：重复绑定返回 `None`（已 Submitted / 已有 exchange id）——重放对账无副作用。
    /// - 一对一冲突防护：任何**其他**本地订单已持有同一 `exchange_order_id` 时拒绝。
    /// - 绑定不改变预留：订单始终是 active，reserve 已持有，无释放/累加。
    pub fn bind_remote_order(
        &self,
        client_order_id: &str,
        exchange_order_id: &str,
    ) -> Option<OrderState> {
        if exchange_order_id.trim().is_empty() {
            return None;
        }
        let mut orders = self.orders.write().unwrap();
        let duplicate = orders.values().any(|s| {
            s.client_order_id != client_order_id
                && s.exchange_order_id.as_deref() == Some(exchange_order_id)
        });
        if duplicate {
            return None;
        }
        let state = orders.get_mut(client_order_id)?;
        if state.status != OrderStateStatus::Received || state.exchange_order_id.is_some() {
            return None;
        }
        state.mark_submitted(exchange_order_id.to_string());
        Some(state.clone())
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

    /// P1-027：并发 fill 无 lost update——10 线程各自 +10，单写锁内串行化，累计 100。
    #[test]
    fn test_apply_fill_by_exchange_order_id_concurrent_no_lost_update() {
        use std::sync::{Arc, Barrier};

        let store = Arc::new(OrderStore::new());
        let mut st = store.create("client-1".to_string(), "t1".to_string(), "BUY".to_string(), 100.0);
        st.mark_submitted("exchange-1".to_string());
        store.upsert(st);

        let barrier = Arc::new(Barrier::new(10));
        let mut handles = Vec::new();
        for _ in 0..10 {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                store.apply_fill_by_exchange_order_id("exchange-1", 10.0);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let st = store.get("client-1").unwrap();
        assert_eq!(
            st.filled_size, 100.0,
            "10 笔并发 fill ×10 应累计到 100，读-改-写非原子会丢更新"
        );
        assert_eq!(st.status, OrderStateStatus::Filled);
        assert_eq!(st.remaining_size, 0.0);
    }

    /// P1-027：非法增量（NaN/≤0/空 id）在 apply_fill_by_exchange_order_id 被拒绝。
    #[test]
    fn test_apply_fill_by_exchange_order_id_rejects_invalid() {
        let store = OrderStore::new();
        let mut st = store.create("client-1".to_string(), "t1".to_string(), "BUY".to_string(), 100.0);
        st.mark_submitted("exchange-1".to_string());
        store.upsert(st);

        assert!(store.apply_fill_by_exchange_order_id("exchange-1", 0.0).is_none());
        assert!(store.apply_fill_by_exchange_order_id("exchange-1", -5.0).is_none());
        assert!(store.apply_fill_by_exchange_order_id("exchange-1", f64::NAN).is_none());
        assert!(store.apply_fill_by_exchange_order_id("", 10.0).is_none());
        assert!(store.apply_fill_by_exchange_order_id("missing", 10.0).is_none());

        let st = store.get("client-1").unwrap();
        assert_eq!(st.filled_size, 0.0, "非法增量不应改变 filled_size");
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

    /// P1-028：已成交单（Filled）不被 mark_rejected 倒退覆盖。
    #[test]
    fn test_mark_rejected_does_not_overwrite_filled() {
        let store = OrderStore::new();
        let mut st = store.create("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
        st.mark_submitted("ex1".to_string());
        st.update_fill(10.0); // Filled
        store.upsert(st);

        store.mark_rejected("o1"); // 尝试 reject 已成交单

        let st = store.get("o1").unwrap();
        assert_eq!(
            st.status,
            OrderStateStatus::Filled,
            "已成交单不应被 mark_rejected 倒退为 Rejected"
        );
        assert_eq!(st.filled_size, 10.0);
    }

    /// P1-028：并发 fill 与 reject 竞争——Filled 后 reject 不倒退；
    /// 结果要么 Filled（fill 赢），要么 Rejected（reject 先到），但 status 与 filled_size 不自相矛盾。
    #[test]
    fn test_concurrent_fill_and_reject_no_regression() {
        use std::sync::{Arc, Barrier};

        let store = Arc::new(OrderStore::new());
        let mut st = store.create("o1".to_string(), "t1".to_string(), "BUY".to_string(), 100.0);
        st.mark_submitted("ex1".to_string());
        store.upsert(st);

        let barrier = Arc::new(Barrier::new(20));
        let mut handles = Vec::new();
        for i in 0..20 {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                if i % 2 == 0 {
                    // 偶数线程：尝试填满（10×10=100）
                    store.apply_fill_by_exchange_order_id("ex1", 10.0);
                } else {
                    // 奇数线程：尝试 reject
                    store.mark_rejected("o1");
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let st = store.get("o1").unwrap();
        // 不变量：filled_size == requested_size ⟺ status == Filled；
        // 若 status 是 Rejected 但 filled_size 已满，说明 reject 覆盖了 Filled（回归）。
        match st.status {
            OrderStateStatus::Filled => {
                assert_eq!(st.filled_size, 100.0, "Filled 单 filled_size 必须为满额");
            }
            OrderStateStatus::Rejected => {
                assert!(
                    st.filled_size < 100.0,
                    "reject 生效时 filled_size 不应已达满额（否则是 Filled 被倒退）"
                );
            }
            other => panic!("并发竞争后出现非预期状态: {}", other),
        }
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

    /// G4 / P1-RISK-006：原子取消带终态保护，不覆盖 Filled。
    #[test]
    fn test_mark_cancelled_atomic_terminal_protection() {
        let store = OrderStore::new();
        store.create("c1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
        store.set_limit_price("c1", 0.5);
        // 未成交 → 可取消
        let cancelled = store.mark_cancelled("c1").unwrap();
        assert_eq!(cancelled.status, OrderStateStatus::Cancelled);
        // 已 Cancelled → 再取消返回 None（终态不覆盖）
        assert!(store.mark_cancelled("c1").is_none());

        // Filled 不可被取消（不倒退）
        store.create("c2".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
        if let Some(mut st) = store.get("c2") {
            st.update_fill(10.0);
            store.upsert(st);
        }
        assert!(store.mark_cancelled("c2").is_none());
        assert_eq!(
            store.get("c2").map(|s| s.status),
            Some(OrderStateStatus::Filled)
        );
    }

    /// G4 / P1-RISK-006：按 exchange_order_id 原子取消 + set_limit_price。
    #[test]
    fn test_mark_cancelled_by_exchange_order_id() {
        let store = OrderStore::new();
        store.create("c1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
        store.set_limit_price("c1", 0.4);
        if let Some(mut st) = store.get("c1") {
            st.mark_submitted("ex-1".to_string());
            store.upsert(st);
        }
        assert_eq!(store.get("c1").map(|s| s.limit_price), Some(0.4));

        let st = store.mark_cancelled_by_exchange_order_id("ex-1").unwrap();
        assert_eq!(st.status, OrderStateStatus::Cancelled);
        assert!(store.mark_cancelled_by_exchange_order_id("ex-1").is_none());
        assert!(store.mark_cancelled_by_exchange_order_id("").is_none());
    }

    /// G4 / P1-RISK-006：active_orders 只返回非终态订单。
    #[test]
    fn test_active_orders_excludes_terminal() {
        let store = OrderStore::new();
        store.create("a".to_string(), "t".to_string(), "BUY".to_string(), 1.0);
        store.create("b".to_string(), "t".to_string(), "BUY".to_string(), 1.0);
        store.mark_cancelled("b");
        let active = store.active_orders();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].client_order_id, "a");
    }

    /// B2 / P1-009：in_doubt_orders 只返回 Received 且无 exchange id 的订单。
    #[test]
    fn test_in_doubt_orders_selection() {
        let store = OrderStore::new();
        // in-doubt：Received、无 exchange id
        store.create("doubt".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
        // 已提交 → 非 in-doubt
        store.create("ok".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
        if let Some(mut st) = store.get("ok") {
            st.mark_submitted("ex-ok".to_string());
            store.upsert(st);
        }
        // 终态 → 非 in-doubt
        store.create("done".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
        if let Some(mut st) = store.get("done") {
            st.update_fill(10.0);
            store.upsert(st);
        }

        let in_doubt = store.in_doubt_orders();
        assert_eq!(in_doubt.len(), 1);
        assert_eq!(in_doubt[0].client_order_id, "doubt");
        // in-doubt 不进入按 exchange id 的对账集合
        assert!(!store.pending_exchange_order_ids().contains("doubt"));
    }

    /// B2：bind_remote_order 幂等 + 一对一冲突防护 + 绑定后进入对账集合。
    #[test]
    fn test_bind_remote_order_idempotent_and_conflict() {
        let store = OrderStore::new();
        store.create("a".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
        store.create("b".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);

        let bound = store.bind_remote_order("a", "ex-1").expect("首次绑定成功");
        assert_eq!(bound.status, OrderStateStatus::Submitted);
        assert_eq!(bound.exchange_order_id.as_deref(), Some("ex-1"));
        assert!(store.pending_exchange_order_ids().contains("ex-1"));

        // 幂等：再次绑定同一订单 → None（状态不变）
        assert!(store.bind_remote_order("a", "ex-1").is_none());
        // 空 exchange id 拒绝
        assert!(store.bind_remote_order("b", "").is_none());
        assert!(store
            .bind_remote_order("b", "   ")
            .is_none());
        // 一对一：其他订单已持有 ex-1 → 拒绝
        assert!(store.bind_remote_order("b", "ex-1").is_none());
        // 未命中 client_order_id → None
        assert!(store.bind_remote_order("missing", "ex-2").is_none());
        // b 仍未绑定
        assert_eq!(
            store.get("b").map(|s| s.status),
            Some(OrderStateStatus::Received)
        );
    }
}
