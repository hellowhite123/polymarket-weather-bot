//! 钱包管理器
//!
//! 整合密钥库、磁盘持久化和以太坊签名能力。

use crate::wallet::error::{WalletError, WalletResult};
use crate::wallet::keystore::{EncryptedKey, KeyStore};
use ethers::signers::{LocalWallet, Signer};
use ethers::types::Signature;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

/// 钱包管理器，负责密钥库的加载、保存和私钥的安全管理
pub struct WalletManager {
    keystore: Mutex<KeyStore>,
    path: PathBuf,
}

impl WalletManager {
    /// 创建新的空钱包管理器，指定密钥库文件路径
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            keystore: Mutex::new(KeyStore::new()),
            path: path.as_ref().to_path_buf(),
        }
    }

    /// 从磁盘加载密钥库（文件不存在则创建空库）
    pub fn load<P: AsRef<Path>>(path: P) -> WalletResult<Self> {
        let path = path.as_ref().to_path_buf();
        let keystore = if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            if content.trim().is_empty() {
                KeyStore::new()
            } else {
                serde_json::from_str(&content)?
            }
        } else {
            KeyStore::new()
        };
        Ok(Self {
            keystore: Mutex::new(keystore),
            path,
        })
    }

    /// 将密钥库保存到磁盘
    pub fn save(&self) -> WalletResult<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let ks = self.keystore.lock().unwrap();
        let content = serde_json::to_string_pretty(&*ks)?;
        std::fs::write(&self.path, content)?;
        Ok(())
    }

    /// 导入私钥（用密码加密后存入密钥库）
    ///
    /// # 参数
    /// - `private_key`: 0x 开头或不带前缀的十六进制私钥
    /// - `password`: 用于加密的密码
    pub fn import_private_key(&self, private_key: &str, password: &str) -> WalletResult<String> {
        let wallet = parse_wallet(private_key)?;
        let address = format!("{:?}", wallet.address());
        let encrypted = EncryptedKey::encrypt(private_key, &address, password)?;
        self.keystore.lock().unwrap().insert(encrypted)?;
        self.save()?;
        Ok(address)
    }

    /// 删除钱包
    pub fn remove_wallet(&self, address: &str) -> WalletResult<()> {
        self.keystore.lock().unwrap().remove(address)?;
        self.save()
    }

    /// 列出所有钱包地址
    pub fn list_addresses(&self) -> Vec<String> {
        self.keystore.lock().unwrap().list_addresses()
    }

    /// 解锁指定地址的钱包，返回可签名的 LocalWallet
    pub fn unlock(&self, address: &str, password: &str) -> WalletResult<LocalWallet> {
        let ks = self.keystore.lock().unwrap();
        let encrypted = ks
            .find(address)
            .ok_or_else(|| WalletError::WalletNotFound(address.to_string()))?;
        // R5-005：private_key 为 Zeroizing<String>，解析出 LocalWallet 后即擦除。
        let private_key = encrypted.decrypt(password)?;
        parse_wallet(private_key.as_str())
    }

    /// 使用指定钱包对原始消息进行 EIP-191 签名
    pub async fn sign_message(
        &self,
        address: &str,
        password: &str,
        message: &[u8],
    ) -> WalletResult<Signature> {
        let wallet = self.unlock(address, password)?;
        let sig = wallet.sign_message(message).await?;
        Ok(sig)
    }

    /// 使用指定钱包对 hash 进行签名（底层签名，不走 EIP-191 前缀）
    ///
    /// 用于 EIP-712 typed data 签名：先计算出 typed data hash，
    /// 再用此方法签名
    pub async fn sign_hash(
        &self,
        address: &str,
        password: &str,
        hash: ethers::types::H256,
    ) -> WalletResult<Signature> {
        let wallet = self.unlock(address, password)?;
        let sig = wallet.sign_hash(hash)?;
        Ok(sig)
    }

    /// 获取密钥库条目数量
    pub fn entry_count(&self) -> usize {
        self.keystore.lock().unwrap().keys.len()
    }
}

/// 从十六进制私钥解析 ethers LocalWallet
fn parse_wallet(private_key: &str) -> WalletResult<LocalWallet> {
    let pk = private_key.trim_start_matches("0x");
    let wallet = LocalWallet::from_str(pk)
        .map_err(|e: ethers::signers::WalletError| WalletError::InvalidPrivateKey(e.to_string()))?;
    Ok(wallet.with_chain_id(137u64)) // Polygon 主网
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_import_and_unlock() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("keystore.json");
        let mgr = WalletManager::new(&path);

        // 使用已知的测试私钥
        let pk = "0xbd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b";
        let address = mgr.import_private_key(pk, "test-pass").unwrap();

        assert!(mgr.list_addresses().contains(&address));

        // 解锁
        let wallet = mgr.unlock(&address, "test-pass").unwrap();
        assert_eq!(format!("{:?}", wallet.address()), address);
    }

    #[test]
    fn test_persistence() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("keystore.json");

        let pk = "0xbd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b";

        // 第一次写入
        {
            let mgr = WalletManager::new(&path);
            let _addr = mgr.import_private_key(pk, "pass1").unwrap();
        }

        // 重新加载
        {
            let mgr = WalletManager::load(&path).unwrap();
            assert_eq!(mgr.list_addresses().len(), 1);
        }
    }
}
