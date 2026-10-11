//! HMAC-SHA256 签名（L2 认证）
//!
//! L2 认证使用 HMAC-SHA256 对 `timestamp + method + requestPath + body`
//! 进行签名，密钥为 base64 编码的 API secret。

use base64::engine::general_purpose::URL_SAFE;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::clob::error::{ClobError, ClobResult};

type HmacSha256 = Hmac<Sha256>;

/// 构建 Polymarket L2 认证的 HMAC 签名
///
/// # 参数
/// - `secret`: base64(URL-safe, 带 padding) 编码的 API secret
/// - `timestamp`: Unix 时间戳（秒）
/// - `method`: HTTP 方法（大写，如 "GET", "POST"）
/// - `request_path`: 请求路径（含 query string）
/// - `body`: 请求体（可选，GET 请求无）
pub fn build_poly_hmac_signature(
    secret: &str,
    timestamp: u64,
    method: &str,
    request_path: &str,
    body: Option<&str>,
) -> ClobResult<String> {
    // 构建待签名消息：{timestamp}{METHOD}{path}{body}
    let mut message = format!("{timestamp}{method}{request_path}");
    if let Some(b) = body {
        message.push_str(b);
    }

    // A5：secret 必须为 URL-safe base64（带 padding）；空/非法一律 **fail-closed**，
    // 不再回退为 raw ASCII（旧 fallback 会把配置错误静默当密钥用 → 401 难排查）。
    if secret.trim().is_empty() {
        return Err(ClobError::AuthFailed("empty API secret".into()));
    }
    let secret_bytes = URL_SAFE
        .decode(secret)
        .map_err(|e| ClobError::AuthFailed(format!("invalid base64 API secret: {e}")))?;

    let mut mac = HmacSha256::new_from_slice(&secret_bytes).expect("HMAC 密钥长度错误");
    mac.update(message.as_bytes());
    let result = mac.finalize().into_bytes();

    // 输出 URL-safe base64（带 padding），与官方一致
    Ok(URL_SAFE.encode(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hmac_signature() {
        // 使用一个固定的 secret 和参数验证签名生成不报错
        let secret = "dGVzdC1zZWNyZXQta2V5LWZvci1wb2x5bWFya2V0"; // "test-secret-key-for-polymarket"
        let sig =
            build_poly_hmac_signature(secret, 1234567890, "GET", "/data/orders", None).unwrap();
        assert!(!sig.is_empty());
        // 验证 URL-safe：不含 + 或 /
        assert!(!sig.contains('+'));
        assert!(!sig.contains('/'));
    }

    /// 对齐官方 rs-clob-client 的测试向量，验证 secret 解码与签名编码一致。
    #[test]
    fn test_hmac_matches_official_vector() {
        let secret = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        let sig = build_poly_hmac_signature(
            secret,
            1_000_000,
            "test-sign",
            "/orders",
            Some(r#"{"hash":"0x123"}"#),
        )
        .unwrap();
        assert_eq!(sig, "4gJVbox-R6XlDK4nlaicig0_ANVL1qdcahiL8CXfXLM=");
    }

    /// A5：非法 base64 / 空 secret 必须 fail-closed，不再回退 raw ASCII。
    #[test]
    fn test_hmac_invalid_secret_fails_closed() {
        assert!(
            build_poly_hmac_signature("", 1, "GET", "/x", None).is_err(),
            "空 secret 应 Err"
        );
        assert!(
            build_poly_hmac_signature("!!!not-base64!!!", 1, "GET", "/x", None).is_err(),
            "非法 base64 应 Err"
        );
    }
}
