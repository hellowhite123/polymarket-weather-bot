//! P0-004：Order Dedup 并发原子性。
//!
//! 验证同一 order_id 并发提交时，原子 `claim` 只允许一个请求占位成功，
//! 杜绝 check-then-act 穿透导致的重复下单。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use polymarket_rust::fast::order_state::OrderStore;

#[test]
fn p0_004_concurrent_claim_only_one_wins() {
    let store = Arc::new(OrderStore::new());
    let claimed = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(16));

    let mut handles = Vec::new();
    for _ in 0..16 {
        let store = Arc::clone(&store);
        let claimed = Arc::clone(&claimed);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            if store
                .claim("p0-004".to_string(), "t1".to_string(), "BUY".to_string(), 10.0)
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
        "并发提交同一 order_id 应只有一次 claim 成功"
    );
}
