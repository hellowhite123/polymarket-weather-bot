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

    // ClobAuth 类型
    let type_str = "ClobAuth(address address,string timestamp,uint256 nonce,string message)";
    let type_hash = keccak256(type_str);

    // address padding
    let mut address_padded = [0u8; 32];
    address_padded[12..].copy_from_slice(address.as_bytes());

    // timestamp hash
    let timestamp_hash = keccak256(timestamp_str.as_bytes());

    // message hash
    let message_hash = keccak256(MSG_TO_SIGN.as_bytes());

    let nonce_u256 = U256::from(nonce);

    // struct hash
    let mut struct_input = Vec::with_capacity(32 * 5);

    struct_input.extend_from_slice(&type_hash);
    struct_input.extend_from_slice(&address_padded);
    struct_input.extend_from_slice(&timestamp_hash);
    struct_input.extend_from_slice(&u256_to_bytes(&nonce_u256));
    struct_input.extend_from_slice(&message_hash);

    let struct_hash = keccak256(&struct_input);

    // EIP712 Domain
    let domain_type_str = "EIP712Domain(string name,string version,uint256 chainId)";

    let domain_type_hash = keccak256(domain_type_str);

    let name_hash = keccak256(b"ClobAuthDomain");

    let version_hash = keccak256(b"1");

    let chain_id = U256::from(137u64);

    let verifying_contract = [0u8; 32]; // 官方 verifying_contract 为 None → 编码为 32 字节 0

    let mut domain_input = Vec::with_capacity(32 * 5);

    domain_input.extend_from_slice(&domain_type_hash);
    domain_input.extend_from_slice(&name_hash);
    domain_input.extend_from_slice(&version_hash);
    domain_input.extend_from_slice(&u256_to_bytes(&chain_id));

    let domain_separator = keccak256(&domain_input);

    // EIP712 final hash
    let mut final_input = Vec::with_capacity(66);

    final_input.extend_from_slice(b"\x19\x01");
    final_input.extend_from_slice(&domain_separator);
    final_input.extend_from_slice(&struct_hash);

    let typed_data_hash = keccak256(&final_input);

    let sig = wallet.sign_hash(H256::from(typed_data_hash))?;

    Ok(format!("0x{}", sig))
}
