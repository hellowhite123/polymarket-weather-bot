//! 认证模块
//!
//! 构建 L1 和 L2 认证请求头。
//! - L1：使用 EIP-712 ClobAuth 签名，用于 API Key 创建/派生
//! - L2：使用 HMAC-SHA256 签名，用于所有需认证的 API 调用

use crate::clob::error::ClobResult;
use crate::clob::signing::{build_l1_signature, build_poly_hmac_signature};
use crate::types::clob_types::ApiKeyCreds;
use ethers::signers::{LocalWallet, Signer};
use std::collections::HashMap;

/// L2 认证所需的参数
#[derive(Debug, Clone)]
pub struct L2HeaderArgs {
    pub method: String,
    pub request_path: String,
    pub body: Option<String>,
}

/// 构建 L1 认证请求头
///
/// 用于创建或派生 API Key 的请求
pub async fn create_l1_headers(
    wallet: &LocalWallet,
    timestamp: u64,
    nonce: u64,
) -> ClobResult<HashMap<String, String>> {
    let sig = build_l1_signature(wallet, timestamp, nonce)?;
    let address = format!("{:?}", wallet.address());

    tracing::info!(
        "L1 AUTH address={} timestamp={} nonce={} sig={}",
        address,
        timestamp,
        nonce,
        sig
    );

    let mut headers = HashMap::new();

    headers.insert("POLY_ADDRESS".to_string(), address);

    headers.insert("POLY_SIGNATURE".to_string(), sig);

    headers.insert("POLY_TIMESTAMP".to_string(), timestamp.to_string());

    headers.insert("POLY_NONCE".to_string(), nonce.to_string());

    Ok(headers)
}

/// 构建 L2 认证请求头
///
/// 用于所有需要 API Key 认证的请求
pub fn create_l2_headers(
    wallet: &LocalWallet,
    creds: &ApiKeyCreds,
    args: &L2HeaderArgs,
    timestamp: u64,
) -> ClobResult<HashMap<String, String>> {
    let sig = build_poly_hmac_signature(
        &creds.secret,
        timestamp,
        &args.method,
        &args.request_path,
        args.body.as_deref(),
    )?;
    let address = format!("{:?}", wallet.address());

    let mut headers = HashMap::new();
    headers.insert("POLY_ADDRESS".to_string(), address);
    headers.insert("POLY_SIGNATURE".to_string(), sig);
    headers.insert("POLY_TIMESTAMP".to_string(), timestamp.to_string());
    headers.insert("POLY_API_KEY".to_string(), creds.key.clone());
    headers.insert("POLY_PASSPHRASE".to_string(), creds.passphrase.clone());
    Ok(headers)
}
