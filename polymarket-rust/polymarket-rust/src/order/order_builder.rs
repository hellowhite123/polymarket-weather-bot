//! 订单构建器
//!
//! 将用户友好的订单参数转换为链上可签名的 EIP-712 Order 结构。

use crate::clob::constants::{get_exchange_address, ZERO_ADDRESS};
use crate::clob::error::ClobResult;
use crate::clob::signing::{generate_order_salt, sign_order};
use crate::types::clob_types::{Order, SignedOrder, UserOrder};
use crate::types::enums::{Side, SignatureType};
use crate::types::order::{round_config_for, CreateOrderOptions, RoundConfig, COLLATERAL_DECIMALS};
use ethers::signers::{LocalWallet, Signer};
use ethers::utils::parse_units;

/// 订单构建器，负责将用户订单转换为已签名的链上订单
pub struct OrderBuilder {
    chain_id: u64,
    signature_type: SignatureType,
    funder_address: Option<String>,
}

impl OrderBuilder {
    /// 创建订单构建器
    pub fn new(
        chain_id: u64,
        signature_type: SignatureType,
        funder_address: Option<String>,
    ) -> Self {
        Self {
            chain_id,
            signature_type,
            funder_address,
        }
    }

    /// 构建并签名限价单
    pub fn build_limit_order(
        &self,
        wallet: &LocalWallet,
        user_order: &UserOrder,
        options: &CreateOrderOptions,
    ) -> ClobResult<SignedOrder> {
        let round_config = round_config_for(options.tick_size);

        let (side_u8, raw_maker_amt, raw_taker_amt) = get_order_raw_amounts(
            user_order.side,
            user_order.size,
            user_order.price,
            round_config,
        );

        let maker_amount = to_wei_string(raw_maker_amt, COLLATERAL_DECIMALS);
        let taker_amount = to_wei_string(raw_taker_amt, COLLATERAL_DECIMALS);

        let signer_address = format!("{:?}", wallet.address());
        let maker = self
            .funder_address
            .clone()
            .unwrap_or_else(|| signer_address.clone());

        let taker = user_order
            .taker
            .clone()
            .unwrap_or_else(|| ZERO_ADDRESS.to_string());

        let fee_rate_bps = user_order.fee_rate_bps.unwrap_or(0).to_string();
        let nonce = user_order.nonce.unwrap_or(0).to_string();
        let expiration = user_order.expiration.unwrap_or(0).to_string();

        let salt = generate_order_salt();

        let order = Order {
            salt,
            maker,
            signer: signer_address,
            taker,
            token_id: user_order.token_id.clone(),
            maker_amount,
            taker_amount,
            expiration,
            nonce,
            fee_rate_bps,
            side: side_u8,
            signature_type: self.signature_type as u8,
        };

        let verifying_contract = get_exchange_address(
            crate::types::enums::Chain::Polygon, // 简化：使用配置的 chain
            options.neg_risk,
        );
        let signature = sign_order(wallet, &order, self.chain_id, verifying_contract)?;

        Ok(SignedOrder { order, signature })
    }
}

/// 计算限价单的原始金额
fn get_order_raw_amounts(side: Side, size: f64, price: f64, config: RoundConfig) -> (u8, f64, f64) {
    let raw_price = round_normal(price, config.price);

    match side {
        Side::Buy => {
            let raw_taker_amt = round_down(size, config.size);
            let mut raw_maker_amt = raw_taker_amt * raw_price;
            if decimal_places(raw_maker_amt) > config.amount {
                raw_maker_amt = round_up(raw_maker_amt, config.amount + 4);
                if decimal_places(raw_maker_amt) > config.amount {
                    raw_maker_amt = round_down(raw_maker_amt, config.amount);
                }
            }
            (0, raw_maker_amt, raw_taker_amt)
        }
        Side::Sell => {
            let raw_maker_amt = round_down(size, config.size);
            let mut raw_taker_amt = raw_maker_amt * raw_price;
            if decimal_places(raw_taker_amt) > config.amount {
                raw_taker_amt = round_up(raw_taker_amt, config.amount + 4);
                if decimal_places(raw_taker_amt) > config.amount {
                    raw_taker_amt = round_down(raw_taker_amt, config.amount);
                }
            }
            (1, raw_maker_amt, raw_taker_amt)
        }
    }
}

/// 将浮点数转为指定 decimals 的 uint256 字符串
fn to_wei_string(value: f64, decimals: u32) -> String {
    let formatted = format!("{:.10}", value);
    match parse_units(&formatted, decimals as u32) {
        Ok(u) => u.to_string(),
        Err(_) => {
            let multiplier = 10f64.powi(decimals as i32);
            let scaled = (value * multiplier).round();
            scaled.to_string()
        }
    }
}

fn round_normal(value: f64, decimals: u32) -> f64 {
    let factor = 10f64.powi(decimals as i32);
    (value * factor).round() / factor
}

fn round_down(value: f64, decimals: u32) -> f64 {
    let factor = 10f64.powi(decimals as i32);
    (value * factor).floor() / factor
}

fn round_up(value: f64, decimals: u32) -> f64 {
    let factor = 10f64.powi(decimals as i32);
    (value * factor).ceil() / factor
}

fn decimal_places(value: f64) -> u32 {
    let s = format!("{:.10}", value);
    if let Some(dot) = s.find('.') {
        let after_dot = &s[dot + 1..];
        let trimmed = after_dot.trim_end_matches('0');
        trimmed.len() as u32
    } else {
        0
    }
}
