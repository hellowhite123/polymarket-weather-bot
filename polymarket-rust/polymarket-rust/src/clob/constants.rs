//! Polymarket CLOB 常量
//!
//! 包含基础 URL、链 ID、合约地址和 EIP-712 域信息。

use crate::types::enums::Chain;

/// CLOB 生产环境主机
pub const CLOB_HOST: &str = "https://clob.polymarket.com";

/// Gamma API（市场元数据）
pub const GAMMA_HOST: &str = "https://gamma-api.polymarket.com";

/// 协议名称
pub const PROTOCOL_NAME: &str = "Polymarket CTF Exchange";
/// 协议版本
pub const PROTOCOL_VERSION: &str = "1";
/// 零地址
pub const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

/// ClobAuth 消息
pub const MSG_TO_SIGN: &str = "This message attests that I control the given wallet";

/// Polygon 主网合约配置
pub struct ContractConfig {
    pub exchange: &'static str,
    pub neg_risk_adapter: &'static str,
    pub neg_risk_exchange: &'static str,
    pub collateral: &'static str,
    pub conditional_tokens: &'static str,
}

/// Polygon (chainId=137) 合约地址
pub const MATIC_CONTRACTS: ContractConfig = ContractConfig {
    exchange: "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E",
    neg_risk_adapter: "0xd91E80cF2E7be2e162c6513ceD06f1dD0dA35296",
    neg_risk_exchange: "0xC5d563A36AE78145C45a50134d48A1215220f80a",
    collateral: "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174",
    conditional_tokens: "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045",
};

/// Amoy 测试网合约配置
pub const AMOY_CONTRACTS: ContractConfig = ContractConfig {
    exchange: "0xdFE02Eb6733538f8Ea35D585af8DE5958AD99E40",
    neg_risk_adapter: "0xd91E80cF2E7be2e162c6513ceD06f1dD0dA35296",
    neg_risk_exchange: "0xC5d563A36AE78145C45a50134d48A1215220f80a",
    collateral: "0x9c4e1703476e875070ee25b56a58b008cfb8fa78",
    conditional_tokens: "0x69308FB512518e39F9b16112fA8d994F4e2Bf8bB",
};

/// 根据 chainId 获取合约配置
pub fn get_contract_config(chain: Chain) -> &'static ContractConfig {
    match chain {
        Chain::Polygon => &MATIC_CONTRACTS,
        Chain::Amoy => &AMOY_CONTRACTS,
    }
}

/// 根据是否 negRisk 返回对应的 exchange 合约地址
pub fn get_exchange_address(chain: Chain, neg_risk: bool) -> &'static str {
    let config = get_contract_config(chain);
    if neg_risk {
        config.neg_risk_exchange
    } else {
        config.exchange
    }
}
