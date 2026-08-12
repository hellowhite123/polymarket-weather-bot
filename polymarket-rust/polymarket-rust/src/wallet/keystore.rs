//! AES-256-GCM 加密的密钥存储
//!
//! 私钥在静止状态下使用 AES-256-GCM 加密，加密密钥通过
//! PBKDF2-HMAC-SHA256 从用户密码派生。每个加密项使用
//! 随机 salt 和 96-bit 随机 nonce。

use crate::wallet::error::{WalletError, WalletResult};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// PBKDF2 迭代次数，兼顾安全与性能
const PBKDF2_ITERATIONS: u32 = 210_000;
/// 派生密钥长度（字节），AES-256 需要 32 字节
const KEY_LEN: usize = 32;
/// GCM nonce 长度（12 字节）
const NONCE_LEN: usize = 12;
/// Salt 长度（16 字节）
const SALT_LEN: usize = 16;

/// 单个加密后的私钥条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedKey {
    /// 钱包地址（0x 开头，小写校验格式）
    pub address: String,
    /// 加密后的密文（base64）
    pub ciphertext: String,
    /// 随机 salt（base64）
    pub salt: String,
    /// 随机 nonce（base64）
    pub nonce: String,
    /// PBKDF2 迭代次数
    pub iterations: u32,
}

/// 整体密钥库，可序列化到磁盘
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KeyStore {
    /// 所有加密的密钥条目，按地址索引
    pub keys: Vec<EncryptedKey>,
}

impl KeyStore {
    /// 创建空的密钥库
    pub fn new() -> Self {
        Self { keys: vec![] }
    }

    /// 查找指定地址的加密条目
    pub fn find(&self, address: &str) -> Option<&EncryptedKey> {
        // 忽略大小写比较地址
        self.keys
            .iter()
            .find(|k| k.address.eq_ignore_ascii_case(address))
    }

    /// 添加一个加密条目（地址冲突时返回错误）
    pub fn insert(&mut self, key: EncryptedKey) -> WalletResult<()> {
        if self.find(&key.address).is_some() {
            return Err(WalletError::WalletExists(key.address));
        }
        self.keys.push(key);
        Ok(())
    }

    /// 删除指定地址的条目
    pub fn remove(&mut self, address: &str) -> WalletResult<()> {
        let before = self.keys.len();
        self.keys.retain(|k| !k.address.eq_ignore_ascii_case(address));
        if self.keys.len() == before {
            Err(WalletError::WalletNotFound(address.to_string()))
        } else {
            Ok(())
        }
    }

    /// 列出所有钱包地址
    pub fn list_addresses(&self) -> Vec<String> {
        self.keys.iter().map(|k| k.address.clone()).collect()
    }
}

impl EncryptedKey {
    /// 使用用户密码加密私钥
    ///
    /// # 参数
    /// - `private_key`: 以 0x 开头或不带前缀的十六进制私钥
    /// - `address`: 对应的以太坊地址（0x 开头）
    /// - `password`: 用户提供的加密密码
    pub fn encrypt(private_key: &str, address: &str, password: &str) -> WalletResult<Self> {
        // 标准化私钥：去掉 0x 前缀，转为字节
        let pk_hex = private_key.trim_start_matches("0x");
        let pk_bytes = hex::decode(pk_hex)
            .map_err(|e| WalletError::InvalidPrivateKey(format!("hex 解码失败: {e}")))?;

        if pk_bytes.len() != 32 {
            return Err(WalletError::InvalidPrivateKey(format!(
                "私钥长度应为 32 字节，实际 {} 字节",
                pk_bytes.len()
            )));
        }

        // 生成随机 salt 和 nonce
        let mut salt = vec![0u8; SALT_LEN];
        let mut nonce_bytes = vec![0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut salt);
        rand::thread_rng().fill_bytes(&mut nonce_bytes);

        // 通过 PBKDF2 从密码派生 AES-256 密钥
        let mut derived_key = [0u8; KEY_LEN];
        pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password.as_bytes(), &salt, PBKDF2_ITERATIONS, &mut derived_key);

        // AES-256-GCM 加密
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&derived_key));
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(nonce, pk_bytes.as_ref())
            .map_err(|e| WalletError::EncryptionFailed(e.to_string()))?;

        Ok(Self {
            address: address.to_lowercase(),
            ciphertext: B64.encode(&ciphertext),
            salt: B64.encode(&salt),
            nonce: B64.encode(&nonce_bytes),
            iterations: PBKDF2_ITERATIONS,
        })
    }

    /// 使用用户密码解密私钥，返回原始十六进制私钥（0x 开头）
    pub fn decrypt(&self, password: &str) -> WalletResult<String> {
        let ciphertext = B64
            .decode(&self.ciphertext)
            .map_err(|_| WalletError::DecryptionFailed)?;
        let salt = B64
            .decode(&self.salt)
            .map_err(|_| WalletError::DecryptionFailed)?;
        let nonce_bytes = B64
            .decode(&self.nonce)
            .map_err(|_| WalletError::DecryptionFailed)?;

        let mut derived_key = [0u8; KEY_LEN];
        pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password.as_bytes(), &salt, self.iterations, &mut derived_key);

        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&derived_key));
        let nonce = Nonce::from_slice(&nonce_bytes);
        let plaintext = cipher.decrypt(nonce, ciphertext.as_ref())?;

        let hex_str = hex::encode(&plaintext);
        Ok(format!("0x{hex_str}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let pk = "0xbd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b";
        let address = "0x1234567890abcdef1234567890abcdef12345678";
        let password = "my-strong-password-123";

        let encrypted = EncryptedKey::encrypt(pk, address, password).unwrap();
        let decrypted = encrypted.decrypt(password).unwrap();

        assert_eq!(decrypted, pk);
    }

    #[test]
    fn test_wrong_password_fails() {
        let pk = "0xbd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b";
        let address = "0x1234567890abcdef1234567890abcdef12345678";

        let encrypted = EncryptedKey::encrypt(pk, address, "correct-password").unwrap();
        let result = encrypted.decrypt("wrong-password");
        assert!(result.is_err());
    }

    #[test]
    fn test_keystore_operations() {
        let mut ks = KeyStore::new();
        let ek = EncryptedKey::encrypt(
            "0xbd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b",
            "0x1234567890abcdef1234567890abcdef12345678",
            "pass",
        )
        .unwrap();

        ks.insert(ek.clone()).unwrap();
        assert!(ks.find("0x1234567890abcdef1234567890abcdef12345678").is_some());
        assert_eq!(ks.list_addresses().len(), 1);

        // 重复插入应失败
        assert!(ks.insert(ek).is_err());

        ks.remove("0x1234567890abcdef1234567890abcdef12345678").unwrap();
        assert!(ks.list_addresses().is_empty());
    }
}
