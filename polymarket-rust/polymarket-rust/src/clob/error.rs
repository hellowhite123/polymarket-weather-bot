//! CLOB 模块错误类型

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ClobError {
    #[error("HTTP 请求失败: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API 返回错误 ({status}): {message}")]
    Api { status: u16, message: String },

    #[error("序列化/反序列化失败: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("钱包签名失败: {0}")]
    Signer(String),

    #[error("无效参数: {0}")]
    InvalidParam(String),

    #[error("认证失败: {0}")]
    AuthFailed(String),

    #[error("未配置 API 凭证")]
    NoApiCreds,

    #[error("市场数据获取失败: {0}")]
    MarketDataError(String),

    #[error("下单失败: {0}")]
    OrderFailed(String),
}

impl From<ethers::signers::WalletError> for ClobError {
    fn from(e: ethers::signers::WalletError) -> Self {
        ClobError::Signer(e.to_string())
    }
}

impl From<crate::wallet::WalletError> for ClobError {
    fn from(e: crate::wallet::WalletError) -> Self {
        ClobError::Signer(e.to_string())
    }
}

impl ClobError {
    /// 下单结果是否「未知」（订单可能已上链/已成交，需 StateSync 确认远端再决定）。
    ///
    /// - 网络错误（超时/连接重置）＝ 请求可能已到达交易所但响应丢失 → 未知
    /// - 5xx 服务端错误 ＝ 服务器可能已处理但返回失败 → 未知
    /// - 其余（4xx 明确拒单 + 本地构建/签名/序列化失败）＝ 确定未成交
    pub fn is_unknown_outcome(&self) -> bool {
        match self {
            ClobError::Http(_) => true,
            ClobError::Api { status, .. } => *status >= 500,
            _ => false,
        }
    }
}

pub type ClobResult<T> = Result<T, ClobError>;

#[cfg(test)]
mod tests {
    use super::*;

    fn api(status: u16) -> ClobError {
        ClobError::Api {
            status,
            message: "x".to_string(),
        }
    }

    #[test]
    fn test_is_unknown_outcome_5xx_unknown_4xx_definite() {
        // 5xx → 未知（服务器可能已处理但返回失败）
        assert!(api(500).is_unknown_outcome());
        assert!(api(502).is_unknown_outcome());
        assert!(api(503).is_unknown_outcome());
        // 4xx → 确定拒单（客户端错误，订单未上链）
        assert!(!api(400).is_unknown_outcome());
        assert!(!api(401).is_unknown_outcome());
        assert!(!api(422).is_unknown_outcome());
    }

    #[test]
    fn test_is_unknown_outcome_local_errors_definite() {
        assert!(!ClobError::Signer("x".to_string()).is_unknown_outcome());
        assert!(!ClobError::InvalidParam("x".to_string()).is_unknown_outcome());
        assert!(!ClobError::OrderFailed("x".to_string()).is_unknown_outcome());
        assert!(!ClobError::NoApiCreds.is_unknown_outcome());
    }
}
