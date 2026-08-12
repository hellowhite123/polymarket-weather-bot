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

pub type ClobResult<T> = Result<T, ClobError>;
