//! L1 认证签名（EIP-712 ClobAuth）
//!
//! L1 认证用于创建或派生 API Key。通过对 ClobAuth 类型的
//! EIP-712 结构化数据进行签名来证明钱包控制权。
//!
//! ClobAuth 类型定义：
//! ```text
//! ClobAuth(address address, string timestamp, uint256 nonce, string message)
//! ```

use crate::clob::constants::MSG_TO_SIGN;
use crate::clob::error::ClobResult;
use crate::clob::signing::eip712_order::u256_to_bytes;
use ethers::signers::{LocalWallet, Signer};
use ethers::types::{H256, U256};
use ethers::utils::keccak256;

/// 构建 L1 EIP-712 签名
///
/// # 参数
/// - `wallet`: 已解锁的以太坊钱包
/// - `timestamp`: Unix 时间戳（秒）
/// - `nonce`: 随机数（默认 0）
///
/// # 返回
/// 0x 开头的 65 字节签名
pub fn build_l1_signature(wallet: &LocalWallet, timestamp: u64, nonce: u64) -> ClobResult<String> {
    let address = wallet.address();
    let timestamp_str = timestamp.to_string();

    // 计算 ClobAuth 类型哈希
    let type_str = "ClobAuth(address address,string timestamp,uint256 nonce,string message)";
    let type_hash = keccak256(type_str);

    // 计算各字段的 hash
    // address 是定长，直接 pad 到 32 字节
    let mut address_padded = [0u8; 32];
    address_padded[12..].copy_from_slice(address.as_bytes());

    // string 的 hash: keccak256(abi.encode(string))
    let timestamp_hash = keccak256(&ethers::abi::encode(&[ethers::abi::Token::String(
        timestamp_str.clone(),
    )]));
    let nonce_u256 = U256::from(nonce);
    let message_hash = keccak256(&ethers::abi::encode(&[ethers::abi::Token::String(
        MSG_TO_SIGN.to_string(),
    )]));

    // struct hash = keccak256(typeHash + address + timestampHash + nonce + messageHash)
    let mut struct_input = Vec::with_capacity(32 * 5);
    struct_input.extend_from_slice(&type_hash);
    struct_input.extend_from_slice(&address_padded);
    struct_input.extend_from_slice(&timestamp_hash);
    struct_input.extend_from_slice(&u256_to_bytes(&nonce_u256));
    struct_input.extend_from_slice(&message_hash);
    let struct_hash = keccak256(&struct_input);

    // 域分隔符
    let domain_type_str = "EIP712Domain(string name,string version,uint256 chainId)";
    let domain_type_hash = keccak256(domain_type_str);

    let name_hash = keccak256(&ethers::abi::encode(&[ethers::abi::Token::String(
        "ClobAuthDomain".to_string(),
    )]));
    let version_hash = keccak256(&ethers::abi::encode(&[ethers::abi::Token::String(
        "1".to_string(),
    )]));
    let chain_id = U256::from(1u64);

    let mut domain_input = Vec::with_capacity(32 * 4);
    domain_input.extend_from_slice(&domain_type_hash);
    domain_input.extend_from_slice(&name_hash);
    domain_input.extend_from_slice(&version_hash);
    domain_input.extend_from_slice(&u256_to_bytes(&chain_id));
    let domain_separator = keccak256(&domain_input);

    // 最终 typed data hash: keccak256("\x19\x01" + domainSeparator + structHash)
    let mut final_input = Vec::with_capacity(2 + 32 + 32);
    final_input.extend_from_slice(b"\x19\x01");
    final_input.extend_from_slice(&domain_separator);
    final_input.extend_from_slice(&struct_hash);
    let typed_data_hash = keccak256(&final_input);

    let h256 = H256::from(typed_data_hash);
    let sig = wallet.sign_hash(h256)?;
    Ok(format!("0x{}", sig))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_l1_signature_generation() {
        let wallet: LocalWallet =
            "bd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b"
                .parse()
                .unwrap();
        let sig = build_l1_signature(&wallet, 1234567890, 0).unwrap();
        assert!(sig.starts_with("0x"));
        // 签名应为 132 字符（0x + 64+64+2）
        assert_eq!(sig.len(), 132);
    }
}
