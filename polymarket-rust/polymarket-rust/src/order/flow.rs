//! 基础下单流程
//!
//! 整合钱包管理和 CLOB API，实现完整的端到端下单流程：
//! 1. 创建/派生 API Key（L1 认证）
//! 2. 构建订单（价格/数量 → makerAmount/takerAmount）
//! 3. EIP-712 签名
//! 4. 提交订单（L2 认证）
//! 5. 查询订单状态

use crate::clob::client::{ClobClient, ClobConfig};
use crate::clob::error::{ClobError, ClobResult};
use crate::types::clob_types::{ApiKeyCreds, OrderResponse, UserOrder};
use crate::types::enums::{OrderType, Side, TickSize};
use crate::types::order::CreateOrderOptions;
use crate::wallet::WalletManager;
use std::sync::Arc;

/// 基础下单流程封装
///
/// 提供简化的下单接口，内部处理 API Key 管理和签名
pub struct OrderFlow {
    client: ClobClient,
    wallet_manager: Arc<WalletManager>,
}

/// 下单参数（简化版）
#[derive(Debug, Clone)]
pub struct PlaceOrderParams {
    /// 钱包地址
    pub wallet_address: String,
    /// 钱包密码
    pub password: String,
    /// Token ID
    pub token_id: String,
    /// 价格（0~1）
    pub price: f64,
    /// 数量
    pub size: f64,
    /// 方向
    pub side: Side,
    /// Tick 大小（默认 0.01）
    pub tick_size: Option<TickSize>,
    /// 是否 negRisk（默认 false）
    pub neg_risk: Option<bool>,
    /// 订单类型（默认 GTC）
    pub order_type: Option<OrderType>,
}

impl PlaceOrderParams {
    /// 创建买入参数
    pub fn buy(
        wallet_address: &str,
        password: &str,
        token_id: &str,
        price: f64,
        size: f64,
    ) -> Self {
        Self {
            wallet_address: wallet_address.to_string(),
            password: password.to_string(),
            token_id: token_id.to_string(),
            price,
            size,
            side: Side::Buy,
            tick_size: None,
            neg_risk: None,
            order_type: None,
        }
    }

    /// 创建卖出参数
    pub fn sell(
        wallet_address: &str,
        password: &str,
        token_id: &str,
        price: f64,
        size: f64,
    ) -> Self {
        Self {
            wallet_address: wallet_address.to_string(),
            password: password.to_string(),
            token_id: token_id.to_string(),
            price,
            size,
            side: Side::Sell,
            tick_size: None,
            neg_risk: None,
            order_type: None,
        }
    }
}

impl OrderFlow {
    /// 创建新的下单流程实例
    pub fn new(wallet_manager: Arc<WalletManager>, config: ClobConfig) -> Self {
        let client = ClobClient::new(wallet_manager.clone(), config);
        Self {
            client,
            wallet_manager,
        }
    }

    /// 初始化 API Key
    ///
    /// 如果已存在则派生，否则创建新的
    pub async fn init_api_key(&mut self, address: &str, password: &str) -> ClobResult<ApiKeyCreds> {
        let creds = self
            .client
            .create_or_derive_api_key(address, password)
            .await?;
        self.client.set_api_creds(creds.clone());
        Ok(creds)
    }

    /// 手动设置 API Key（如果已有凭证）
    pub fn set_api_creds(&mut self, creds: ApiKeyCreds) {
        self.client.set_api_creds(creds);
    }

    /// 执行完整的下单流程
    ///
    /// 步骤：
    /// 1. 确保 API Key 已设置（若未设置则创建/派生）
    /// 2. 查询 tickSize（若未提供）
    /// 3. 构建并签名订单
    /// 4. 提交订单
    pub async fn place_order(&mut self, params: PlaceOrderParams) -> ClobResult<OrderResponse> {
        // 1. 确保 API Key
        if !self.client.has_api_creds() {
            self.init_api_key(&params.wallet_address, &params.password)
                .await?;
        }

        // 2. 确定 tickSize
        let tick_size = match params.tick_size {
            Some(t) => t,
            None => {
                // 从 API 获取
                match self.client.get_tick_size(&params.token_id).await {
                    Ok(ts_str) => parse_tick_size(&ts_str).unwrap_or(TickSize::Point01),
                    Err(_) => TickSize::Point01, // 默认
                }
            }
        };

        // 3. 构建订单选项
        let options = CreateOrderOptions {
            tick_size,
            neg_risk: params.neg_risk.unwrap_or(false),
        };

        // 4. 构建 UserOrder
        let user_order = UserOrder {
            token_id: params.token_id.clone(),
            price: params.price,
            size: params.size,
            side: params.side,
            fee_rate_bps: None,
            nonce: None,
            expiration: None,
            taker: None,
        };

        // 5. 提交订单
        let order_type = params.order_type.unwrap_or(OrderType::GTC);
        let response = self
            .client
            .post_limit_order(
                &params.wallet_address,
                &params.password,
                &user_order,
                &options,
                order_type,
            )
            .await?;

        if !response.success {
            return Err(ClobError::OrderFailed(format!(
                "下单失败: {} (status: {})",
                response.errorMsg, response.status
            )));
        }

        Ok(response)
    }

    /// 取消订单
    pub async fn cancel_order(
        &self,
        address: &str,
        password: &str,
        order_id: &str,
    ) -> ClobResult<()> {
        let _ = self
            .client
            .cancel_order(address, password, order_id)
            .await?;
        Ok(())
    }

    /// 取消所有订单
    pub async fn cancel_all(&self, address: &str, password: &str) -> ClobResult<()> {
        let _ = self.client.cancel_all_orders(address, password).await?;
        Ok(())
    }

    /// 获取客户端引用（用于直接调用 API）
    pub fn client(&self) -> &ClobClient {
        &self.client
    }

    /// 获取钱包管理器引用
    pub fn wallet_manager(&self) -> &WalletManager {
        &self.wallet_manager
    }
}

/// 解析 tick size 字符串
fn parse_tick_size(s: &str) -> Option<TickSize> {
    match s.trim() {
        "0.1" => Some(TickSize::Point1),
        "0.01" => Some(TickSize::Point01),
        "0.001" => Some(TickSize::Point001),
        "0.0001" => Some(TickSize::Point0001),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tick_size() {
        assert_eq!(parse_tick_size("0.1"), Some(TickSize::Point1));
        assert_eq!(parse_tick_size("0.01"), Some(TickSize::Point01));
        assert_eq!(parse_tick_size("0.001"), Some(TickSize::Point001));
        assert_eq!(parse_tick_size("0.0001"), Some(TickSize::Point0001));
        assert_eq!(parse_tick_size("invalid"), None);
    }
}
