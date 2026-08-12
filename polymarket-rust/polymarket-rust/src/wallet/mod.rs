//! 钱包管理模块入口
//!
//! 提供 AES-256-GCM 加密的私钥安全存储、助记词生成与恢复、
//! 以及与 Polymarket 兼容的以太坊签名能力。

pub mod error;
pub mod keystore;
pub mod manager;

pub use error::{WalletError, WalletResult};
pub use keystore::{EncryptedKey, KeyStore};
pub use manager::WalletManager;
