//! Polymarket CLOB 枚举类型

use serde::{Deserialize, Serialize};

/// 订单方向
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    #[serde(rename = "BUY")]
    Buy,
    #[serde(rename = "SELL")]
    Sell,
}

impl Side {
    /// 转为数值（EIP-712 Order 结构中 side 字段用 uint8）
    pub fn as_u8(&self) -> u8 {
        match self {
            Side::Buy => 0,
            Side::Sell => 1,
        }
    }

    /// 转为大写字符串（用于 API 请求体）
    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        }
    }
}

/// 订单类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    /// Good-Till-Cancelled，挂单直到成交或手动取消
    GTC,
    /// Fill-or-Kill，全部成交或全部取消
    FOK,
    /// Good-Till-Date，有效期到指定时间
    GTD,
    /// Fill-and-Kill（IOC），允许部分成交，剩余取消
    FAK,
}

impl OrderType {
    pub fn as_str(&self) -> &'static str {
        match self {
            OrderType::GTC => "GTC",
            OrderType::FOK => "FOK",
            OrderType::GTD => "GTD",
            OrderType::FAK => "FAK",
        }
    }
}

/// 签名类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
#[derive(Default)]
pub enum SignatureType {
    /// EOA 钱包签名
    #[default]
    Eoa = 0,
    /// Polymarket Proxy 钱包签名
    PolyProxy = 1,
    /// Polymarket Gnosis Safe 签名
    PolyGnosisSafe = 2,
}


/// 链 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    Polygon = 137,
    Amoy = 80002,
}

impl Chain {
    pub fn id(&self) -> u64 {
        *self as u64
    }
}

/// Tick 大小
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickSize {
    Point1,    // 0.1
    Point01,   // 0.01
    Point001,  // 0.001
    Point0001, // 0.0001
}

impl TickSize {
    pub fn as_str(&self) -> &'static str {
        match self {
            TickSize::Point1 => "0.1",
            TickSize::Point01 => "0.01",
            TickSize::Point001 => "0.001",
            TickSize::Point0001 => "0.0001",
        }
    }
}

/// 资产类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetType {
    Collateral,
    Conditional,
}
