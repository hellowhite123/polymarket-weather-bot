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
use crate::order::{OrderFlow, PlaceOrderParams};
use crate::types::enums::{OrderType, Side};
use crate::wallet::WalletManager;

use super::polymarket as pb;

/// 方向映射: proto Direction -> Rust Side
///
/// BUY_YES: 买入 YES token -> Side::Buy
/// BUY_NO:  卖出 YES token (等价于做多 NO) -> Side::Sell
///
/// 注意: 实际 Polymarket 中 BUY_NO 应该是对 NO token 做 Side::Buy。
/// 此处假设 token_id 已经指向正确的 outcome token, direction 只决定买卖。
/// 待接入真实市场数据后可能需要调整。
fn direction_to_side(direction: i32) -> Result<Side, Status> {
    match direction {
        x if x == pb::Direction::BuyYes as i32 => Ok(Side::Buy),
        x if x == pb::Direction::BuyNo as i32 => Ok(Side::Sell),
        _ => Err(Status::invalid_argument(format!(
            "未知方向: {}",
            direction
        ))),
    }
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
        }
    }

    /// 挂载亚秒级执行器。此后 `place_order` 优先走 fast path。
    ///
    /// 注意：应在挂载前对 `FastExecutor` 调用过 `init_api_key`（预热钱包/
    /// 连接/域分隔符），否则首笔单会付出一次 PBKDF2 代价。
    pub fn set_fast_executor(&mut self, fe: Arc<FastExecutor>) {
        self.fast_executor = Some(fe);
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
            }));
        }

        // ★ 纸面交易模式：不真正下单，返回模拟成交
        if order.paper_trading {
            tracing::info!(
                "[PAPER] 纸面下单: order_id={}, token={}, price={}, qty={}, conf={:.2}",
                order.order_id, order.token_id, order.target_price,
                order.quantity, order.confidence,
            );
            return Ok(Response::new(pb::OrderResult {
                order_id: order.order_id.clone(),
                success: true,
                exchange_order_id: format!("paper_{}", order.order_id),
                error_message: String::new(),
                timestamp: Utc::now().to_rfc3339(),
                filled_price: order.target_price,
                filled_size: order.quantity,
                status: pb::ExecutionStatus::Filled as i32,
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
                }));
            }
        }

        // 方向转换
        let side = direction_to_side(order.direction)?;

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
                    // 风控：更新持仓
                    {
                        let mut rs = self.risk_state.lock().await;
                        *rs.positions.entry(order.token_id.clone()).or_insert(0.0) += order.quantity;
                    }
                    return Ok(Response::new(pb::OrderResult {
                        order_id: order.order_id.clone(),
                        success,
                        exchange_order_id,
                        error_message,
                        timestamp: Utc::now().to_rfc3339(),
                        filled_price: order.target_price,
                        filled_size: order.quantity,
                        status: if success {
                            pb::ExecutionStatus::Filled as i32
                        } else {
                            pb::ExecutionStatus::Rejected as i32
                        },
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
                // 更新风控状态
                {
                    let mut rs = self.risk_state.lock().await;
                    // OrderResponse 没有 filled_size, 用请求数量近似
                    *rs.positions.entry(order.token_id.clone()).or_insert(0.0) += order.quantity;
                }

                // 从 OrderResponse 提取成交信息
                // makingAmount / takingAmount 是字符串形式的大数
                let filled_price = if !response.makingAmount.is_empty() && !response.takingAmount.is_empty() {
                    response.makingAmount.parse::<f64>().unwrap_or(0.0)
                        / response.takingAmount.parse::<f64>().unwrap_or(1.0)
                } else {
                    order.target_price
                };

                Ok(Response::new(pb::OrderResult {
                    order_id: order.order_id.clone(),
                    success: response.success,
                    exchange_order_id: response.orderID.clone(),
                    error_message: response.errorMsg.clone(),
                    timestamp: Utc::now().to_rfc3339(),
                    filled_price,
                    filled_size: order.quantity,
                    status: if response.success {
                        pb::ExecutionStatus::Filled as i32
                    } else {
                        pb::ExecutionStatus::Rejected as i32
                    },
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

    /// 添加钱包（导入私钥）。
    async fn add_wallet(
        &self,
        request: Request<pb::AddWalletRequest>,
    ) -> Result<Response<pb::WalletResult>, Status> {
        let req = request.into_inner();
        match self.wallet_manager.import_private_key(&req.private_key, &req.password) {
            Ok(address) => {
                let _ = self.wallet_manager.save();
                tracing::info!("新增钱包: {}", address);
                Ok(Response::new(pb::WalletResult {
                    success: true,
                    address,
                    message: "钱包已添加".to_string(),
                }))
            }
            Err(e) => Ok(Response::new(pb::WalletResult {
                success: false,
                address: String::new(),
                message: format!("添加钱包失败: {}", e),
            })),
        }
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
