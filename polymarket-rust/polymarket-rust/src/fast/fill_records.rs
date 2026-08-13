//! 成交记录账簿 (FillLedger)
//!
//! 统一记录「我的每笔订单」从下单 → 成交 → 结算 的完整生命周期，
//! 补齐成交记录所需的完整字段：
//!
//! | 字段 | 含义 |
//! |------|------|
//! | order_id     | 统一订单 ID（交易所 orderID） |
//! | placed_at    | 下单时间（unix 秒） |
//! | filled_at    | 成交时间（unix 秒） |
//! | settled_at   | 结算时间（unix 秒，结算后才有） |
//! | side         | 方向 BUY / SELL |
//! | token_id     | 市场（token id） |
//! | limit_price  | 下单价（预期价） |
//! | filled_price | 实际成交价 |
//! | size         | 成交数量（shares） |
//! | fee          | 手续费（USDC） |
//! | slippage     | 滑点金额（USDC） |
//! | slippage_pct | 滑点百分比（%） |
//! | pnl          | 盈亏（USDC，结算后才有） |
//!
//! ## 生命周期
//!
//! ```text
//! new() ── 下单，记录 placed_at + limit_price
//!   └─ mark_filled() ── 成交，记录 filled_at + filled_price + fee + slippage
//!       └─ mark_settled() ── 结算，记录 settled_at + pnl
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// 当前 unix 秒
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ============================================================================
// 成交状态
// ============================================================================

/// 成交状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FillStatus {
    /// 已成交，未结算
    Filled,
    /// 已结算
    Settled,
    /// 已取消（未成交）
    Cancelled,
}

impl std::fmt::Display for FillStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FillStatus::Filled => write!(f, "filled"),
            FillStatus::Settled => write!(f, "settled"),
            FillStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

// ============================================================================
// FillRecord
// ============================================================================

/// 单笔成交记录 —— 完整字段
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillRecord {
    /// 统一订单 ID（交易所 orderID）
    pub order_id: String,
    /// 市场 token id
    pub token_id: String,
    /// 方向 BUY / SELL
    pub side: String,
    /// 下单时间（unix 秒）
    pub placed_at: u64,
    /// 成交时间（unix 秒）
    pub filled_at: u64,
    /// 结算时间（unix 秒，结算后才有）
    pub settled_at: Option<u64>,
    /// 下单价（预期价）
    pub limit_price: f64,
    /// 实际成交价
    pub filled_price: f64,
    /// 成交数量（shares）
    pub size: f64,
    /// 手续费（USDC）
    pub fee: f64,
    /// 滑点金额（USDC）= |filled - limit| * size
    pub slippage: f64,
    /// 滑点百分比（%）
    pub slippage_pct: f64,
    /// 盈亏（USDC，结算后才有）
    pub pnl: Option<f64>,
    /// 状态
    pub status: FillStatus,
}

impl FillRecord {
    /// 从下单信息创建（下单时）。
    ///
    /// `placed_at` 与 `filled_at` 初始均为当前时刻；成交详情由 [`mark_filled`]
    /// 后续填充。
    pub fn new(
        order_id: String,
        token_id: String,
        side: String,
        limit_price: f64,
        size: f64,
    ) -> Self {
        let now = now_secs();
        Self {
            order_id,
            token_id,
            side,
            placed_at: now,
            filled_at: now,
            settled_at: None,
            limit_price,
            filled_price: limit_price,
            size,
            fee: 0.0,
            slippage: 0.0,
            slippage_pct: 0.0,
            pnl: None,
            status: FillStatus::Filled,
        }
    }

    /// 记录成交（填充成交价、手续费、滑点）。
    pub fn mark_filled(&mut self, filled_price: f64, fee: f64) {
        self.filled_at = now_secs();
        self.filled_price = filled_price;
        self.fee = fee;
        let (slip_amt, slip_pct) = compute_slippage(self.limit_price, filled_price, self.size);
        self.slippage = slip_amt;
        self.slippage_pct = slip_pct;
        self.status = FillStatus::Filled;
    }

    /// 记录结算（填充结算时间 + 盈亏）。
    pub fn mark_settled(&mut self, pnl: f64) {
        self.settled_at = Some(now_secs());
        self.pnl = Some(pnl);
        self.status = FillStatus::Settled;
    }

    /// 标记取消。
    pub fn mark_cancelled(&mut self) {
        self.status = FillStatus::Cancelled;
    }

    /// 名义价值 = filled_price * size。
    pub fn notional(&self) -> f64 {
        self.filled_price * self.size
    }
}

// ============================================================================
// 滑点计算
// ============================================================================

/// 计算滑点。
///
/// 返回 `(滑点金额, 滑点百分比)`。
/// 滑点定义为「实际成交价相对下单价（预期价）的绝对偏差」，
/// 始终 ≥ 0：0 表示按预期价成交，正值表示价格劣化。
pub fn compute_slippage(limit: f64, filled: f64, size: f64) -> (f64, f64) {
    if limit <= 0.0 {
        return (0.0, 0.0);
    }
    let per_share = (filled - limit).abs();
    let pct = per_share / limit * 100.0;
    (per_share * size, pct)
}

// ============================================================================
// 盈亏计算
// ============================================================================

/// 计算单笔成交的结算盈亏。
///
/// `settlement` 为该 token 的最终结算价（二元市场为 0.0 或 1.0）。
///
/// - `BUY`（做多）：pnl = size × (settlement − filled_price) − fee
/// - `SELL`（做空）：pnl = size × (filled_price − settlement) − fee
pub fn compute_pnl(side: &str, size: f64, filled_price: f64, settlement: f64, fee: f64) -> f64 {
    match side {
        "BUY" => size * (settlement - filled_price) - fee,
        "SELL" => size * (filled_price - settlement) - fee,
        _ => 0.0,
    }
}

// ============================================================================
// FillLedger
// ============================================================================

/// 成交记录账簿。
///
/// 线程安全（内部 `RwLock<HashMap>`），可被 executor / service 共享。
#[derive(Debug, Default)]
pub struct FillLedger {
    records: RwLock<HashMap<String, FillRecord>>,
}

impl FillLedger {
    pub fn new() -> Self {
        Self {
            records: RwLock::new(HashMap::new()),
        }
    }

    /// 记录一笔成交（若 order_id 已存在则覆盖）。
    pub fn record(&self, rec: FillRecord) {
        self.records.write().unwrap().insert(rec.order_id.clone(), rec);
    }

    /// 获取单笔成交记录。
    pub fn get(&self, order_id: &str) -> Option<FillRecord> {
        self.records.read().unwrap().get(order_id).cloned()
    }

    /// 结算：更新指定订单的盈亏（返回是否命中）。
    pub fn settle(&self, order_id: &str, pnl: f64) -> bool {
        let mut records = self.records.write().unwrap();
        if let Some(rec) = records.get_mut(order_id) {
            rec.mark_settled(pnl);
            true
        } else {
            false
        }
    }

    /// 按 token 批量结算：给定某市场的结算结果（outcome 0.0 或 1.0），
    /// 自动结算该市场下所有未结算的成交记录，回填 `settled_at` 与 `pnl`。
    ///
    /// 返回结算的记录数。
    pub fn settle_by_token(&self, token_id: &str, outcome: f64) -> usize {
        let mut records = self.records.write().unwrap();
        let mut count = 0;
        for rec in records.values_mut() {
            if rec.token_id == token_id && rec.status == FillStatus::Filled {
                let pnl = compute_pnl(&rec.side, rec.size, rec.filled_price, outcome, rec.fee);
                rec.mark_settled(pnl);
                count += 1;
            }
        }
        count
    }

    /// 取消：标记指定订单为已取消。
    pub fn cancel(&self, order_id: &str) -> bool {
        let mut records = self.records.write().unwrap();
        if let Some(rec) = records.get_mut(order_id) {
            rec.mark_cancelled();
            true
        } else {
            false
        }
    }

    /// 列出全部成交记录（按下单时间升序）。
    pub fn list(&self) -> Vec<FillRecord> {
        let mut v: Vec<FillRecord> = self.records.read().unwrap().values().cloned().collect();
        v.sort_by_key(|r| r.placed_at);
        v
    }

    /// 记录总数。
    pub fn len(&self) -> usize {
        self.records.read().unwrap().len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.records.read().unwrap().is_empty()
    }

    /// 序列化为 JSON（供 gRPC / 文件持久化）。
    pub fn to_json(&self) -> String {
        let list = self.list();
        serde_json::to_string_pretty(&list).unwrap_or_else(|_| "[]".to_string())
    }

    /// 汇总统计。
    pub fn stats(&self) -> FillStats {
        let list = self.list();
        let total = list.len();
        let settled = list.iter().filter(|r| r.status == FillStatus::Settled).count();
        let total_fee: f64 = list.iter().map(|r| r.fee).sum();
        let total_slippage: f64 = list.iter().map(|r| r.slippage).sum();
        let total_pnl: f64 = list.iter().filter_map(|r| r.pnl).sum();
        let avg_slippage_pct = if total > 0 {
            list.iter().map(|r| r.slippage_pct).sum::<f64>() / total as f64
        } else {
            0.0
        };
        FillStats {
            total,
            settled,
            total_fee,
            total_slippage,
            total_pnl,
            avg_slippage_pct,
        }
    }
}

/// 成交账簿统计。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FillStats {
    pub total: usize,
    pub settled: usize,
    pub total_fee: f64,
    pub total_slippage: f64,
    pub total_pnl: f64,
    pub avg_slippage_pct: f64,
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_slippage_zero_when_same_price() {
        let (amt, pct) = compute_slippage(0.50, 0.50, 10.0);
        assert_eq!(amt, 0.0);
        assert_eq!(pct, 0.0);
    }

    #[test]
    fn test_compute_slippage_positive() {
        // 下单价 0.50，成交价 0.55，数量 10 → 每股偏差 0.05，10% 偏差
        let (amt, pct) = compute_slippage(0.50, 0.55, 10.0);
        assert!((amt - 0.5).abs() < 1e-9, "amt={}", amt);
        assert!((pct - 10.0).abs() < 1e-9, "pct={}", pct);
    }

    #[test]
    fn test_compute_slippage_negative_improvement() {
        // 下单价 0.50，成交价 0.45（更优），数量 10 → 仍是 10% 偏差幅度
        let (amt, pct) = compute_slippage(0.50, 0.45, 10.0);
        assert!((amt - 0.5).abs() < 1e-9);
        assert!((pct - 10.0).abs() < 1e-9);
    }

    #[test]
    fn test_compute_slippage_zero_limit() {
        let (amt, pct) = compute_slippage(0.0, 0.5, 10.0);
        assert_eq!(amt, 0.0);
        assert_eq!(pct, 0.0);
    }

    #[test]
    fn test_fill_record_lifecycle() {
        let mut rec = FillRecord::new(
            "ord-1".to_string(),
            "token-1".to_string(),
            "BUY".to_string(),
            0.50,
            10.0,
        );
        assert_eq!(rec.status, FillStatus::Filled);
        assert_eq!(rec.limit_price, 0.50);
        assert!(rec.pnl.is_none());
        assert!(rec.settled_at.is_none());

        rec.mark_filled(0.55, 0.0);
        assert!((rec.filled_price - 0.55).abs() < 1e-9);
        assert!((rec.slippage - 0.5).abs() < 1e-9);
        assert!((rec.slippage_pct - 10.0).abs() < 1e-9);

        rec.mark_settled(0.45);
        assert!(rec.settled_at.is_some());
        assert!((rec.pnl.unwrap() - 0.45).abs() < 1e-9);
        assert_eq!(rec.status, FillStatus::Settled);
    }

    #[test]
    fn test_fill_record_notional() {
        let rec = FillRecord::new(
            "ord-2".to_string(),
            "token-2".to_string(),
            "SELL".to_string(),
            0.40,
            25.0,
        );
        assert!((rec.notional() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn test_ledger_record_and_list() {
        let ledger = FillLedger::new();
        assert!(ledger.is_empty());

        ledger.record(FillRecord::new(
            "ord-a".to_string(),
            "t1".to_string(),
            "BUY".to_string(),
            0.5,
            10.0,
        ));
        ledger.record(FillRecord::new(
            "ord-b".to_string(),
            "t2".to_string(),
            "SELL".to_string(),
            0.6,
            5.0,
        ));
        assert_eq!(ledger.len(), 2);
        assert_eq!(ledger.list().len(), 2);
        assert!(ledger.get("ord-a").is_some());
        assert!(ledger.get("missing").is_none());
    }

    #[test]
    fn test_ledger_settle_and_cancel() {
        let ledger = FillLedger::new();
        ledger.record(FillRecord::new(
            "ord-s".to_string(),
            "t1".to_string(),
            "BUY".to_string(),
            0.5,
            10.0,
        ));

        assert!(ledger.settle("ord-s", 3.0));
        let rec = ledger.get("ord-s").unwrap();
        assert_eq!(rec.status, FillStatus::Settled);
        assert!(rec.pnl.is_some());

        ledger.record(FillRecord::new(
            "ord-c".to_string(),
            "t2".to_string(),
            "SELL".to_string(),
            0.5,
            10.0,
        ));
        assert!(ledger.cancel("ord-c"));
        assert_eq!(ledger.get("ord-c").unwrap().status, FillStatus::Cancelled);
        assert!(!ledger.settle("missing", 1.0));
    }

    #[test]
    fn test_ledger_stats() {
        let ledger = FillLedger::new();
        let mut r1 = FillRecord::new("o1".to_string(), "t1".to_string(), "BUY".to_string(), 0.5, 10.0);
        r1.mark_filled(0.5, 1.0);
        r1.mark_settled(2.0);
        ledger.record(r1);

        let mut r2 = FillRecord::new("o2".to_string(), "t2".to_string(), "SELL".to_string(), 0.5, 10.0);
        r2.mark_filled(0.5, 1.0);
        ledger.record(r2);

        let stats = ledger.stats();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.settled, 1);
        assert!((stats.total_fee - 2.0).abs() < 1e-9);
        assert!((stats.total_pnl - 2.0).abs() < 1e-9);
    }

    #[test]
    fn test_ledger_to_json() {
        let ledger = FillLedger::new();
        ledger.record(FillRecord::new("o1".to_string(), "t1".to_string(), "BUY".to_string(), 0.5, 10.0));
        let json = ledger.to_json();
        assert!(json.contains("o1"));
        assert!(json.contains("token"));
    }

    #[test]
    fn test_compute_pnl_buy_win() {
        // 做多 10 股 @ 0.5，结算 1.0，fee 0 → 盈利 5.0
        let pnl = compute_pnl("BUY", 10.0, 0.5, 1.0, 0.0);
        assert!((pnl - 5.0).abs() < 1e-9, "pnl={}", pnl);
    }

    #[test]
    fn test_compute_pnl_buy_loss() {
        // 做多 10 股 @ 0.5，结算 0.0，fee 0 → 亏损 -5.0
        let pnl = compute_pnl("BUY", 10.0, 0.5, 0.0, 0.0);
        assert!((pnl + 5.0).abs() < 1e-9, "pnl={}", pnl);
    }

    #[test]
    fn test_compute_pnl_sell() {
        // 做空 10 股 @ 0.5，结算 1.0（做空方向看错）→ 亏损 -5.0
        let pnl = compute_pnl("SELL", 10.0, 0.5, 1.0, 0.0);
        assert!((pnl + 5.0).abs() < 1e-9, "pnl={}", pnl);
        // 做空 10 股 @ 0.5，结算 0.0（做空方向看对）→ 盈利 5.0
        let pnl2 = compute_pnl("SELL", 10.0, 0.5, 0.0, 0.0);
        assert!((pnl2 - 5.0).abs() < 1e-9, "pnl={}", pnl2);
    }

    #[test]
    fn test_compute_pnl_with_fee() {
        // 做多 10 股 @ 0.5，结算 1.0，fee 1.0 → 盈利 5.0 - 1.0 = 4.0
        let pnl = compute_pnl("BUY", 10.0, 0.5, 1.0, 1.0);
        assert!((pnl - 4.0).abs() < 1e-9, "pnl={}", pnl);
    }

    #[test]
    fn test_ledger_settle_by_token() {
        let ledger = FillLedger::new();
        // 两条同市场记录 + 一条其他市场
        let mut r1 = FillRecord::new("o1".to_string(), "t1".to_string(), "BUY".to_string(), 0.5, 10.0);
        r1.mark_filled(0.5, 0.0);
        ledger.record(r1);

        let mut r2 = FillRecord::new("o2".to_string(), "t1".to_string(), "SELL".to_string(), 0.4, 20.0);
        r2.mark_filled(0.4, 0.0);
        ledger.record(r2);

        let mut r3 = FillRecord::new("o3".to_string(), "t2".to_string(), "BUY".to_string(), 0.6, 5.0);
        r3.mark_filled(0.6, 0.0);
        ledger.record(r3);

        // 结算 t1（outcome=1.0）
        let n = ledger.settle_by_token("t1", 1.0);
        assert_eq!(n, 2, "应结算 2 条 t1 记录");

        let o1 = ledger.get("o1").unwrap();
        assert_eq!(o1.status, FillStatus::Settled);
        assert!(o1.settled_at.is_some());
        // BUY 10 @ 0.5，结算 1.0 → +5.0
        assert!((o1.pnl.unwrap() - 5.0).abs() < 1e-9);

        let o2 = ledger.get("o2").unwrap();
        assert_eq!(o2.status, FillStatus::Settled);
        // SELL 20 @ 0.4，结算 1.0 → 20*(0.4-1.0) = -12.0
        assert!((o2.pnl.unwrap() + 12.0).abs() < 1e-9);

        // t2 不应被结算
        let o3 = ledger.get("o3").unwrap();
        assert_eq!(o3.status, FillStatus::Filled);
        assert!(o3.pnl.is_none());
    }
}
