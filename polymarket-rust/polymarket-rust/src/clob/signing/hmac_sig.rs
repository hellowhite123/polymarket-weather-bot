//! HMAC-SHA256 签名（L2 认证）
//!
//! L2 认证使用 HMAC-SHA256 对 `timestamp + method + requestPath + body`
//! 进行签名，密钥为 base64 编码的 API secret。

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// 构建 Polymarket L2 认证的 HMAC 签名
///
/// # 参数
/// - `secret`: base64 编码的 API secret
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
) -> String {
    // 构建待签名消息
    let mut message = format!("{timestamp}{method}{request_path}");
    if let Some(b) = body {
        message.push_str(b);
    }

    // secret 可能是 base64url 编码，统一解码
    let secret_bytes = B64URL.decode(secret).unwrap_or_else(|_| {
        // 回退到标准 base64
        base64::engine::general_purpose::STANDARD
            .decode(secret)
            .unwrap_or_else(|_| secret.as_bytes().to_vec())
    });

    let mut mac = HmacSha256::new_from_slice(&secret_bytes).expect("HMAC 密钥长度错误");
    mac.update(message.as_bytes());
    let result = mac.finalize().into_bytes();

    // 返回 url-safe base64（保留 = 后缀的变体，与 TS SDK 一致）
    // 先转标准 base64，再替换 + -> -, / -> _
    let standard_b64 = base64::engine::general_purpose::STANDARD.encode(result);
    standard_b64.replace('+', "-").replace('/', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hmac_signature() {
        // 使用一个固定的 secret 和参数验证签名生成不报错
        let secret = "dGVzdC1zZWNyZXQta2V5LWZvci1wb2x5bWFya2V0"; // "test-secret-key-for-polymarket"
        let sig = build_poly_hmac_signature(secret, 1234567890, "GET", "/data/orders", None);
        assert!(!sig.is_empty());
        // 验证 URL-safe：不含 + 或 /
        assert!(!sig.contains('+'));
        assert!(!sig.contains('/'));
    }
}
