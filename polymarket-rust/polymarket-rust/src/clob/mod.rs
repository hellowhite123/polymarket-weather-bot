//! CLOB API 封装模块入口
//!
//! 提供对 Polymarket Central Limit Order Book (CLOB) API 的完整封装，
//! 包括 L1/L2 认证、EIP-712 订单签名、市场数据查询和订单管理。

pub mod auth;
pub mod client;
pub mod constants;
pub mod endpoints;
pub mod error;
pub mod signing;

pub use auth::{create_l1_headers, create_l2_headers, L2HeaderArgs};
pub use client::{ClobClient, ClobConfig};
pub use constants::*;
pub use endpoints::*;
pub use error::{ClobError, ClobResult};
