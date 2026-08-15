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
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

use crate::clob::client::ClobConfig;
use crate::fast::executor::FastExecutor;
use crate::fast::fill_records::{compute_slippage, FillLedger, FillRecord};
use crate::fast::order_state::{OrderState, OrderStore};
use crate::fast::risk_engine::{RiskConfig, RiskEngine};
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
        _ => Err(Status::invalid_argument(format!(
            "未知方向: {}",
            direction
        ))),
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
fn filled_size_from_status(status: &str, requested: f64, side: Side, making: &str, taking: &str) -> f64 {
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
            risk_engine: None,
        }
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
}

#[tonic::async_trait]
impl pb::execution_service_server::ExecutionService for ExecutionServiceImpl {
    /// 下单: 接收 AggregatedOrder, 执行下单, 返回 OrderResult。
    async fn place_order(
        &self,
        request: Request<pb::AggregatedOrder>,
    ) -> Result<Response<pb::OrderResult>, Status> {
        let order = request.into_inner();

        // ★ R2-P0-06 幂等：order_id 已存在则返回已有状态，不重复下单
        if !order.order_id.is_empty() {
            if let Some(existing) = self.order_store.get(&order.order_id) {
                tracing::info!(
                    "order_id={} 已存在（幂等去重），返回已有状态 {}",
                    order.order_id, existing.status
                );
                let status = match existing.status {
                    crate::fast::order_state::OrderStateStatus::Filled => pb::ExecutionStatus::Filled as i32,
                    crate::fast::order_state::OrderStateStatus::PartialFilled => pb::ExecutionStatus::Partial as i32,
                    crate::fast::order_state::OrderStateStatus::Live => pb::ExecutionStatus::Live as i32,
                    crate::fast::order_state::OrderStateStatus::Rejected => pb::ExecutionStatus::Rejected as i32,
                    crate::fast::order_state::OrderStateStatus::Cancelled => pb::ExecutionStatus::Cancelled as i32,
                    _ => pb::ExecutionStatus::Pending as i32,
                };
                return Ok(Response::new(pb::OrderResult {
                    order_id: order.order_id.clone(),
                    success: true,
                    exchange_order_id: existing.exchange_order_id.unwrap_or_default(),
                    error_message: String::new(),
                    timestamp: Utc::now().to_rfc3339(),
                    filled_price: 0.0,
                    filled_size: existing.filled_size,
                    status,
                    ..Default::default()
                }));
            }
        }

        // 只执行 OPEN 状态的指令
        if order.status != pb::OrderStatus::Open as i32 {
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
                order.order_id, order.token_id, order.target_price,
                order.quantity, order.confidence,
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

        // 方向转换
        let side = direction_to_side(order.direction)?;

        // ★ R2-P0-08 风控：RiskEngine pre_check 强制接入（熔断/仓位硬限制/时间窗口等）
        if let Some(ref re) = self.risk_engine {
            let notional = order.quantity * order.target_price;
            if let Err(e) = re.pre_check(&order.token_id, side, notional, order.quantity, 0) {
                tracing::warn!("风控预检拦截: token={}, {}", order.token_id, e);
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
                        &result.response.status, order.quantity, side,
                        &result.response.makingAmount, &result.response.takingAmount,
                    );
                    let filled_price = derive_price(
                        side, &result.response.makingAmount, &result.response.takingAmount,
                    ).unwrap_or(order.target_price);
                    let (slippage, slippage_pct) = compute_slippage(order.target_price, filled_price, filled_size);

                    // 记录订单状态（R2-P0-05 状态机）
                    if !order.order_id.is_empty() {
                        let mut st = self.order_store
                            .get(&order.order_id)
                            .unwrap_or_else(|| self.order_store.create(
                                order.order_id.clone(),
                                order.token_id.clone(),
                                side.as_str().to_string(),
                                order.quantity,
                            ));
                        if !exchange_order_id.is_empty() {
                            st.mark_submitted(exchange_order_id.clone());
                        }
                        if filled_size > 0.0 {
                            st.update_fill(filled_size);
                        } else if st.status == crate::fast::order_state::OrderStateStatus::Submitted {
                            st.mark_live();
                        }
                        self.order_store.upsert(st);
                    }

                    // 风控：只有真实 fill 才更新持仓（P0-03/04）
                    if filled_size > 0.0 {
                        let mut rs = self.risk_state.lock().await;
                        *rs.positions.entry(order.token_id.clone()).or_insert(0.0) += filled_size;
                    }

                    // 记录成交：只有真实 fill 才记（P0-04）
                    if filled_size > 0.0 && !exchange_order_id.is_empty() {
                        let mut rec = FillRecord::new(
                            exchange_order_id.clone(),
                            order.token_id.clone(),
                            side.as_str().to_string(),
                            order.target_price,
                            filled_size,
                        );
                        rec.mark_filled(filled_price, 0.0);
                        self.fill_ledger.record(rec);
                    }

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

        tracing::info!(
            "收到下单指令: order_id={}, token={}, direction={:?}, price={}, qty={}",
            order.order_id,
            order.token_id,
            pb::Direction::try_from(order.direction).unwrap_or(pb::Direction::Unspecified),
            order.target_price,
            order.quantity,
        );

        // 构建 PlaceOrderParams
        let params = PlaceOrderParams {
            wallet_address: self.wallet_address.clone(),
            password: self.password.clone(),
            token_id: order.token_id.clone(),
            price: order.target_price,
            size: order.quantity,
            side,
            tick_size: None,     // 从 API 获取
            neg_risk: None,      // 默认 false
            order_type: Some(OrderType::GTC),  // 默认 GTC
        };

        // 执行下单
        let mut flow = self.order_flow.lock().await;
        match flow.place_order(params).await {
            Ok(response) => {
                // 状态机：accepted ≠ filled（P0-03）
                let execution_status = map_execution_status(&response.status, response.success);
                // R2-P0-03/21：从 making/taking 按 side 推导真实 fill 与成交价
                let filled_size = filled_size_from_status(
                    &response.status, order.quantity, side,
                    &response.makingAmount, &response.takingAmount,
                );
                let filled_price = derive_price(
                    side, &response.makingAmount, &response.takingAmount,
                ).unwrap_or(order.target_price);

                // 滑点计算
                let (slippage, slippage_pct) = compute_slippage(order.target_price, filled_price, filled_size);

                // 记录订单状态（R2-P0-05 状态机）
                if !order.order_id.is_empty() {
                    let mut st = self.order_store
                        .get(&order.order_id)
                        .unwrap_or_else(|| self.order_store.create(
                            order.order_id.clone(),
                            order.token_id.clone(),
                            side.as_str().to_string(),
                            order.quantity,
                        ));
                    if !response.orderID.is_empty() {
                        st.mark_submitted(response.orderID.clone());
                    }
                    if filled_size > 0.0 {
                        st.update_fill(filled_size);
                    } else if st.status == crate::fast::order_state::OrderStateStatus::Submitted {
                        st.mark_live();
                    }
                    self.order_store.upsert(st);
                }

                // 风控：只有真实 fill 才更新持仓（P0-03/04）
                if filled_size > 0.0 {
                    let mut rs = self.risk_state.lock().await;
                    *rs.positions.entry(order.token_id.clone()).or_insert(0.0) += filled_size;
                }

                // 记录成交：只有真实 fill 才记（P0-04）
                if filled_size > 0.0 && !response.orderID.is_empty() {
                    let mut rec = FillRecord::new(
                        response.orderID.clone(),
                        order.token_id.clone(),
                        side.as_str().to_string(),
                        order.target_price,
                        filled_size,
                    );
                    rec.mark_filled(filled_price, 0.0);
                    self.fill_ledger.record(rec);
                }

                Ok(Response::new(pb::OrderResult {
                    order_id: order.order_id.clone(),
                    success: response.success,
                    exchange_order_id: response.orderID.clone(),
                    error_message: response.errorMsg.clone(),
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
                }))
            }
            Err(e) => {
                tracing::error!("下单失败: {}", e);
                Ok(Response::new(pb::OrderResult {
                    order_id: order.order_id.clone(),
                    success: false,
                    exchange_order_id: String::new(),
                    error_message: format!("下单失败: {}", e),
                    timestamp: Utc::now().to_rfc3339(),
                    filled_price: 0.0,
                    filled_size: 0.0,
                    status: pb::ExecutionStatus::Rejected as i32,
                    ..Default::default()
                }))
            }
        }
    }

    /// 批量下单。
    async fn place_orders(
        &self,
        request: Request<pb::PlaceOrdersRequest>,
    ) -> Result<Response<pb::PlaceOrdersResponse>, Status> {
        let orders = request.into_inner().orders;
        let mut results = Vec::with_capacity(orders.len());

        for order in orders {
            let result = self
                .place_order(Request::new(order))
                .await?
                .into_inner();
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
    async fn cancel_all(
        &self,
        _request: Request<pb::CancelAllRequest>,
    ) -> Result<Response<pb::CancelResult>, Status> {
        let flow = self.order_flow.lock().await;

        match flow.cancel_all(&self.wallet_address, &self.password).await {
            Ok(()) => {
                let count = {
                    let mut rs = self.risk_state.lock().await;
                    let n = rs.positions.len() as i32;
                    rs.positions.clear();
                    n
                };
                Ok(Response::new(pb::CancelResult {
                    success: true,
                    message: format!("已取消 {} 个订单", count),
                    cancelled_count: count,
                }))
            }
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
    type StreamRiskStateUpdatesStream =
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<pb::RiskStateUpdate, Status>> + Send>>;

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
        assert_eq!(filled_size_from_status("live", 10.0, Side::Buy, "", ""), 0.0);
        assert_eq!(filled_size_from_status("pending", 10.0, Side::Buy, "", ""), 0.0);
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
}
