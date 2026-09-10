# P1-010 代码审查 diff（转 GPT 用）

> 用途：提交给 GPT / Gemini 交叉审查。
> 变更主题：`Order Dedup claim 占位失败清理`（P0-004 遗留）。
> 状态：代码已实现，`cargo test` 263 全绿；**尚未 commit**，待 Human review。

## 审查重点（请 GPT 逐项回答）

1. **幂等语义**：`claim_or_retry` 把「查状态 + 插入/重置」放进单次写锁，是否真正消除了原 `get + claim` 两步的非原子竞态（P0-004 并发穿透）？
2. **重试终态范围**：`Rejected/Expired/Cancelled` 允许重新占位下单，是否正确？有没有本不该重试却被放行、或本该重试却被拦截的终态？
3. **失败路径覆盖**：`service.rs` 补的 6 处 `mark_rejected` 是否覆盖了 `claim()` 占位之后的**全部**失败出口？有没有遗漏的 `return` 路径（会残留 `Received` 幽灵 pending）？
4. **竞态回归**：`claim_or_retry` 在写锁内调 `OrderState::new`，`mark_rejected` 用 `get`（读锁）+ `upsert`（写锁）两步——两者之间是否有 TOCTOU 风险？
5. **`existing_order_result` 兜底**：`claim_or_retry` 返回 `None` 后取 `get` 再返回已有状态，如果此时状态已被并发线程改成别的值，返回是否仍一致？

---

## 变更 1/2：`polymarket-rust/polymarket-rust/src/fast/order_state.rs`

```diff
diff --git a/polymarket-rust/polymarket-rust/src/fast/order_state.rs b/polymarket-rust/polymarket-rust/src/fast/order_state.rs
index 34dc04e..b97a4cf 100644
--- a/polymarket-rust/polymarket-rust/src/fast/order_state.rs
+++ b/polymarket-rust/polymarket-rust/src/fast/order_state.rs
@@ -201,6 +201,42 @@ impl OrderStore {
         Some(state)
     }
 
+    /// 标记拒绝（若存在）。用于下单失败路径清理占位，避免残留 `Received`
+    /// 状态被 `pending_order_ids` 误报，或被幂等命中拦下导致无法重试。
+    pub fn mark_rejected(&self, client_order_id: &str) {
+        if let Some(mut st) = self.get(client_order_id) {
+            st.mark_rejected();
+            self.upsert(st);
+        }
+    }
+
+    /// 幂等占位（支持重试）：`client_order_id` 不存在、或处于可重试终态
+    /// （`Rejected` / `Expired` / `Cancelled`）时，创建/重置为 `Received` 并返回状态；
+    /// 处于进行中或已成交则返回 `None`（拒绝重复下单）。
+    ///
+    /// 单次写锁内完成「查状态 + 插入/重置」，保证并发原子性（P0-004）。
+    pub fn claim_or_retry(
+        &self,
+        client_order_id: String,
+        token_id: String,
+        side: String,
+        requested_size: f64,
+    ) -> Option<OrderState> {
+        let mut orders = self.orders.write().unwrap();
+        match orders.get(&client_order_id).map(|s| s.status) {
+            None
+            | Some(OrderStateStatus::Rejected)
+            | Some(OrderStateStatus::Expired)
+            | Some(OrderStateStatus::Cancelled) => {
+                let state =
+                    OrderState::new(client_order_id.clone(), token_id, side, requested_size);
+                orders.insert(client_order_id, state.clone());
+                Some(state)
+            }
+            _ => None,
+        }
+    }
+
     /// 订单总数。
     pub fn len(&self) -> usize {
         self.orders.read().unwrap().len()
@@ -388,4 +424,57 @@ mod tests {
             "并发提交同一 order_id 应只有一次 claim 成功（原子占位）"
         );
     }
+
+    /// claim_or_retry：下单失败标记 rejected 后，同 order_id 可重新占位（重试）。
+    #[test]
+    fn test_claim_retry_after_rejected() {
+        let store = OrderStore::new();
+
+        // 首次占位成功
+        assert!(store
+            .claim_or_retry("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0)
+            .is_some());
+
+        // 进行中：重复 claim_or_retry 返回 None（幂等去重）
+        assert!(store
+            .claim_or_retry("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0)
+            .is_none());
+
+        // 下单失败 → 标记 rejected
+        store.mark_rejected("o1");
+        assert_eq!(
+            store.get("o1").map(|s| s.status),
+            Some(OrderStateStatus::Rejected)
+        );
+
+        // rejected 后可重试：claim_or_retry 重新占位成功
+        assert!(store
+            .claim_or_retry("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0)
+            .is_some());
+    }
+
+    /// 失败路径 mark_rejected 后，订单不再进入 pending（避免对账误报）。
+    #[test]
+    fn test_mark_rejected_clears_pending() {
+        let store = OrderStore::new();
+        store.create("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
+        assert!(store.pending_order_ids().contains("o1"));
+
+        store.mark_rejected("o1");
+        assert!(!store.pending_order_ids().contains("o1"));
+    }
+
+    /// claim_or_retry 对已成交订单去重（不可重复下单）。
+    #[test]
+    fn test_claim_or_retry_rejects_filled() {
+        let store = OrderStore::new();
+        store.create("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0);
+        if let Some(mut st) = store.get("o1") {
+            st.update_fill(10.0); // Filled
+            store.upsert(st);
+        }
+        assert!(store
+            .claim_or_retry("o1".to_string(), "t1".to_string(), "BUY".to_string(), 10.0)
+            .is_none());
+    }
 }
```

---

## 变更 2/2：`polymarket-rust/polymarket-rust/src/grpc/service.rs`

```diff
diff --git a/polymarket-rust/polymarket-rust/src/grpc/service.rs b/polymarket-rust/polymarket-rust/src/grpc/service.rs
index 3aa2526..4a73513 100644
--- a/polymarket-rust/polymarket-rust/src/grpc/service.rs
+++ b/polymarket-rust/polymarket-rust/src/grpc/service.rs
@@ -406,20 +406,12 @@ impl pb::execution_service_server::ExecutionService for ExecutionServiceImpl {
         // 方向转换（提前到幂等检查之前，供原子占位使用）
         let side = direction_to_side(order.direction)?;
 
-        // ★ P0-004 幂等：先查已有，再原子 claim 占位，并发下只允许一个请求下单
+        // ★ P0-004 幂等：原子 claim_or_retry 占位，并发下只允许一个请求下单；
+        // 可重试终态（Rejected/Expired/Cancelled）会被重置并重新下单。
         if !order.order_id.is_empty() {
-            if let Some(existing) = self.order_store.get(&order.order_id) {
-                tracing::info!(
-                    "order_id={} 已存在（幂等去重），返回已有状态 {}",
-                    order.order_id,
-                    existing.status
-                );
-                return Ok(Response::new(existing_order_result(&existing)));
-            }
-            // 原子占位：首次返回 Some；并发穿透者返回 None（已被抢先占位）
             if self
                 .order_store
-                .claim(
+                .claim_or_retry(
                     order.order_id.clone(),
                     order.token_id.clone(),
                     side.as_str().to_string(),
@@ -427,8 +419,13 @@ impl pb::execution_service_server::ExecutionService for ExecutionServiceImpl {
                 )
                 .is_none()
             {
+                // 进行中或已成交：幂等去重，返回已有状态
                 if let Some(existing) = self.order_store.get(&order.order_id) {
-                    tracing::info!("order_id={} 并发去重命中，返回已有状态", order.order_id);
+                    tracing::info!(
+                        "order_id={} 幂等去重（进行中/已成交），返回已有状态 {}",
+                        order.order_id,
+                        existing.status
+                    );
                     return Ok(Response::new(existing_order_result(&existing)));
                 }
             }
@@ -436,6 +433,7 @@ impl pb::execution_service_server::ExecutionService for ExecutionServiceImpl {
 
         // 只执行 OPEN 状态的指令
         if order.status != pb::OrderStatus::Open as i32 {
+            self.order_store.mark_rejected(&order.order_id);
             return Ok(Response::new(pb::OrderResult {
                 order_id: order.order_id.clone(),
                 success: false,
@@ -498,6 +496,7 @@ impl pb::execution_service_server::ExecutionService for ExecutionServiceImpl {
         {
             let rs = self.risk_state.lock().await;
             if rs.daily_circuit_breaker || rs.manual_halt {
+                self.order_store.mark_rejected(&order.order_id);
                 return Ok(Response::new(pb::OrderResult {
                     order_id: order.order_id.clone(),
                     success: false,
@@ -522,6 +521,7 @@ impl pb::execution_service_server::ExecutionService for ExecutionServiceImpl {
                     order.token_id
                 );
 
+                self.order_store.mark_rejected(&order.order_id);
                 return Ok(Response::new(pb::OrderResult {
                     order_id: order.order_id.clone(),
                     success: false,
@@ -541,6 +541,7 @@ impl pb::execution_service_server::ExecutionService for ExecutionServiceImpl {
         if let Err(e) = re.pre_check(&order.token_id, side, notional, order.quantity, 0) {
             tracing::warn!("风控预检拦截: token={}, {}", order.token_id, e);
 
+            self.order_store.mark_rejected(&order.order_id);
             return Ok(Response::new(pb::OrderResult {
                 order_id: order.order_id.clone(),
                 success: false,
@@ -657,6 +658,7 @@ impl pb::execution_service_server::ExecutionService for ExecutionServiceImpl {
                 }
                 Err(e) => {
                     tracing::error!("fast path 下单失败: {}", e);
+                    self.order_store.mark_rejected(&order.order_id);
                     return Ok(Response::new(pb::OrderResult {
                         order_id: order.order_id.clone(),
                         success: false,
@@ -677,6 +679,7 @@ impl pb::execution_service_server::ExecutionService for ExecutionServiceImpl {
             order.order_id
         );
 
+        self.order_store.mark_rejected(&order.order_id);
         return Ok(Response::new(pb::OrderResult {
             order_id: order.order_id.clone(),
             success: false,
@@ -1031,4 +1034,40 @@ mod tests {
         let rs = service.risk_state.lock().await;
         assert_eq!(rs.positions.get("token-1").copied().unwrap_or(0.0), 10.0);
     }
+
+    /// P1-010：下单失败路径（RiskEngine 未初始化）会把 claim 占位标记为 Rejected，
+    /// 之后同 order_id 可重试且不进入 pending 对账。
+    #[tokio::test]
+    async fn test_failed_place_order_marks_rejected() {
+        use crate::fast::order_state::OrderStateStatus;
+        use crate::fast::trading_gate::TradingGateState;
+        use pb::execution_service_server::ExecutionService;
+
+        let mut service = ExecutionServiceImpl::new_mock();
+        service.trading_gate.set_state(TradingGateState::Ready);
+        // 不挂载 risk_engine → 走「RiskEngine 未初始化」失败路径
+
+        let req = pb::AggregatedOrder {
+            order_id: "o-fail".to_string(),
+            token_id: "t1".to_string(),
+            direction: pb::Direction::BuyYes as i32,
+            quantity: 10.0,
+            status: pb::OrderStatus::Open as i32,
+            target_price: 0.5,
+            paper_trading: false,
+            ..Default::default()
+        };
+
+        let resp = service.place_order(Request::new(req)).await.unwrap();
+        let result = resp.into_inner();
+
+        assert_eq!(result.status, pb::ExecutionStatus::Rejected as i32);
+        // 占位已被标记 rejected（不再残留 Received）
+        assert_eq!(
+            service.order_store.get("o-fail").map(|s| s.status),
+            Some(OrderStateStatus::Rejected)
+        );
+        // 且不再进入 pending（对账不误报）
+        assert!(!service.order_store.pending_order_ids().contains("o-fail"));
+    }
 }
```
