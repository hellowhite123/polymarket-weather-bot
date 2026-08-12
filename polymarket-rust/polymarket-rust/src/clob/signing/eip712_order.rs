//! EIP-712 订单签名
//!
//! Polymarket 订单使用 EIP-712 类型化数据签名。
//! Order 结构定义：
//! ```text
//! Order(
//!     uint256 salt,
//!     address maker,
//!     address signer,
//!     address taker,
//!     uint256 tokenId,
//!     uint256 makerAmount,
//!     uint256 takerAmount,
//!     uint256 expiration,
//!     uint256 nonce,
//!     uint256 feeRateBps,
//!     uint8 side,
//!     uint8 signatureType
//! )
//! ```

use crate::clob::error::ClobResult;
use crate::types::clob_types::Order;
use ethers::signers::LocalWallet;
use ethers::types::{H256, U256};

// 类型字符串的唯一定义源在 signing_cache，这里重导出以保持 API 兼容
pub use crate::fast::signing_cache::{DOMAIN_TYPE_STR, ORDER_TYPE_STR};

/// 生成随机 salt（uint256）
pub fn generate_order_salt() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let salt: [u8; 32] = rng.gen();
    // 转为 U256 再格式化为十进制字符串
    let salt_u256 = U256::from_big_endian(&salt);
    salt_u256.to_string()
}

/// 计算 Order 的 EIP-712 typed data hash
///
/// # 实现说明
///
/// 本函数已改为委托给 [`crate::fast::signing_cache`]，原因有二：
///
/// 1. **正确性修复**：旧实现用 `keccak256(abi_encode(Token::String(name)))`
///    计算域中 `name` / `version` 的哈希。ABI 编码会引入
///    `offset || length || padded_data` 前缀，而 EIP-712 规范明确要求
///    `string` 直接对其 UTF-8 字节取 keccak256。旧实现算出的
///    domain separator 与交易所侧不一致，签名会被判为无效而拒单。
/// 2. **性能**：域分隔符只取决于 `(chain_id, verifying_contract)`，
///    现已预计算并进程内缓存；struct hash 改用栈上定长缓冲，
///    消除原本的两次 `Vec` 堆分配。
pub fn order_typed_data_hash(order: &Order, chain_id: u64, verifying_contract: &str) -> H256 {
    crate::fast::signing_cache::fast_order_digest(order, chain_id, verifying_contract)
}

/// 对 Order 进行 EIP-712 签名
///
/// 返回 0x 开头的 65 字节签名
pub fn sign_order(
    wallet: &LocalWallet,
    order: &Order,
    chain_id: u64,
    verifying_contract: &str,
) -> ClobResult<String> {
    let hash = order_typed_data_hash(order, chain_id, verifying_contract);
    let sig = wallet.sign_hash(hash)?;
    Ok(format!("0x{}", sig))
}

/// 将地址字符串转为 32 字节（左侧补零）
pub fn parse_address(addr: &str) -> [u8; 32] {
    let mut padded = [0u8; 32];
    padded[12..].copy_from_slice(&crate::fast::signing_cache::parse_address_20(addr));
    padded
}

/// 将十进制（或 0x 十六进制）字符串转为 U256
pub fn parse_uint256(s: &str) -> U256 {
    crate::fast::signing_cache::parse_uint256(s)
}

/// 将 U256 转为 32 字节大端字节数组
pub fn u256_to_bytes(u: &U256) -> [u8; 32] {
    let mut buf = [0u8; 32];
    u.to_big_endian(&mut buf);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers::signers::Signer;

    #[test]
    fn test_salt_generation() {
        let salt1 = generate_order_salt();
        let salt2 = generate_order_salt();
        assert_ne!(salt1, salt2);
        // 应该能被 parse_uint256 正确解析
        let _ = parse_uint256(&salt1);
    }

    #[test]
    fn test_order_hash_deterministic() {
        let order = Order {
            salt: "1234567890".to_string(),
            maker: "0x0000000000000000000000000000000000000000".to_string(),
            signer: "0x0000000000000000000000000000000000000000".to_string(),
            taker: "0x0000000000000000000000000000000000000000".to_string(),
            token_id: "12345".to_string(),
            maker_amount: "1000000".to_string(),
            taker_amount: "500000".to_string(),
            expiration: "0".to_string(),
            nonce: "0".to_string(),
            fee_rate_bps: "0".to_string(),
            side: 0,
            signature_type: 0,
        };
        let hash1 = order_typed_data_hash(&order, 137, "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E");
        let hash2 = order_typed_data_hash(&order, 137, "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_order_signing() {
        let wallet: LocalWallet =
            "bd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b"
                .parse()
                .unwrap();
        let order = Order {
            salt: "1234567890".to_string(),
            maker: format!("{:?}", wallet.address()),
            signer: format!("{:?}", wallet.address()),
            taker: "0x0000000000000000000000000000000000000000".to_string(),
            token_id: "12345".to_string(),
            maker_amount: "1000000".to_string(),
            taker_amount: "500000".to_string(),
            expiration: "0".to_string(),
            nonce: "0".to_string(),
            fee_rate_bps: "0".to_string(),
            side: 0,
            signature_type: 0,
        };
        let sig = sign_order(&wallet, &order, 137, "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E").unwrap();
        assert!(sig.starts_with("0x"));
        assert_eq!(sig.len(), 132);
    }
}
