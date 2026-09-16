//! ExecutionService 实现 -- gRPC 服务核心。
//!
//! 接收 Python 策略引擎的 AggregatedOrder, 转换为 PlaceOrderParams,
//! 调用 OrderFlow 执行下单, 返回 OrderResult。
//!
//! 同时维护内部风控状态 (持仓、PnL), 供 Python 端查询。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use tokio::sync::{broadcast, Mutex};
use tonic::{Request, Response, Status};

use crate::clob::client::ClobConfig;
use crate::fast::executor::FastExecutor;
use crate::fast::fill_records::{compute_slippage, FillLedger, FillRecord};
use crate::fast::market_events::{MarketEvent, TradeEvent};
use crate::fast::order_state::{OrderState, OrderStore};
use crate::fast::risk_engine::{RiskConfig, RiskEngine};
use crate::fast::state_sync::{RecoveryAction, RecoveryPlan};
use crate::fast::trading_gate::TradingGate;
use crate::order::{OrderFlow, PlaceOrderParams};
use crate::types::enums::{OrderType, Side};
use crate::wallet::WalletManager;

use super::polymarket as pb;

/// 方向映射: proto Direction -> Rust Side
///
/// BUY_YES: 买入 YES token -> Side::Buy
/// BUY_NO:  买入 NO token  -> Side::Buy（token_id 已指向正确的 outcome token）
///
/// 关键语义（P0-02 修复）：BUY_NO 绝不等同于 SELL YES。
/// Python 发送 BUY_NO 时必须传入 no_token_id；Rust 侧无论 BUY_YES / BUY_NO
/// 都是 Side::Buy，差别仅在于 token_id 指向 YES 还是 NO。
/// 未来若需平仓，应新增独立的 SELL_YES / SELL_NO 方向。
fn direction_to_side(direction: i32) -> Result<Side, Status> {
    match direction {
        x if x == pb::Direction::BuyYes as i32 => Ok(Side::Buy),
        x if x == pb::Direction::BuyNo as i32 => Ok(Side::Buy),
        _ => Err(Status::invalid_argument(format!("未知方向: {}", direction))),
    }
}

/// 已有订单状态 → 幂等返回的 OrderResult。
fn existing_order_result(existing: &crate::fast::order_state::OrderState) -> pb::OrderResult {
    use crate::fast::order_state::OrderStateStatus;
    let status = match existing.status {
        OrderStateStatus::Filled => pb::ExecutionStatus::Filled as i32,
        OrderStateStatus::PartialFilled => pb::ExecutionStatus::Partial as i32,
        OrderStateStatus::Live => pb::ExecutionStatus::Live as i32,
        OrderStateStatus::Rejected => pb::ExecutionStatus::Rejected as i32,
        OrderStateStatus::Cancelled => pb::ExecutionStatus::Cancelled as i32,
        _ => pb::ExecutionStatus::Pending as i32,
    };
    pb::OrderResult {
        order_id: existing.client_order_id.clone(),
        success: true,
        exchange_order_id: existing.exchange_order_id.clone().unwrap_or_default(),
        error_message: String::new(),
        timestamp: Utc::now().to_rfc3339(),
        filled_price: 0.0,
        filled_size: existing.filled_size,
        status,
        ..Default::default()
    }
}

/// unix 秒 → ISO 8601 字符串。
fn format_unix(secs: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

/// 根据 CLOB order status 映射执行状态（P0-03 修复：accepted ≠ filled）。
///
/// - matched → Filled（真实成交）
/// - live → Live（挂单未成交）
/// - delayed/pending → Pending
/// - 其他 → Rejected（或 success 时退化为 Pending）
fn map_execution_status(status: &str, success: bool) -> pb::ExecutionStatus {
    match status.to_lowercase().as_str() {
        "matched" => pb::ExecutionStatus::Filled,
        "live" => pb::ExecutionStatus::Live,
        "delayed" | "pending" => pb::ExecutionStatus::Pending,
        "cancelled" | "canceled" => pb::ExecutionStatus::Cancelled,
        "rejected" => pb::ExecutionStatus::Rejected,
        "expired" => pb::ExecutionStatus::Expired,
        _ if success => pb::ExecutionStatus::Pending,
        _ => pb::ExecutionStatus::Rejected,
    }
}

/// 根据 CLOB order status 判断真实成交数量（P0-04 修复：只记真实 fill）。
///
/// - matched → 从 making/taking 推导真实成交数量（R2-P0-03：matched ≠ 全额）
/// - live/pending 等 → 0（未成交）
fn filled_size_from_status(
    status: &str,
    requested: f64,
    side: Side,
    making: &str,
    taking: &str,
) -> f64 {
    match status.to_lowercase().as_str() {
        "matched" => derive_filled_size(side, making, taking).unwrap_or(0.0),
        _ => 0.0,
    }
}

/// 根据 side 从 making/taking 推导真实成交数量（R2-P0-03：partial fill 正确）。
///
/// Polymarket CLOB 语义（maker 挂单 / taker 吃单）：
///   - BUY:  filled shares = takerAmount
///   - SELL: filled shares = makerAmount
fn derive_filled_size(side: Side, making: &str, taking: &str) -> Option<f64> {
    let maker = making.parse::<f64>().ok()?;
    let taker = taking.parse::<f64>().ok()?;
    if maker < 0.0 || taker < 0.0 {
        return None;
    }
    Some(match side {
        Side::Buy => taker,
        Side::Sell => maker,
    })
}

/// 根据 side 从 making/taking 推导成交价（R2-P0-21：不能无条件 maker/taker）。
///
/// Polymarket CLOB 语义：
///   - BUY:  price = makerAmount(USDC) / takerAmount(shares)
///   - SELL: price = takerAmount(USDC) / makerAmount(shares)
fn derive_price(side: Side, making: &str, taking: &str) -> Option<f64> {
    let maker = making.parse::<f64>().ok()?;
    let taker = taking.parse::<f64>().ok()?;
    if maker <= 0.0 || taker <= 0.0 {
        return None;
    }
    Some(match side {
        Side::Buy => maker / taker,
        Side::Sell => taker / maker,
    })
}

/// 执行服务实现。
#[derive(Clone)]
pub struct ExecutionServiceImpl {
    /// 下单流程 (内部封装了 ClobClient + WalletManager)
    order_flow: Arc<Mutex<OrderFlow>>,

    /// 钱包管理器
    wallet_manager: Arc<WalletManager>,

    /// 钱包地址
    wallet_address: String,

    /// 钱包密码
    password: String,

    /// 钱包是否已加载
    wallet_loaded: bool,

    /// API 凭证是否已设置
    api_creds_set: bool,

    /// 启动时间
    start_time: Instant,

    /// 内部风控状态
    risk_state: Arc<Mutex<RiskStateInternal>>,

    /// 亚秒级执行器（可选）。设置后 `place_order` 走 fast path，
    /// 消除 PBKDF2 与 tick 往返，本地计算压进 100ms 预算。
    fast_executor: Option<Arc<FastExecutor>>,

    /// 成交记录账簿 —— 统一记录每笔订单的下单/成交/结算完整字段。
    fill_ledger: Arc<FillLedger>,

    /// 订单状态存储 —— 状态机 + 幂等去重（R2-P0-05/06）。
    order_store: Arc<OrderStore>,

    /// 交易闸门 —— 未达到 READY 时禁止新订单进入执行路径。
    trading_gate: Arc<TradingGate>,

    /// 风控引擎（可选）—— 挂载后 place_order 强制走 pre_check（R2-P0-08）。
    risk_engine: Option<Arc<RiskEngine>>,
}

/// 内部风控状态 (对应 proto RiskState)。
#[derive(Default)]
struct RiskStateInternal {
    /// 持仓 {token_id: 数量}
    positions: HashMap<String, f64>,

    /// 当日盈亏 (USDC)
    daily_pnl: f64,

    /// 每日亏损熔断
    daily_circuit_breaker: bool,

    /// 手动熔断
    manual_halt: bool,

    /// 上次重置日期
    last_reset_date: String,
}

impl RiskStateInternal {
    fn to_proto(&self) -> pb::RiskState {
        let positions: Vec<pb::PositionEntry> = self
            .positions
            .iter()
            .map(|(tid, qty)| pb::PositionEntry {
                token_id: tid.clone(),
                quantity: *qty,
            })
            .collect();

        pb::RiskState {
            positions,
            settlement_times: Vec::new(), // TODO: 从市场数据获取
            daily_pnl: self.daily_pnl,
            daily_circuit_breaker: self.daily_circuit_breaker,
            manual_halt: self.manual_halt,
            last_reset_date: self.last_reset_date.clone(),
        }
    }

    fn snapshot(&self) -> pb::RiskState {
        self.to_proto()
    }
}

impl ExecutionServiceImpl {
    /// 创建新的执行服务实例。
    ///
    /// Args:
    ///   order_flow: 已初始化的 OrderFlow (含 ClobClient + WalletManager)
    ///   wallet_address: 钱包地址
    ///   password: 钱包密码
    pub fn new(
        order_flow: OrderFlow,
        wallet_manager: Arc<WalletManager>,
        wallet_address: String,
        password: String,
        trading_gate: Arc<TradingGate>,
    ) -> Self {
        Self {
            order_flow: Arc::new(Mutex::new(order_flow)),
            wallet_manager,
            wallet_address,
            password,
            wallet_loaded: true,
            api_creds_set: false,
            start_time: Instant::now(),
            risk_state: Arc::new(Mutex::new(RiskStateInternal {
                last_reset_date: Utc::now().format("%Y-%m-%d").to_string(),
                ..Default::default()
            })),
            fast_executor: None,
            fill_ledger: Arc::new(FillLedger::new()),
            order_store: Arc::new(OrderStore::new()),
            trading_gate,
            risk_engine: None,
        }
    }
    /// 获取当前本地仍处于活动状态的远端订单 ID。
    ///
    /// 用于 StateSync 启动对账。
    pub fn snapshot_pending_exchange_order_ids(&self) -> std::collections::HashSet<String> {
        self.order_store.pending_exchange_order_ids()
    }
    /// 获取当前本地风险状态中的仓位快照。
    ///
    /// 用于 StateSync 启动对账。
    pub async fn snapshot_positions(&self) -> std::collections::HashMap<String, f64> {
        self.risk_state.lock().await.positions.clone()
    }
    /// 创建用于测试的 Mock 实例 (不连接真实 API)。
    pub fn new_mock() -> Self {
        let wallet_mgr = Arc::new(WalletManager::new("keystore_mock.json"));
        let flow = OrderFlow::new(wallet_mgr.clone(), ClobConfig::default());
        Self {
            order_flow: Arc::new(Mutex::new(flow)),
            wallet_manager: wallet_mgr,
            wallet_address: String::new(),
            password: String::new(),
            wallet_loaded: false,
            api_creds_set: false,
            start_time: Instant::now(),
            risk_state: Arc::new(Mutex::new(RiskStateInternal {
                last_reset_date: Utc::now().format("%Y-%m-%d").to_string(),
                ..Default::default()
            })),
            fast_executor: None,
            fill_ledger: Arc::new(FillLedger::new()),
            order_store: Arc::new(OrderStore::new()),
            trading_gate: Arc::new(TradingGate::new()),
            risk_engine: None,
        }
    }

    /// 挂载亚秒级执行器。此后 `place_order` 优先走 fast path。
    ///
    /// 注意：应在挂载前对 `FastExecutor` 调用过 `init_api_key`（预热钱包/
    /// 连接/域分隔符），否则首笔单会付出一次 PBKDF2 代价。
    pub fn set_fast_executor(&mut self, fe: Arc<FastExecutor>) {
        self.fast_executor = Some(fe);
    }

    /// 挂载风控引擎。此后 `place_order` 强制走 pre_check（R2-P0-08）。
    pub fn set_risk_engine(&mut self, re: Arc<RiskEngine>) {
        self.risk_engine = Some(re);
    }

    /// 获取成交记录账簿引用。
    pub fn fill_ledger(&self) -> &Arc<FillLedger> {
        &self.fill_ledger
    }

    /// 列出全部成交记录（JSON 字符串）。
    pub fn list_fills_json(&self) -> String {
        self.fill_ledger.to_json()
    }

    /// 结算某笔订单（更新盈亏，返回是否命中）。
    pub fn settle_fill(&self, order_id: &str, pnl: f64) -> bool {
        self.fill_ledger.settle(order_id, pnl)
    }

    /// 按市场结算：给定 token_id 与结算结果 outcome（0.0 或 1.0），
    /// 自动结算该市场下所有未结算成交，回填盈亏。返回结算条数。
    pub fn settle_token(&self, token_id: &str, outcome: f64) -> usize {
        self.fill_ledger.settle_by_token(token_id, outcome)
    }

    /// P1-006：异步成交同步。
    ///
    /// 将 market_events 轮询到的成交事件（TradeEvent）同步到：
    /// 1. RiskEngine 权威仓位（on_fill）
    /// 2. 内部 risk_state 快照（保持与 snapshot_positions 一致）
    /// 3. FillLedger 成交账簿
    /// 4. OrderStore 订单状态
    pub async fn on_market_trade(&self, trade: &TradeEvent) {
        let side = match trade.side.as_str() {
            "BUY" => Side::Buy,
            "SELL" => Side::Sell,
            _ => {
                tracing::warn!("忽略未知 side 的成交事件: {}", trade.side);
                return;
            }
        };

        // P0-015：fill 幂等——同一 trade_id 只处理一次（防双路/重复轮询重复记账）
        if !self.fill_ledger.claim_trade(&trade.trade_id) {
            tracing::debug!("on_market_trade: trade_id={} 已处理，跳过", trade.trade_id);
            return;
        }

        let notional = trade.price * trade.size;

        // 1. RiskEngine 权威仓位
        if let Some(re) = self.risk_engine.as_ref() {
            re.on_fill(&trade.token_id, side, trade.size, notional);
        }

        // 2. 内部 risk_state 快照
        {
            let mut rs = self.risk_state.lock().await;
            let entry = rs.positions.entry(trade.token_id.clone()).or_insert(0.0);
            match side {
                Side::Buy => *entry += trade.size,
                Side::Sell => *entry -= trade.size,
            }
        }

        // 3. FillLedger 成交账簿
        if !trade.order_id.is_empty() {
            let mut rec = FillRecord::new(
                trade.order_id.clone(),
                trade.token_id.clone(),
                trade.side.clone(),
                trade.price,
                trade.size,
            );
            rec.trade_id = trade.trade_id.clone(); // P0-015：fill 级 identity
            rec.mark_filled(trade.price, 0.0);
            self.fill_ledger.record(rec);
        }

        // 4. OrderStore 订单状态（P0-016：trade.order_id 是交易所订单 ID，
        //    需按 exchange_order_id 反查，而非 client_order_id；
        //    P1-026：用 apply_fill 增量累加，避免多笔 partial fill 绝对覆盖；
        //    P1-027：改用 apply_fill_by_exchange_order_id 单写锁内完成，避免并发 fill lost update）
        self.order_store
            .apply_fill_by_exchange_order_id(&trade.order_id, trade.size);
    }

    /// P0-007B：应用 StateSync 生成的恢复计划，修复本地状态。
    ///
    /// - `MarkOrderFilled` → 走 `on_market_trade`（补记 FillLedger + RiskEngine + OrderStore，
    ///   复用 trade_id 幂等去重，防重复记账）
    /// - `MarkOrderNonFilled` → 标记 cancelled（终态，清除 pending）
    /// - `RebuildPosition` → 以远端为权威重建 risk_state 与 RiskEngine 仓位
    ///
    /// 返回应用的恢复动作数量。
    pub async fn apply_recovery(&self, plan: &RecoveryPlan) -> usize {
        for action in &plan.actions {
            match action {
                RecoveryAction::MarkOrderFilled {
                    order_id,
                    token_id,
                    side,
                    price,
                    size,
                    trade_id,
                } => {
                    let trade = TradeEvent {
                        token_id: token_id.clone(),
                        trade_id: trade_id.clone(),
                        order_id: order_id.clone(),
                        side: side.clone(),
                        price: *price,
                        size: *size,
                        timestamp: 0,
                        taker_order_id: order_id.clone(),
                        maker_order_id: String::new(),
                        trader_side: "TAKER".to_string(),
                        owner: String::new(),
                        status: "MATCHED".to_string(),
                    };
                    self.on_market_trade(&trade).await;
                }
                RecoveryAction::MarkOrderNonFilled { order_id } => {
                    // P0-016：order_id 是交易所订单 ID（来自 pending_exchange_order_ids），
                    //    需按 exchange_order_id 反查
                    if let Some(mut st) = self.order_store.get_by_exchange_order_id(order_id) {
                        st.mark_cancelled();
                        self.order_store.upsert(st);
                    }
                }
                RecoveryAction::RebuildPosition {
                    token_id,
                    qty,
                    notional,
                } => {
                    {
                        let mut rs = self.risk_state.lock().await;
                        if qty.abs() < 1e-9 && notional.abs() < 1e-9 {
                            rs.positions.remove(token_id);
                        } else {
                            rs.positions.insert(token_id.clone(), *qty);
                        }
                    }
                    if let Some(re) = self.risk_engine.as_ref() {
                        re.sync_position(token_id, *qty, *notional);
                    }
                }
            }
        }
        plan.actions.len()
    }

    /// P1-031：重建远端仍 open 的 BUY 订单的预留额度。
    ///
    /// 对账恢复时 `sync_position` 会清空各市场的 reserved（幽灵预留），但远端
    /// 仍 open 的订单（本地下单、未成交、仍在挂单）对应的 pending 预留没有重建。
    /// 本方法遍历本地 pending 订单（`OrderStore` 持有正确的 token_id，规避
    /// open order `asset_id` 与 ERC-1155 token_id 的映射问题），按 exchange_order_id
    /// 匹配远端 open order 的 price，计算剩余 notional = remaining_size × price，
    /// 按 token 聚合后回填 RiskEngine.reserved。
    pub fn rebuild_open_order_reserves(&self, remote_open_orders: &[serde_json::Value]) {
        let re = match self.risk_engine.as_ref() {
            Some(re) => re,
            None => return,
        };

        // 远端 open order 的 price 索引（按 id）
        let price_by_id: HashMap<String, f64> = remote_open_orders
            .iter()
            .filter_map(|o| {
                let id = o.get("id")?.as_str()?.to_string();
                let price = match o.get("price") {
                    Some(serde_json::Value::String(s)) => s.parse::<f64>().ok()?,
                    Some(serde_json::Value::Number(n)) => n.as_f64()?,
                    _ => return None,
                };
                Some((id, price))
            })
            .collect();

        // 按 token 聚合 open BUY 订单的剩余 notional
        let mut reserve_by_token: HashMap<String, f64> = HashMap::new();
        for ex_id in self.order_store.pending_exchange_order_ids() {
            let Some(st) = self.order_store.get_by_exchange_order_id(&ex_id) else {
                continue;
            };
            if st.side != "BUY" || st.remaining_size <= 0.0 {
                continue;
            }
            let Some(price) = price_by_id.get(&ex_id) else {
                continue;
            };
            let notional = st.remaining_size * price;
            if notional > 0.0 {
                *reserve_by_token.entry(st.token_id.clone()).or_insert(0.0) += notional;
            }
        }

        for (token_id, notional) in reserve_by_token {
            re.rebuild_reserve(&token_id, notional);
        }
    }
}

/// P0-014：市场事件消费循环。把 poller 广播的 Trade 事件转发到
/// [`ExecutionServiceImpl::on_market_trade`]（成交 → RiskEngine/FillLedger/OrderStore）。
///
/// 当 broadcast sender 全部 drop 后（`rx.recv()` 返回 Err）循环退出。
pub async fn consume_trade_events(
    service: Arc<ExecutionServiceImpl>,
    mut rx: broadcast::Receiver<MarketEvent>,
) {
    while let Ok(event) = rx.recv().await {
        if let MarketEvent::Trade(trade) = event {
            service.on_market_trade(&trade).await;
        }
    }
}

#[tonic::async_trait]
impl pb::execution_service_server::ExecutionService for ExecutionServiceImpl {
    /// 下单: 接收 AggregatedOrder, 执行下单, 返回 OrderResult。
    async fn place_order(
        &self,
        request: Request<pb::AggregatedOrder>,
    ) -> Result<Response<pb::OrderResult>, Status> {
        let order = request.into_inner();
        self.trading_gate
            .ensure_ready()
            .map_err(Status::failed_precondition)?;

        // 方向转换（提前到幂等检查之前，供原子占位使用）
        let side = direction_to_side(order.direction)?;

        // ★ P0-004 幂等：原子 claim_or_retry 占位，并发下只允许一个请求下单；
        // 可重试终态（Rejected/Expired/Cancelled）会被重置并重新下单。
        if !order.order_id.is_empty() {
            if self
                .order_store
                .claim_or_retry(
                    order.order_id.clone(),
                    order.token_id.clone(),
                    side.as_str().to_string(),
                    order.quantity,
                )
                .is_none()
            {
                // 进行中或已成交：幂等去重，返回已有状态
                if let Some(existing) = self.order_store.get(&order.order_id) {
                    tracing::info!(
                        "order_id={} 幂等去重（进行中/已成交），返回已有状态 {}",
                        order.order_id,
                        existing.status
                    );
                    return Ok(Response::new(existing_order_result(&existing)));
                }
            }
        }

        // 只执行 OPEN 状态的指令
        if order.status != pb::OrderStatus::Open as i32 {
            self.order_store.mark_rejected(&order.order_id);
            return Ok(Response::new(pb::OrderResult {
                order_id: order.order_id.clone(),
                success: false,
                exchange_order_id: String::new(),
                error_message: format!("指令状态非 OPEN, 跳过: {:?}", order.status),
                timestamp: Utc::now().to_rfc3339(),
                filled_price: 0.0,
                filled_size: 0.0,
                status: pb::ExecutionStatus::Rejected as i32,
                ..Default::default()
            }));
        }

        // ★ 纸面交易模式：不真正下单，返回模拟成交
        if order.paper_trading {
            tracing::info!(
                "[PAPER] 纸面下单: order_id={}, token={}, price={}, qty={}, conf={:.2}",
                order.order_id,
                order.token_id,
                order.target_price,
                order.quantity,
                order.confidence,
            );
            let ex_id = format!("paper_{}", order.order_id);
            let side_str = match order.direction {
                x if x == pb::Direction::BuyYes as i32 => "BUY",
                x if x == pb::Direction::BuyNo as i32 => "BUY",
                _ => "BUY",
            };
            // 记录成交（纸面成交也纳入账簿）
            let mut rec = FillRecord::new(
                ex_id.clone(),
                order.token_id.clone(),
                side_str.to_string(),
                order.target_price,
                order.quantity,
            );
            rec.mark_filled(order.target_price, 0.0);
            self.fill_ledger.record(rec);

            return Ok(Response::new(pb::OrderResult {
                order_id: order.order_id.clone(),
                success: true,
                exchange_order_id: ex_id,
                error_message: String::new(),
                timestamp: Utc::now().to_rfc3339(),
                filled_price: order.target_price,
                filled_size: order.quantity,
                status: pb::ExecutionStatus::Filled as i32,
                filled_at: Utc::now().to_rfc3339(),
                settled_at: String::new(),
                slippage: 0.0,
                slippage_pct: 0.0,
                fee: 0.0,
                pnl: 0.0,
            }));
        }

        // 检查熔断
        {
            let rs = self.risk_state.lock().await;
            if rs.daily_circuit_breaker || rs.manual_halt {
                self.order_store.mark_rejected(&order.order_id);
                return Ok(Response::new(pb::OrderResult {
                    order_id: order.order_id.clone(),
                    success: false,
                    exchange_order_id: String::new(),
                    error_message: "交易已熔断, 无法下单".to_string(),
                    timestamp: Utc::now().to_rfc3339(),
                    filled_price: 0.0,
                    filled_size: 0.0,
                    status: pb::ExecutionStatus::Rejected as i32,
                    ..Default::default()
                }));
            }
        }

        // ★ R2-P0-08：RiskEngine 必须存在，缺失时 FAIL-CLOSED
        let re = match self.risk_engine.as_ref() {
            Some(re) => re,
            None => {
                tracing::error!(
                    "RiskEngine 未初始化，拒绝实时订单: order_id={}, token={}",
                    order.order_id,
                    order.token_id
                );

                self.order_store.mark_rejected(&order.order_id);
                return Ok(Response::new(pb::OrderResult {
                    order_id: order.order_id.clone(),
                    success: false,
                    exchange_order_id: String::new(),
                    error_message: "RiskEngine 未初始化，禁止实时交易".to_string(),
                    timestamp: Utc::now().to_rfc3339(),
                    filled_price: 0.0,
                    filled_size: 0.0,
                    status: pb::ExecutionStatus::Rejected as i32,
                    ..Default::default()
                }));
            }
        };

        let notional = order.quantity * order.target_price;

        // ★ P0-008：原子「检查 + 预留」——并发下单不会同时通过仓位上限。
        if let Err(e) = re.check_and_reserve(&order.token_id, side, notional, order.quantity, 0) {
            tracing::warn!("风控预检拦截: token={}, {}", order.token_id, e);

            self.order_store.mark_rejected(&order.order_id);
            return Ok(Response::new(pb::OrderResult {
                order_id: order.order_id.clone(),
                success: false,
                exchange_order_id: String::new(),
                error_message: format!("风控预检拦截: {}", e),
                timestamp: Utc::now().to_rfc3339(),
                filled_price: 0.0,
                filled_size: 0.0,
                status: pb::ExecutionStatus::Rejected as i32,
                ..Default::default()
            }));
        }

        // 亚秒级 fast path：若已挂载 FastExecutor，走热路径（缓存命中时本地 < 1ms）
        if let Some(fe) = &self.fast_executor {
            let params = crate::fast::executor::FastOrderParams {
                wallet_address: self.wallet_address.clone(),
                password: Some(self.password.clone()),
                token_id: order.token_id.clone(),
                price: order.target_price,
                size: order.quantity,
                side,
                tick_size: None,
                neg_risk: None,
                order_type: Some(OrderType::GTC),
                prefer_presign: false,
            };
            match fe.execute_fast(&params).await {
                Ok(result) => {
                    let success = result.response.success;
                    let exchange_order_id = result.response.orderID.clone();
                    let error_message = result.response.errorMsg.clone();
                    // 状态机：accepted ≠ filled（P0-03）
                    let execution_status = map_execution_status(&result.response.status, success);
                    // R2-P0-03/21：从 making/taking 按 side 推导真实 fill 与成交价
                    let filled_size = filled_size_from_status(
                        &result.response.status,
                        order.quantity,
                        side,
                        &result.response.makingAmount,
                        &result.response.takingAmount,
                    );
                    let filled_price = derive_price(
                        side,
                        &result.response.makingAmount,
                        &result.response.takingAmount,
                    )
                    .unwrap_or(order.target_price);
                    let (slippage, slippage_pct) =
                        compute_slippage(order.target_price, filled_price, filled_size);

                    // 记录订单状态（R2-P0-05 状态机）
                    if !order.order_id.is_empty() {
                        let mut st = self.order_store.get(&order.order_id).unwrap_or_else(|| {
                            self.order_store.create(
                                order.order_id.clone(),
                                order.token_id.clone(),
                                side.as_str().to_string(),
                                order.quantity,
                            )
                        });
                        if !exchange_order_id.is_empty() {
                            st.mark_submitted(exchange_order_id.clone());
                        }
                        if filled_size > 0.0 {
                            st.update_fill(filled_size);
                        } else if st.status == crate::fast::order_state::OrderStateStatus::Submitted
                        {
                            st.mark_live();
                        }
                        self.order_store.upsert(st);
                    }

                    // 方案 X（P0-015 收尾）：成交记账唯一走 on_market_trade（路径 B）。
                    // 下单路径只更新 OrderStore 状态，不再 on_fill / 记 FillLedger，
                    // 避免与轮询成交双路重复记账。仓位预留由 P0-008 check-and-reserve 解决。

                    return Ok(Response::new(pb::OrderResult {
                        order_id: order.order_id.clone(),
                        success,
                        exchange_order_id,
                        error_message,
                        timestamp: Utc::now().to_rfc3339(),
                        filled_price,
                        filled_size,
                        status: execution_status as i32,
                        filled_at: Utc::now().to_rfc3339(),
                        settled_at: String::new(),
                        slippage,
                        slippage_pct,
                        fee: 0.0,
                        pnl: 0.0,
                    }));
                }
                Err(e) => {
                    tracing::error!("fast path 下单失败: {}", e);
                    // P0-008：下单失败，释放预留额度
                    re.release_reserve(&order.token_id, notional);
                    self.order_store.mark_rejected(&order.order_id);
                    return Ok(Response::new(pb::OrderResult {
                        order_id: order.order_id.clone(),
                        success: false,
                        exchange_order_id: String::new(),
                        error_message: format!("fast path 下单失败: {}", e),
                        timestamp: Utc::now().to_rfc3339(),
                        filled_price: 0.0,
                        filled_size: 0.0,
                        status: pb::ExecutionStatus::Rejected as i32,
                        ..Default::default()
                    }));
                }
            }
        }

        tracing::error!(
            "FastExecutor 未初始化，拒绝实时订单，禁止降级到普通 OrderFlow: order_id={}",
            order.order_id
        );

        // P0-008：FastExecutor 缺失，释放预留额度
        re.release_reserve(&order.token_id, notional);
        self.order_store.mark_rejected(&order.order_id);
        return Ok(Response::new(pb::OrderResult {
            order_id: order.order_id.clone(),
            success: false,
            exchange_order_id: String::new(),
            error_message: "FastExecutor 未初始化，禁止进入普通交易路径".to_string(),
            timestamp: Utc::now().to_rfc3339(),
            filled_price: 0.0,
            filled_size: 0.0,
            status: pb::ExecutionStatus::Rejected as i32,
            ..Default::default()
        }));
    }

    /// 批量下单。
    async fn place_orders(
        &self,
        request: Request<pb::PlaceOrdersRequest>,
    ) -> Result<Response<pb::PlaceOrdersResponse>, Status> {
        let orders = request.into_inner().orders;
        let mut results = Vec::with_capacity(orders.len());

        for order in orders {
            let result = self.place_order(Request::new(order)).await?.into_inner();
            results.push(result);
        }

        Ok(Response::new(pb::PlaceOrdersResponse { results }))
    }

    /// 取消指定订单。
    async fn cancel_order(
        &self,
        request: Request<pb::CancelOrderRequest>,
    ) -> Result<Response<pb::CancelResult>, Status> {
        let req = request.into_inner();
        let flow = self.order_flow.lock().await;

        match flow
            .cancel_order(&self.wallet_address, &self.password, &req.order_id)
            .await
        {
            Ok(()) => Ok(Response::new(pb::CancelResult {
                success: true,
                message: format!("已取消订单 {}", req.order_id),
                cancelled_count: 1,
            })),
            Err(e) => Ok(Response::new(pb::CancelResult {
                success: false,
                message: format!("取消失败: {}", e),
                cancelled_count: 0,
            })),
        }
    }

    /// 取消所有订单。

    /// 取消所有订单。
    async fn cancel_all(
        &self,
        _request: Request<pb::CancelAllRequest>,
    ) -> Result<Response<pb::CancelResult>, Status> {
        let flow = self.order_flow.lock().await;

        match flow.cancel_all(&self.wallet_address, &self.password).await {
            Ok(()) => Ok(Response::new(pb::CancelResult {
                success: true,
                message: "已取消全部未成交订单，持仓保持不变".to_string(),
                cancelled_count: 0,
            })),
            Err(e) => Ok(Response::new(pb::CancelResult {
                success: false,
                message: format!("取消全部失败: {}", e),
                cancelled_count: 0,
            })),
        }
    }

    /// 查询当前风控状态。
    async fn get_risk_state(
        &self,
        _request: Request<pb::Empty>,
    ) -> Result<Response<pb::RiskState>, Status> {
        let rs = self.risk_state.lock().await;
        Ok(Response::new(rs.snapshot()))
    }

    /// 流式推送风控状态更新。
    type StreamRiskStateUpdatesStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<pb::RiskStateUpdate, Status>> + Send>,
    >;

    async fn stream_risk_state_updates(
        &self,
        _request: Request<pb::Empty>,
    ) -> Result<Response<Self::StreamRiskStateUpdatesStream>, Status> {
        let risk_state = self.risk_state.clone();

        let stream = async_stream::try_stream! {
            // 先推一次当前状态
            {
                let rs = risk_state.lock().await;
                yield pb::RiskStateUpdate {
                    risk_state: Some(rs.snapshot()),
                    update_reason: "initial".to_string(),
                    timestamp: Utc::now().to_rfc3339(),
                };
            }

            // 心跳循环
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                let rs = risk_state.lock().await;
                yield pb::RiskStateUpdate {
                    risk_state: Some(rs.snapshot()),
                    update_reason: "heartbeat".to_string(),
                    timestamp: Utc::now().to_rfc3339(),
                };
            }
        };

        Ok(Response::new(Box::pin(stream)))
    }

    /// 健康检查。
    async fn health_check(
        &self,
        _request: Request<pb::Empty>,
    ) -> Result<Response<pb::HealthCheckResponse>, Status> {
        let uptime_secs = self.start_time.elapsed().as_secs();
        let hours = uptime_secs / 3600;
        let mins = (uptime_secs % 3600) / 60;
        let secs = uptime_secs % 60;

        Ok(Response::new(pb::HealthCheckResponse {
            healthy: true,
            version: "0.2.0".to_string(),
            uptime: format!("{}h {}m {}s", hours, mins, secs),
            wallet_loaded: self.wallet_loaded,
            api_creds_set: self.api_creds_set,
        }))
    }

    /// 查询成交记录列表（完整字段：下单/成交/结算时间 + 滑点 + 手续费 + 盈亏）。
    async fn get_fills(
        &self,
        _request: Request<pb::Empty>,
    ) -> Result<Response<pb::FillListResponse>, Status> {
        let fills = self.fill_ledger.list();
        let stats = self.fill_ledger.stats();

        let fill_msgs: Vec<pb::FillRecord> = fills
            .iter()
            .map(|r| pb::FillRecord {
                order_id: r.order_id.clone(),
                token_id: r.token_id.clone(),
                side: r.side.clone(),
                placed_at: format_unix(r.placed_at),
                filled_at: format_unix(r.filled_at),
                settled_at: r.settled_at.map(format_unix).unwrap_or_default(),
                limit_price: r.limit_price,
                filled_price: r.filled_price,
                size: r.size,
                fee: r.fee,
                slippage: r.slippage,
                slippage_pct: r.slippage_pct,
                pnl: r.pnl.unwrap_or(0.0),
                status: r.status.to_string(),
            })
            .collect();

        Ok(Response::new(pb::FillListResponse {
            fills: fill_msgs,
            total: stats.total as i32,
            total_fee: stats.total_fee,
            total_slippage: stats.total_slippage,
            total_pnl: stats.total_pnl,
        }))
    }

    /// 添加钱包（导入私钥）。
    ///
    /// R2-P0 安全修复：生产禁止通过 gRPC 传输私钥。私钥应在 Rust 端
    /// 本地 keystore 文件管理（server 启动时加载），Python 永不持有私钥。
    async fn add_wallet(
        &self,
        request: Request<pb::AddWalletRequest>,
    ) -> Result<Response<pb::WalletResult>, Status> {
        let req = request.into_inner();
        tracing::warn!("AddWallet 通过 gRPC 传输私钥（安全红线），已拒绝");
        // 拒绝通过 RPC 导入私钥，强制使用本地 keystore
        return Ok(Response::new(pb::WalletResult {
            success: false,
            address: String::new(),
            message: "禁止通过 gRPC 导入私钥；请在 Rust 端本地 keystore 文件管理钱包".to_string(),
        }));
    }

    /// 删除钱包。
    async fn remove_wallet(
        &self,
        request: Request<pb::RemoveWalletRequest>,
    ) -> Result<Response<pb::WalletResult>, Status> {
        let req = request.into_inner();
        match self.wallet_manager.remove_wallet(&req.address) {
            Ok(()) => {
                let _ = self.wallet_manager.save();
                tracing::info!("已删除钱包: {}", req.address);
                Ok(Response::new(pb::WalletResult {
                    success: true,
                    address: req.address.clone(),
                    message: "钱包已删除".to_string(),
                }))
            }
            Err(e) => Ok(Response::new(pb::WalletResult {
                success: false,
                address: req.address,
                message: format!("删除钱包失败: {}", e),
            })),
        }
    }

    /// 列出所有钱包地址。
    async fn list_wallets(
        &self,
        _request: Request<pb::Empty>,
    ) -> Result<Response<pb::WalletListResponse>, Status> {
        let addresses = self.wallet_manager.list_addresses();
        Ok(Response::new(pb::WalletListResponse { addresses }))
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_execution_status_matched_is_filled() {
        assert_eq!(
            map_execution_status("matched", true),
            pb::ExecutionStatus::Filled
        );
        assert_eq!(
            map_execution_status("MATCHED", true),
            pb::ExecutionStatus::Filled
        );
    }

    #[test]
    fn test_map_execution_status_live_is_not_filled() {
        // P0-03 核心：accepted/live ≠ filled
        assert_eq!(
            map_execution_status("live", true),
            pb::ExecutionStatus::Live
        );
    }

    #[test]
    fn test_map_execution_status_delayed_pending() {
        assert_eq!(
            map_execution_status("delayed", true),
            pb::ExecutionStatus::Pending
        );
    }

    #[test]
    fn test_map_execution_status_rejected() {
        assert_eq!(
            map_execution_status("rejected", false),
            pb::ExecutionStatus::Rejected
        );
    }

    #[test]
    fn test_filled_size_matched_full() {
        // BUY: filled shares = takerAmount
        assert_eq!(
            filled_size_from_status("matched", 10.0, Side::Buy, "5.0", "10.0"),
            10.0
        );
    }

    #[test]
    fn test_filled_size_matched_partial() {
        // R2-P0-03：matched 不等于全额；BUY 请求 100，实际 takerAmount=37 → filled=37
        assert_eq!(
            filled_size_from_status("matched", 100.0, Side::Buy, "18.5", "37.0"),
            37.0
        );
    }

    #[test]
    fn test_filled_size_live_zero() {
        // P0-04 核心：live 订单不是 fill，filled_size 必须为 0
        assert_eq!(
            filled_size_from_status("live", 10.0, Side::Buy, "", ""),
            0.0
        );
        assert_eq!(
            filled_size_from_status("pending", 10.0, Side::Buy, "", ""),
            0.0
        );
        assert_eq!(filled_size_from_status("", 10.0, Side::Buy, "", ""), 0.0);
    }

    #[test]
    fn test_derive_price_by_side() {
        // R2-P0-21：BUY price = maker/taker；SELL price = taker/maker
        let buy = derive_price(Side::Buy, "5.0", "10.0").unwrap();
        assert!((buy - 0.5).abs() < 1e-9);
        let sell = derive_price(Side::Sell, "10.0", "5.0").unwrap();
        assert!((sell - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_direction_to_side_buy_no_is_buy() {
        // P0-02 核心：BUY_NO 也是 Side::Buy，绝不等同于 SELL YES
        let side = direction_to_side(pb::Direction::BuyNo as i32).unwrap();
        assert_eq!(side, Side::Buy);

        let side_yes = direction_to_side(pb::Direction::BuyYes as i32).unwrap();
        assert_eq!(side_yes, Side::Buy);
    }

    #[tokio::test]
    async fn test_on_market_trade_updates_fill_ledger_and_risk() {
        let mut service = ExecutionServiceImpl::new_mock();
        let risk_engine = Arc::new(RiskEngine::new(RiskConfig::default()));
        service.set_risk_engine(risk_engine);

        let trade = TradeEvent {
            token_id: "token-1".to_string(),
            trade_id: "trade-1".to_string(),
            order_id: "order-1".to_string(),
            side: "BUY".to_string(),
            price: 0.5,
            size: 10.0,
            timestamp: 0,
            taker_order_id: "order-1".to_string(),
            maker_order_id: "maker-1".to_string(),
            trader_side: "TAKER".to_string(),
            owner: "0xowner".to_string(),
            status: "MATCHED".to_string(),
        };

        service.on_market_trade(&trade).await;

        // FillLedger 记录 1 条
        assert_eq!(service.fill_ledger().list().len(), 1);

        // risk_state 仓位更新（BUY +10）
        let rs = service.risk_state.lock().await;
        assert_eq!(rs.positions.get("token-1").copied().unwrap_or(0.0), 10.0);
    }

    /// P0-015：同一 trade_id 重复投递只记账一次。
    #[tokio::test]
    async fn test_on_market_trade_idempotent_by_trade_id() {
        let mut service = ExecutionServiceImpl::new_mock();
        let risk_engine = Arc::new(RiskEngine::new(RiskConfig::default()));
        service.set_risk_engine(risk_engine);

        let trade = TradeEvent {
            token_id: "token-1".to_string(),
            trade_id: "trade-1".to_string(),
            order_id: "order-1".to_string(),
            side: "BUY".to_string(),
            price: 0.5,
            size: 10.0,
            timestamp: 0,
            taker_order_id: "order-1".to_string(),
            maker_order_id: "maker-1".to_string(),
            trader_side: "TAKER".to_string(),
            owner: "0xowner".to_string(),
            status: "MATCHED".to_string(),
        };

        service.on_market_trade(&trade).await;
        service.on_market_trade(&trade).await; // 重复投递

        // FillLedger 只记 1 条
        assert_eq!(service.fill_ledger().list().len(), 1);
        // 仓位只 +10 一次（不重复累加）
        let rs = service.risk_state.lock().await;
        assert_eq!(rs.positions.get("token-1").copied().unwrap_or(0.0), 10.0);
    }

    /// P1-026：on_market_trade 多笔 partial fill 累加（apply_fill 增量，非绝对覆盖）。
    #[tokio::test]
    async fn test_on_market_trade_accumulates_partial_fills() {
        use crate::fast::order_state::OrderStateStatus;

        let mut service = ExecutionServiceImpl::new_mock();
        service.set_risk_engine(Arc::new(RiskEngine::new(RiskConfig::default())));

        // 造一个 100 shares 订单（client 键 + exchange 字段）
        service
            .order_store
            .create("client-pf".to_string(), "tok-pf".to_string(), "BUY".to_string(), 100.0);
        if let Some(mut st) = service.order_store.get("client-pf") {
            st.mark_submitted("exchange-pf".to_string());
            st.mark_live();
            service.order_store.upsert(st);
        }

        // 三笔 partial fill（不同 trade_id，同一订单）
        let t1 = TradeEvent {
            token_id: "tok-pf".to_string(),
            trade_id: "trade-pf-1".to_string(),
            order_id: "exchange-pf".to_string(),
            side: "BUY".to_string(),
            price: 0.5,
            size: 30.0,
            timestamp: 0,
            taker_order_id: "exchange-pf".to_string(),
            maker_order_id: String::new(),
            trader_side: "TAKER".to_string(),
            owner: String::new(),
            status: "MATCHED".to_string(),
        };
        let t2 = TradeEvent {
            token_id: "tok-pf".to_string(),
            trade_id: "trade-pf-2".to_string(),
            order_id: "exchange-pf".to_string(),
            side: "BUY".to_string(),
            price: 0.5,
            size: 40.0,
            timestamp: 0,
            taker_order_id: "exchange-pf".to_string(),
            maker_order_id: String::new(),
            trader_side: "TAKER".to_string(),
            owner: String::new(),
            status: "MATCHED".to_string(),
        };
        let t3 = TradeEvent {
            token_id: "tok-pf".to_string(),
            trade_id: "trade-pf-3".to_string(),
            order_id: "exchange-pf".to_string(),
            side: "BUY".to_string(),
            price: 0.5,
            size: 30.0,
            timestamp: 0,
            taker_order_id: "exchange-pf".to_string(),
            maker_order_id: String::new(),
            trader_side: "TAKER".to_string(),
            owner: String::new(),
            status: "MATCHED".to_string(),
        };

        service.on_market_trade(&t1).await;
        service.on_market_trade(&t2).await;
        service.on_market_trade(&t3).await;

        // OrderStore filled_size 累加：30 → 70 → 100（三笔不互相覆盖）
        let st = service.order_store.get("client-pf").unwrap();
        assert_eq!(st.filled_size, 100.0, "多笔 partial fill 应累加到满额");
        assert_eq!(st.remaining_size, 0.0);
        assert_eq!(st.status, OrderStateStatus::Filled);
    }

    /// P0-014：事件消费链 poller → MarketEvent::Trade → on_market_trade 接通。
    #[tokio::test]
    async fn test_consume_trade_events_wires_on_market_trade() {
        let (tx, _) = broadcast::channel(16);
        let rx = tx.subscribe();

        let mut service = ExecutionServiceImpl::new_mock();
        service.set_risk_engine(Arc::new(RiskEngine::new(RiskConfig::default())));
        let service = Arc::new(service);

        // spawn consumer
        let svc = service.clone();
        tokio::spawn(consume_trade_events(svc, rx));

        // 发送一笔成交事件
        let trade = TradeEvent {
            token_id: "token-1".to_string(),
            trade_id: "trade-wire".to_string(),
            order_id: "order-1".to_string(),
            side: "BUY".to_string(),
            price: 0.5,
            size: 10.0,
            timestamp: 0,
            taker_order_id: "order-1".to_string(),
            maker_order_id: "maker-1".to_string(),
            trader_side: "TAKER".to_string(),
            owner: "0xowner".to_string(),
            status: "MATCHED".to_string(),
        };
        let _ = tx.send(MarketEvent::Trade(trade));
        drop(tx); // 关闭 sender，让消费循环退出

        // 给 consumer 时间处理
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // 成交被记账
        assert_eq!(service.fill_ledger().list().len(), 1);
        let rs = service.risk_state.lock().await;
        assert_eq!(rs.positions.get("token-1").copied().unwrap_or(0.0), 10.0);
    }

    /// P1-010：下单失败路径（RiskEngine 未初始化）会把 claim 占位标记为 Rejected，
    /// 之后同 order_id 可重试且不进入 pending 对账。
    #[tokio::test]
    async fn test_failed_place_order_marks_rejected() {
        use crate::fast::order_state::OrderStateStatus;
        use crate::fast::trading_gate::TradingGateState;
        use pb::execution_service_server::ExecutionService;

        let mut service = ExecutionServiceImpl::new_mock();
        service.trading_gate.set_state(TradingGateState::Ready);
        // 不挂载 risk_engine → 走「RiskEngine 未初始化」失败路径

        let req = pb::AggregatedOrder {
            order_id: "o-fail".to_string(),
            token_id: "t1".to_string(),
            direction: pb::Direction::BuyYes as i32,
            quantity: 10.0,
            status: pb::OrderStatus::Open as i32,
            target_price: 0.5,
            paper_trading: false,
            ..Default::default()
        };

        let resp = service.place_order(Request::new(req)).await.unwrap();
        let result = resp.into_inner();

        assert_eq!(result.status, pb::ExecutionStatus::Rejected as i32);
        // 占位已被标记 rejected（不再残留 Received）
        assert_eq!(
            service.order_store.get("o-fail").map(|s| s.status),
            Some(OrderStateStatus::Rejected)
        );
        // 且不再进入 pending（对账不误报）
        assert!(!service.order_store.pending_order_ids().contains("o-fail"));
    }

    /// P0-003：超仓订单在 place_order 中被 `check_and_reserve` 拦截，
    /// 不会进入 FastExecutor / 普通下单路径，也不会产生有效订单。
    ///
    /// 关键判别：`new_mock()` 的 `fast_executor` 为 None，若超仓拒单没有
    /// 在 FastExecutor 之前拦截，会落到「FastExecutor 未初始化」的拒绝分支。
    /// 断言 error_message 是「风控预检拦截」而非「FastExecutor 未初始化」，
    /// 即证明拒单发生在执行路径之前（不调 FastExecutor）。
    #[tokio::test]
    async fn test_place_order_over_limit_rejected_before_executor() {
        use crate::fast::order_state::OrderStateStatus;
        use crate::fast::trading_gate::TradingGateState;
        use pb::execution_service_server::ExecutionService;

        let mut service = ExecutionServiceImpl::new_mock();
        service.trading_gate.set_state(TradingGateState::Ready);

        // 挂载一个「单市场上限极小」的风控引擎，并把仓位占满到上限
        let mut cfg = RiskConfig::default();
        cfg.max_position_per_market = 100.0;
        cfg.max_global_exposure = 100_000.0;
        let engine = RiskEngine::new(cfg);
        engine.post_check("t1", Side::Buy, 100.0, 100.0, 0.0, 10_000.0);
        service.set_risk_engine(Arc::new(engine));

        // 下单：qty=50 @ 0.5 → notional=25，100+25=125 > 100 超仓
        let req = pb::AggregatedOrder {
            order_id: "o-over".to_string(),
            token_id: "t1".to_string(),
            direction: pb::Direction::BuyYes as i32,
            quantity: 50.0,
            status: pb::OrderStatus::Open as i32,
            target_price: 0.5,
            paper_trading: false,
            ..Default::default()
        };

        let resp = service.place_order(Request::new(req)).await.unwrap();
        let result = resp.into_inner();

        // 拒单
        assert_eq!(result.status, pb::ExecutionStatus::Rejected as i32);
        assert!(!result.success);
        // 是「风控预检拦截」而非「FastExecutor 未初始化」→ 未进入执行路径
        assert!(
            result.error_message.contains("风控预检拦截"),
            "应被风控拦截，实际错误: {}",
            result.error_message
        );
        // 不产生有效订单：占位被标记 rejected，且不在 pending
        assert_eq!(
            service.order_store.get("o-over").map(|s| s.status),
            Some(OrderStateStatus::Rejected)
        );
        assert!(!service.order_store.pending_order_ids().contains("o-over"));
    }

    /// P0-007B：apply_recovery 应用恢复计划（非成交订单标记终态 + 仓位重建 + 成交补记）。
    #[tokio::test]
    async fn test_apply_recovery_rebuild_and_nonfilled() {
        use crate::fast::order_state::OrderStateStatus;
        use crate::fast::state_sync::RecoveryAction;
        use crate::fast::state_sync::RecoveryPlan;

        let mut service = ExecutionServiceImpl::new_mock();
        service.set_risk_engine(Arc::new(RiskEngine::new(RiskConfig::default())));

        // 先制造一个 pending 订单（Live 状态）
        service
            .order_store
            .create("o-vanish".to_string(), "tok-1".to_string(), "BUY".to_string(), 10.0);
        if let Some(mut st) = service.order_store.get("o-vanish") {
            st.mark_submitted("ex-vanish".to_string());
            st.mark_live();
            service.order_store.upsert(st);
        }
        assert!(service
            .order_store
            .pending_exchange_order_ids()
            .contains("ex-vanish"));

        let plan = RecoveryPlan {
            actions: vec![
                // P0-016：MarkOrderNonFilled 的 order_id 是交易所订单 ID（ex-vanish），
                //    非 client_order_id（o-vanish），与 build_recovery_plan 生产路径一致
                RecoveryAction::MarkOrderNonFilled {
                    order_id: "ex-vanish".to_string(),
                },
                RecoveryAction::RebuildPosition {
                    token_id: "tok-1".to_string(),
                    qty: 25.0,
                    notional: 12.5,
                },
            ],
            unresolved_orders: Vec::new(),
        };

        let applied = service.apply_recovery(&plan).await;
        assert_eq!(applied, 2);

        // 非成交订单 → cancelled，退出 pending
        assert_eq!(
            service.order_store.get("o-vanish").map(|s| s.status),
            Some(OrderStateStatus::Cancelled)
        );
        assert!(!service
            .order_store
            .pending_exchange_order_ids()
            .contains("ex-vanish"));

        // 仓位重建 → risk_state 与 RiskEngine 都更新
        let rs = service.risk_state.lock().await;
        assert_eq!(rs.positions.get("tok-1").copied().unwrap_or(0.0), 25.0);
        drop(rs);
        let re = service.risk_engine.as_ref().unwrap();
        assert_eq!(re.snapshot_positions().get("tok-1"), Some(&25.0));
        assert_eq!(re.snapshot().global_exposure, 12.5);
    }

    /// P1-031：rebuild_open_order_reserves 用 OrderStore 的 token_id + open order 的 price
    /// 重建远端仍 open 的 BUY 订单预留额度。
    #[tokio::test]
    async fn test_rebuild_open_order_reserves_rebuilds_reserve() {
        let mut service = ExecutionServiceImpl::new_mock();
        let mut cfg = RiskConfig::default();
        cfg.max_position_per_market = 100.0;
        cfg.max_global_exposure = 100.0;
        let engine = Arc::new(RiskEngine::new(cfg));
        service.set_risk_engine(engine.clone());

        // 本地 pending BUY 订单：requested 100，已成交 60，remaining 40
        let mut st = service
            .order_store
            .create("client-r".to_string(), "tok-r".to_string(), "BUY".to_string(), 100.0);
        st.mark_submitted("exchange-r".to_string());
        st.update_fill(60.0); // PartialFilled, remaining 40
        service.order_store.upsert(st);

        // 远端 open order JSON：id 匹配 exchange-r，price = 0.5
        let open_orders: Vec<serde_json::Value> = serde_json::from_value(serde_json::json!([
            {"id": "exchange-r", "price": "0.5", "side": "BUY", "original_size": "100", "size_matched": "60"}
        ]))
        .unwrap();

        service.rebuild_open_order_reserves(&open_orders);

        // 重建 reserved = remaining 40 × 0.5 = 20。
        // 再 reserve 85 → 20 + 85 = 105 > 100 → Err（暴露检查计入重建预留）
        assert!(engine
            .check_and_reserve("tok-r", Side::Buy, 85.0, 85.0, 50)
            .is_err());
        // 再 reserve 80 → 20 + 80 = 100 = 上限 → Ok
        assert!(engine
            .check_and_reserve("tok-r", Side::Buy, 80.0, 80.0, 50)
            .is_ok());
    }

    /// P1-031：rebuild_open_order_reserves 对 SELL 订单不重建（减少暴露无需预留）。
    #[tokio::test]
    async fn test_rebuild_open_order_reserves_skips_sell() {
        let mut service = ExecutionServiceImpl::new_mock();
        let mut cfg = RiskConfig::default();
        cfg.max_position_per_market = 100.0;
        cfg.max_global_exposure = 100.0;
        let engine = Arc::new(RiskEngine::new(cfg));
        service.set_risk_engine(engine.clone());

        // 本地 pending SELL 订单
        let mut st = service
            .order_store
            .create("client-s".to_string(), "tok-s".to_string(), "SELL".to_string(), 100.0);
        st.mark_submitted("exchange-s".to_string());
        st.mark_live();
        service.order_store.upsert(st);

        let open_orders: Vec<serde_json::Value> = serde_json::from_value(serde_json::json!([
            {"id": "exchange-s", "price": "0.5", "side": "SELL", "original_size": "100", "size_matched": "0"}
        ]))
        .unwrap();

        service.rebuild_open_order_reserves(&open_orders);

        // SELL 不重建预留 → 仍可满额 reserve 100
        assert!(engine
            .check_and_reserve("tok-s", Side::Buy, 100.0, 100.0, 50)
            .is_ok());
    }

    /// P0-007B：apply_recovery 对 MarkOrderFilled 走 on_market_trade（成交补记 + 幂等）。
    #[tokio::test]
    async fn test_apply_recovery_filled_records_fill() {
        use crate::fast::order_state::OrderStateStatus;
        use crate::fast::state_sync::{RecoveryAction, RecoveryPlan};

        let mut service = ExecutionServiceImpl::new_mock();
        service.set_risk_engine(Arc::new(RiskEngine::new(RiskConfig::default())));

        // 先制造一个已提交的订单（client_order_id 键 + exchange_order_id 字段）
        service
            .order_store
            .create("client-fill".to_string(), "tok-1".to_string(), "BUY".to_string(), 10.0);
        if let Some(mut st) = service.order_store.get("client-fill") {
            st.mark_submitted("exchange-fill".to_string());
            st.mark_live();
            service.order_store.upsert(st);
        }

        let plan = RecoveryPlan {
            actions: vec![RecoveryAction::MarkOrderFilled {
                // P0-016：order_id 是交易所订单 ID（exchange-fill）
                order_id: "exchange-fill".to_string(),
                token_id: "tok-1".to_string(),
                side: "BUY".to_string(),
                price: 0.5,
                size: 10.0,
                trade_id: "trade-recover-1".to_string(),
            }],
            unresolved_orders: Vec::new(),
        };

        let applied = service.apply_recovery(&plan).await;
        assert_eq!(applied, 1);

        // FillLedger 记 1 条
        assert_eq!(service.fill_ledger().list().len(), 1);
        // risk_state 仓位 +10
        let rs = service.risk_state.lock().await;
        assert_eq!(rs.positions.get("tok-1").copied().unwrap_or(0.0), 10.0);
        drop(rs);
        // P0-016：OrderStore 通过 exchange_order_id 反查成功，成交后状态 → Filled
        assert_eq!(
            service.order_store.get("client-fill").map(|s| s.status),
            Some(OrderStateStatus::Filled)
        );
    }
}
