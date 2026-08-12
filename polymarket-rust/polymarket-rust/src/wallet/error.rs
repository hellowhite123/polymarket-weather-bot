//! 钱包模块的错误类型

use thiserror::Error;

#[derive(Error, Debug)]
pub enum WalletError {
    #[error("无效的私钥格式: {0}")]
    InvalidPrivateKey(String),

    #[error("无效的助记词: {0}")]
    InvalidMnemonic(String),

    #[error("密码错误，无法解密私钥")]
    DecryptionFailed,

    #[error("钱包已存在: {0}")]
    WalletExists(String),

    #[error("钱包不存在: {0}")]
    WalletNotFound(String),

    #[error("加密失败: {0}")]
    EncryptionFailed(String),

    #[error("签名失败: {0}")]
    SigningFailed(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("序列化错误: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("底层密码学错误: {0}")]
    Crypto(String),
}

impl From<aes_gcm::Error> for WalletError {
    fn from(_: aes_gcm::Error) -> Self {
        WalletError::DecryptionFailed
    }
}

impl From<ethers::signers::WalletError> for WalletError {
    fn from(e: ethers::signers::WalletError) -> Self {
        WalletError::InvalidPrivateKey(e.to_string())
    }
}

pub type WalletResult<T> = Result<T, WalletError>;
