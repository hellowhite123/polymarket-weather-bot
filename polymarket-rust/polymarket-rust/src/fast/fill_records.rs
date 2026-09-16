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
use std::collections::{HashMap, HashSet};
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
    /// 成交唯一标识（trade id，fill 级 identity，P0-015 幂等 key）
    pub trade_id: String,
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
            trade_id: String::new(),
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
// OrderFill —— 订单级成交聚合（P1-029）
// ============================================================================

/// 订单级成交聚合。
///
/// 一个订单可能被多笔 partial fill 成交（每笔有不同的成交价/数量/手续费），
/// `FillRecord` 是「单笔成交」记录，`OrderFill` 则是「订单」维度的累计汇总，
/// 供订单级 API（`get` / `settle` / `cancel`）使用；每笔 fill 明细存于
/// `FillLedger.fills`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderFill {
    /// 统一订单 ID（交易所 orderID）
    pub order_id: String,
    /// 市场 token id
    pub token_id: String,
    /// 方向 BUY / SELL
    pub side: String,
    /// 下单时间（unix 秒）
    pub placed_at: u64,
    /// 下单价（预期价）
    pub limit_price: f64,
    /// 累计成交量
    pub total_size: f64,
    /// 加权平均成交价（按 size 加权）
    pub avg_filled_price: f64,
    /// 累计手续费（USDC）
    pub total_fee: f64,
    /// 累计滑点金额（USDC）
    pub total_slippage: f64,
    /// 累计滑点百分比（%）
    pub total_slippage_pct: f64,
    /// 状态
    pub status: FillStatus,
    /// 结算时间（unix 秒，结算后才有）
    pub settled_at: Option<u64>,
    /// 盈亏（USDC，结算后才有）
    pub pnl: Option<f64>,
    /// 已成交的 fill 笔数
    pub fill_count: usize,
}

impl OrderFill {
    /// 从首笔 fill 建立聚合。
    fn from_fill(rec: &FillRecord) -> Self {
        Self {
            order_id: rec.order_id.clone(),
            token_id: rec.token_id.clone(),
            side: rec.side.clone(),
            placed_at: rec.placed_at,
            limit_price: rec.limit_price,
            total_size: rec.size,
            avg_filled_price: rec.filled_price,
            total_fee: rec.fee,
            total_slippage: rec.slippage,
            total_slippage_pct: rec.slippage_pct,
            status: rec.status,
            settled_at: rec.settled_at,
            pnl: rec.pnl,
            fill_count: 1,
        }
    }

    /// 累加一笔 fill（累计 size / 加权平均价 / 累计 fee / 累计滑点）。
    fn accumulate(&mut self, rec: &FillRecord) {
        let new_total = self.total_size + rec.size;
        if new_total > 0.0 {
            self.avg_filled_price = (self.avg_filled_price * self.total_size
                + rec.filled_price * rec.size)
                / new_total;
        }
        self.total_size = new_total;
        self.total_fee += rec.fee;
        self.total_slippage += rec.slippage;
        self.total_slippage_pct += rec.slippage_pct;
        self.fill_count += 1;
        // 状态随最新 fill 推进（Cancelled 优先保持终态，不倒退）
        if self.status != FillStatus::Cancelled && rec.status != FillStatus::Cancelled {
            self.status = rec.status;
        }
    }

    /// 标记结算。
    fn mark_settled(&mut self, pnl: f64) {
        self.settled_at = Some(now_secs());
        self.pnl = Some(pnl);
        self.status = FillStatus::Settled;
    }

    /// 标记取消。
    fn mark_cancelled(&mut self) {
        self.status = FillStatus::Cancelled;
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
///
/// ## 双索引结构（P1-029）
///
/// - `fills`：fill 级历史，每笔成交独立（key = trade_id，空则回退 order_id），
///   供 `list` / `stats` / `to_json` 输出「每笔成交」明细，不再互相覆盖。
/// - `orders`：订单级聚合，`order_id → OrderFill`，累计该订单所有 fill 的
///   成交量/加权平均价/手续费/滑点，供订单级 API（`get` / `settle` / `cancel`）使用。
#[derive(Debug, Default)]
pub struct FillLedger {
    /// fill 级历史：fill key → 单笔成交记录（P1-029）
    fills: RwLock<HashMap<String, FillRecord>>,
    /// 订单级聚合：order_id → 订单成交汇总（P1-029）
    orders: RwLock<HashMap<String, OrderFill>>,
    /// fill 幂等去重集合（trade_id），P0-015
    seen_trade_ids: RwLock<HashSet<String>>,
}

impl FillLedger {
    pub fn new() -> Self {
        Self {
            fills: RwLock::new(HashMap::new()),
            orders: RwLock::new(HashMap::new()),
            seen_trade_ids: RwLock::new(HashSet::new()),
        }
    }

    /// P0-015：fill 幂等去重。首次见到该 `trade_id` 返回 `true`（并登记），
    /// 已处理过返回 `false`（调用方应忽略）。空 `trade_id` 不启用去重（保持旧行为）。
    pub fn claim_trade(&self, trade_id: &str) -> bool {
        if trade_id.is_empty() {
            return true;
        }
        let mut seen = self.seen_trade_ids.write().unwrap();
        seen.insert(trade_id.to_string())
    }

    /// 记录一笔成交。
    ///
    /// P1-029：不再以 `order_id` 覆盖式存储——而是：
    /// 1. 把该笔 fill 写入 `fills`（key = trade_id 非空 ? trade_id : order_id），
    ///    同订单多笔 fill 各自独立，不互相覆盖；
    /// 2. 聚合到 `orders[order_id]`（累计 size / 加权平均价 / fee / 滑点）。
    pub fn record(&self, rec: FillRecord) {
        let fill_key = if rec.trade_id.is_empty() {
            rec.order_id.clone()
        } else {
            rec.trade_id.clone()
        };
        self.fills.write().unwrap().insert(fill_key, rec.clone());

        let mut orders = self.orders.write().unwrap();
        match orders.get_mut(&rec.order_id) {
            Some(o) => o.accumulate(&rec),
            None => {
                orders.insert(rec.order_id.clone(), OrderFill::from_fill(&rec));
            }
        }
    }

    /// 获取订单级成交聚合。
    pub fn get(&self, order_id: &str) -> Option<OrderFill> {
        self.orders.read().unwrap().get(order_id).cloned()
    }

    /// 结算：更新指定订单的盈亏（返回是否命中）。
    ///
    /// 订单级操作：更新 `orders[order_id]` 的结算状态与盈亏，并回写该订单
    /// 所有 fill 的状态为 `Settled`（fill 的 `pnl` 保持 `None`，订单盈亏在聚合层）。
    pub fn settle(&self, order_id: &str, pnl: f64) -> bool {
        let mut orders = self.orders.write().unwrap();
        let hit = match orders.get_mut(order_id) {
            Some(o) => {
                o.mark_settled(pnl);
                true
            }
            None => false,
        };
        drop(orders);
        if hit {
            let mut fills = self.fills.write().unwrap();
            for rec in fills.values_mut() {
                if rec.order_id == order_id {
                    rec.status = FillStatus::Settled;
                    rec.settled_at = Some(now_secs());
                }
            }
        }
        hit
    }

    /// 按 token 批量结算：给定某市场的结算结果（outcome 0.0 或 1.0），
    /// 自动结算该市场下所有未结算的成交记录，回填 `settled_at` 与 `pnl`。
    ///
    /// P1-029：遍历 fill 级历史，每笔 fill 独立计算盈亏并回填（成交价不同，
    /// 盈亏分别计算更精确），再聚合到订单级。返回结算的 fill 笔数。
    pub fn settle_by_token(&self, token_id: &str, outcome: f64) -> usize {
        let mut fills = self.fills.write().unwrap();
        let mut order_pnl: HashMap<String, f64> = HashMap::new();
        let mut count = 0;
        for rec in fills.values_mut() {
            if rec.token_id == token_id && rec.status == FillStatus::Filled {
                let pnl = compute_pnl(&rec.side, rec.size, rec.filled_price, outcome, rec.fee);
                rec.mark_settled(pnl);
                *order_pnl.entry(rec.order_id.clone()).or_insert(0.0) += pnl;
                count += 1;
            }
        }
        drop(fills);

        let mut orders = self.orders.write().unwrap();
        for (order_id, pnl) in order_pnl {
            if let Some(o) = orders.get_mut(&order_id) {
                o.mark_settled(pnl);
            }
        }
        count
    }

    /// 取消：标记指定订单为已取消（返回是否命中）。
    ///
    /// 订单级操作：更新 `orders[order_id]`，并回写该订单所有 fill 的状态为 `Cancelled`。
    pub fn cancel(&self, order_id: &str) -> bool {
        let mut orders = self.orders.write().unwrap();
        let hit = match orders.get_mut(order_id) {
            Some(o) => {
                o.mark_cancelled();
                true
            }
            None => false,
        };
        drop(orders);
        if hit {
            let mut fills = self.fills.write().unwrap();
            for rec in fills.values_mut() {
                if rec.order_id == order_id {
                    rec.status = FillStatus::Cancelled;
                }
            }
        }
        hit
    }

    /// 列出全部成交记录（fill 级，每笔成交一条，按下单时间升序）。
    ///
    /// P1-029：返回 fill 级历史，同订单多笔 fill 各自独立，不再互相覆盖。
    pub fn list(&self) -> Vec<FillRecord> {
        let mut v: Vec<FillRecord> = self.fills.read().unwrap().values().cloned().collect();
        v.sort_by_key(|r| r.placed_at);
        v
    }

    /// 成交笔数（fill 级）。
    pub fn len(&self) -> usize {
        self.fills.read().unwrap().len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.fills.read().unwrap().is_empty()
    }

    /// 序列化为 JSON（供 gRPC / 文件持久化）。
    pub fn to_json(&self) -> String {
        let list = self.list();
        serde_json::to_string_pretty(&list).unwrap_or_else(|_| "[]".to_string())
    }

    /// 汇总统计。
    ///
    /// P1-029：`total`/`total_fee`/`total_slippage`/`avg_slippage_pct` 基于 fill 级历史；
    /// `settled`/`total_pnl` 基于订单级聚合（盈亏是订单维度的）。
    pub fn stats(&self) -> FillStats {
        let fills = self.fills.read().unwrap();
        let orders = self.orders.read().unwrap();
        let list: Vec<FillRecord> = fills.values().cloned().collect();
        let total = list.len();
        let settled = orders
            .values()
            .filter(|o| o.status == FillStatus::Settled)
            .count();
        let total_fee: f64 = list.iter().map(|r| r.fee).sum();
        let total_slippage: f64 = list.iter().map(|r| r.slippage).sum();
        let total_pnl: f64 = orders.values().filter_map(|o| o.pnl).sum();
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
        let mut r1 = FillRecord::new(
            "o1".to_string(),
            "t1".to_string(),
            "BUY".to_string(),
            0.5,
            10.0,
        );
        r1.mark_filled(0.5, 1.0);
        r1.mark_settled(2.0);
        ledger.record(r1);

        let mut r2 = FillRecord::new(
            "o2".to_string(),
            "t2".to_string(),
            "SELL".to_string(),
            0.5,
            10.0,
        );
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
        ledger.record(FillRecord::new(
            "o1".to_string(),
            "t1".to_string(),
            "BUY".to_string(),
            0.5,
            10.0,
        ));
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
        let mut r1 = FillRecord::new(
            "o1".to_string(),
            "t1".to_string(),
            "BUY".to_string(),
            0.5,
            10.0,
        );
        r1.mark_filled(0.5, 0.0);
        ledger.record(r1);

        let mut r2 = FillRecord::new(
            "o2".to_string(),
            "t1".to_string(),
            "SELL".to_string(),
            0.4,
            20.0,
        );
        r2.mark_filled(0.4, 0.0);
        ledger.record(r2);

        let mut r3 = FillRecord::new(
            "o3".to_string(),
            "t2".to_string(),
            "BUY".to_string(),
            0.6,
            5.0,
        );
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

    #[test]
    fn test_claim_trade_dedup() {
        let ledger = FillLedger::new();
        assert!(ledger.claim_trade("trade-1")); // 首次
        assert!(!ledger.claim_trade("trade-1")); // 重复
        assert!(ledger.claim_trade("trade-2")); // 不同 trade
    }

    #[test]
    fn test_claim_trade_empty_always_true() {
        let ledger = FillLedger::new();
        assert!(ledger.claim_trade(""));
        assert!(ledger.claim_trade(""));
    }

    /// P1-029：同订单多笔 fill 不互相覆盖——fill 级历史 3 条独立，订单级聚合累计。
    #[test]
    fn test_ledger_multi_fill_not_overwritten() {
        let ledger = FillLedger::new();

        let mut f1 = FillRecord::new("ord-x".to_string(), "t1".to_string(), "BUY".to_string(), 0.5, 30.0);
        f1.trade_id = "trade-1".to_string();
        f1.mark_filled(0.50, 0.1);
        ledger.record(f1);

        let mut f2 = FillRecord::new("ord-x".to_string(), "t1".to_string(), "BUY".to_string(), 0.5, 20.0);
        f2.trade_id = "trade-2".to_string();
        f2.mark_filled(0.55, 0.1);
        ledger.record(f2);

        let mut f3 = FillRecord::new("ord-x".to_string(), "t1".to_string(), "BUY".to_string(), 0.5, 50.0);
        f3.trade_id = "trade-3".to_string();
        f3.mark_filled(0.45, 0.1);
        ledger.record(f3);

        // fill 级历史：3 笔独立，不覆盖
        assert_eq!(ledger.len(), 3, "同订单 3 笔 fill 应各自独立，不覆盖");
        assert_eq!(ledger.list().len(), 3);

        // 订单级聚合
        let agg = ledger.get("ord-x").unwrap();
        assert_eq!(agg.fill_count, 3);
        assert!((agg.total_size - 100.0).abs() < 1e-9, "累计成交量应为 100");
        // 加权平均价 = (0.50*30 + 0.55*20 + 0.45*50) / 100
        let expected_avg = (0.50 * 30.0 + 0.55 * 20.0 + 0.45 * 50.0) / 100.0;
        assert!(
            (agg.avg_filled_price - expected_avg).abs() < 1e-9,
            "avg={} expected={}",
            agg.avg_filled_price,
            expected_avg
        );
        assert!((agg.total_fee - 0.3).abs() < 1e-9, "累计手续费应为 0.3");
    }

    /// P1-029：同订单多笔 fill 结算——每笔独立算盈亏，订单级聚合累加。
    #[test]
    fn test_ledger_multi_fill_settle_by_token() {
        let ledger = FillLedger::new();

        let mut f1 = FillRecord::new("ord-y".to_string(), "t1".to_string(), "BUY".to_string(), 0.5, 30.0);
        f1.trade_id = "trade-1".to_string();
        f1.mark_filled(0.50, 0.0);
        ledger.record(f1);

        let mut f2 = FillRecord::new("ord-y".to_string(), "t1".to_string(), "BUY".to_string(), 0.5, 20.0);
        f2.trade_id = "trade-2".to_string();
        f2.mark_filled(0.60, 0.0);
        ledger.record(f2);

        // 结算 t1（outcome=1.0），应结算 2 笔 fill
        let n = ledger.settle_by_token("t1", 1.0);
        assert_eq!(n, 2, "应结算 2 笔 fill");

        // 每笔 fill 独立盈亏之和：30*(1.0-0.5) + 20*(1.0-0.6) = 15 + 8 = 23
        let fills = ledger.list();
        let fill_pnl_sum: f64 = fills.iter().filter_map(|r| r.pnl).sum();
        assert!(
            (fill_pnl_sum - 23.0).abs() < 1e-9,
            "每笔 fill 盈亏之和应为 23.0，实际 {}",
            fill_pnl_sum
        );

        // 订单级聚合盈亏 = 23.0
        let agg = ledger.get("ord-y").unwrap();
        assert_eq!(agg.status, FillStatus::Settled);
        assert!(
            (agg.pnl.unwrap() - 23.0).abs() < 1e-9,
            "订单级盈亏应为 23.0"
        );
    }
}
