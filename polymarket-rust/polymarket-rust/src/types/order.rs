//! 订单构建相关类型

use crate::types::enums::{Side, SignatureType, TickSize};

/// 创建订单的选项
#[derive(Debug, Clone)]
pub struct CreateOrderOptions {
    pub tick_size: TickSize,
    pub neg_risk: bool,
}

impl Default for CreateOrderOptions {
    fn default() -> Self {
        Self {
            tick_size: TickSize::Point01,
            neg_risk: false,
        }
    }
}

/// 订单创建参数（内部用，传递给签名构建器）
#[derive(Debug, Clone)]
pub struct OrderData {
    pub maker: String,
    pub taker: String,
    pub token_id: String,
    pub maker_amount: String, // uint256 字符串
    pub taker_amount: String, // uint256 字符串
    pub side: Side,
    pub fee_rate_bps: String,
    pub nonce: String,
    pub signer: String,
    pub expiration: String,
    pub signature_type: SignatureType,
}

/// 舍入配置（不同 tickSize 对应不同的小数位数）
#[derive(Debug, Clone, Copy)]
pub struct RoundConfig {
    pub price: u32,
    pub size: u32,
    pub amount: u32,
}

/// 根据 tickSize 获取舍入配置
pub fn round_config_for(tick: TickSize) -> RoundConfig {
    match tick {
        TickSize::Point1 => RoundConfig {
            price: 1,
            size: 2,
            amount: 3,
        },
        TickSize::Point01 => RoundConfig {
            price: 2,
            size: 2,
            amount: 4,
        },
        TickSize::Point001 => RoundConfig {
            price: 3,
            size: 2,
            amount: 5,
        },
        TickSize::Point0001 => RoundConfig {
            price: 4,
            size: 2,
            amount: 6,
        },
    }
}

/// USDC（抵押品）的小数位数
pub const COLLATERAL_DECIMALS: u32 = 6;
