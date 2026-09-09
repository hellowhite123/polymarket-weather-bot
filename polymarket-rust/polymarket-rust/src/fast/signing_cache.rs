//! EIP-712 签名快速路径 —— 域分隔符预计算 + 零堆分配 struct hash
//!
//! ## 优化点
//!
//! 原 `eip712_order::order_typed_data_hash` 每次签名都要：
//!
//! 1. `keccak256(ORDER_TYPE_STR)` —— 类型哈希，实际是常量
//! 2. `keccak256(DOMAIN_TYPE_STR)` —— 同上
//! 3. `keccak256(abi_encode(name))` + `keccak256(abi_encode(version))`
//! 4. 拼接域字段 → `keccak256` 得到 domain separator
//! 5. 两次 `Vec::with_capacity` 堆分配
//!
//! 其中 1~4 的结果**只取决于 (chain_id, verifying_contract)**，在整个进程
//! 生命周期内至多 4 种组合（2 条链 × negRisk 与否）。本模块把它们预计算并
//! 缓存，热路径只剩两次 keccak256。
//!
//! ## 顺带修复的正确性 Bug
//!
//! 原实现用 `keccak256(ethers::abi::encode(&[Token::String(name)]))` 计算
//! `name` 的哈希。ABI 编码会产出 `offset(32B) || length(32B) || padded_data`，
//! 再对其取哈希——这与 EIP-712 规范不符。
//!
//! > EIP-712: The dynamic values `bytes` and `string` are encoded as a
//! > keccak256 hash of their contents.
//!
//! 正确做法是 `keccak256(utf8_bytes(name))`。原实现算出的 domain separator
//! 是错的，签名会被 Polymarket 交易所判定为无效签名而拒单。本模块使用
//! 规范实现。

use crate::clob::constants::{PROTOCOL_NAME, PROTOCOL_VERSION};
use crate::types::clob_types::Order;
use ethers::types::{H256, U256};
use ethers::utils::keccak256;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};

/// Order 结构的 EIP-712 类型字符串
pub const ORDER_TYPE_STR: &str = "Order(uint256 salt,address maker,address signer,address taker,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint256 expiration,uint256 nonce,uint256 feeRateBps,uint8 side,uint8 signatureType)";

/// EIP712Domain 类型字符串
pub const DOMAIN_TYPE_STR: &str =
    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";

/// Order 类型哈希（进程内只算一次）
#[inline]
pub fn order_type_hash() -> &'static [u8; 32] {
    static H: OnceLock<[u8; 32]> = OnceLock::new();
    H.get_or_init(|| keccak256(ORDER_TYPE_STR.as_bytes()))
}

/// EIP712Domain 类型哈希
#[inline]
fn domain_type_hash() -> &'static [u8; 32] {
    static H: OnceLock<[u8; 32]> = OnceLock::new();
    H.get_or_init(|| keccak256(DOMAIN_TYPE_STR.as_bytes()))
}

/// name 字段哈希 —— 按 EIP-712 规范直接对 UTF-8 字节取 keccak256
#[inline]
fn name_hash() -> &'static [u8; 32] {
    static H: OnceLock<[u8; 32]> = OnceLock::new();
    H.get_or_init(|| keccak256(PROTOCOL_NAME.as_bytes()))
}

/// version 字段哈希
#[inline]
fn version_hash() -> &'static [u8; 32] {
    static H: OnceLock<[u8; 32]> = OnceLock::new();
    H.get_or_init(|| keccak256(PROTOCOL_VERSION.as_bytes()))
}

/// 域分隔符缓存表
fn domain_cache() -> &'static RwLock<HashMap<(u64, [u8; 20]), [u8; 32]>> {
    static C: OnceLock<RwLock<HashMap<(u64, [u8; 20]), [u8; 32]>>> = OnceLock::new();
    C.get_or_init(|| RwLock::new(HashMap::new()))
}

static DOMAIN_HITS: AtomicU64 = AtomicU64::new(0);
static DOMAIN_MISSES: AtomicU64 = AtomicU64::new(0);

/// 计算（并缓存）EIP-712 域分隔符
///
/// 键为 `(chain_id, verifying_contract)`。命中时零 keccak256 调用。
pub fn domain_separator(chain_id: u64, verifying_contract: &str) -> [u8; 32] {
    let addr = parse_address_20(verifying_contract);
    let key = (chain_id, addr);

    // 读锁快路径
    if let Ok(guard) = domain_cache().read() {
        if let Some(v) = guard.get(&key) {
            DOMAIN_HITS.fetch_add(1, Ordering::Relaxed);
            return *v;
        }
    }

    // 未命中：计算
    DOMAIN_MISSES.fetch_add(1, Ordering::Relaxed);
    let sep = compute_domain_separator(chain_id, &addr);
    if let Ok(mut guard) = domain_cache().write() {
        guard.insert(key, sep);
    }
    sep
}

/// 实际计算域分隔符（无缓存）
fn compute_domain_separator(chain_id: u64, verifying_contract: &[u8; 20]) -> [u8; 32] {
    // 5 个 32 字节字段，栈上固定数组，零堆分配
    let mut buf = [0u8; 32 * 5];
    buf[0..32].copy_from_slice(domain_type_hash());
    buf[32..64].copy_from_slice(name_hash());
    buf[64..96].copy_from_slice(version_hash());
    U256::from(chain_id).to_big_endian(&mut buf[96..128]);
    // address 左侧补零到 32 字节
    buf[128 + 12..160].copy_from_slice(verifying_contract);
    keccak256(buf)
}

/// 域分隔符缓存统计 (hits, misses)
pub fn domain_cache_stats() -> (u64, u64) {
    (
        DOMAIN_HITS.load(Ordering::Relaxed),
        DOMAIN_MISSES.load(Ordering::Relaxed),
    )
}

/// 预热域分隔符缓存
///
/// 启动时调用，把 Polygon 主网的两个 exchange 合约（普通 / negRisk）
/// 全部算好，避免第一笔单付出额外开销。
pub fn prewarm_domains() {
    use crate::clob::constants::get_exchange_address;
    use crate::types::enums::Chain;
    for chain in [Chain::Polygon, Chain::Amoy] {
        for neg_risk in [false, true] {
            let _ = domain_separator(chain.id(), get_exchange_address(chain, neg_risk));
        }
    }
}

// ================================================================
// 已预解析的订单 —— 把字符串 → U256 的解析移出热路径
// ================================================================

/// 预解析的订单字段
///
/// `Order` 的所有字段都是 `String`，每次签名都要跑 12 次字符串解析。
/// 对于预签名场景，可以提前解析一次、反复复用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedOrder {
    pub salt: U256,
    pub maker: [u8; 20],
    pub signer: [u8; 20],
    pub taker: [u8; 20],
    pub token_id: U256,
    pub maker_amount: U256,
    pub taker_amount: U256,
    pub expiration: U256,
    pub nonce: U256,
    pub fee_rate_bps: U256,
    pub side: u8,
    pub signature_type: u8,
}

impl PreparedOrder {
    /// 从 `Order` 解析
    pub fn from_order(order: &Order) -> Self {
        Self {
            salt: parse_uint256(&order.salt),
            maker: parse_address_20(&order.maker),
            signer: parse_address_20(&order.signer),
            taker: parse_address_20(&order.taker),
            token_id: parse_uint256(&order.token_id),
            maker_amount: parse_uint256(&order.maker_amount),
            taker_amount: parse_uint256(&order.taker_amount),
            expiration: parse_uint256(&order.expiration),
            nonce: parse_uint256(&order.nonce),
            fee_rate_bps: parse_uint256(&order.fee_rate_bps),
            side: order.side,
            signature_type: order.signature_type,
        }
    }

    /// 计算 struct hash（零堆分配）
    pub fn struct_hash(&self) -> [u8; 32] {
        // 12 个字段 + 1 个类型哈希 = 13 个 32 字节槽
        let mut buf = [0u8; 32 * 13];
        buf[0..32].copy_from_slice(order_type_hash());
        self.salt.to_big_endian(&mut buf[32..64]);
        buf[64 + 12..96].copy_from_slice(&self.maker);
        buf[96 + 12..128].copy_from_slice(&self.signer);
        buf[128 + 12..160].copy_from_slice(&self.taker);
        self.token_id.to_big_endian(&mut buf[160..192]);
        self.maker_amount.to_big_endian(&mut buf[192..224]);
        self.taker_amount.to_big_endian(&mut buf[224..256]);
        self.expiration.to_big_endian(&mut buf[256..288]);
        self.nonce.to_big_endian(&mut buf[288..320]);
        self.fee_rate_bps.to_big_endian(&mut buf[320..352]);
        // uint8 也按 32 字节右对齐编码
        buf[352 + 31] = self.side;
        buf[384 + 31] = self.signature_type;
        keccak256(buf)
    }

    /// 结合域分隔符得到最终的 EIP-712 digest
    #[inline]
    pub fn digest(&self, domain_sep: &[u8; 32]) -> H256 {
        finalize_digest(domain_sep, &self.struct_hash())
    }
}

/// `keccak256("\x19\x01" || domainSeparator || structHash)`
#[inline]
pub fn finalize_digest(domain_sep: &[u8; 32], struct_hash: &[u8; 32]) -> H256 {
    let mut buf = [0u8; 66];
    buf[0] = 0x19;
    buf[1] = 0x01;
    buf[2..34].copy_from_slice(domain_sep);
    buf[34..66].copy_from_slice(struct_hash);
    H256::from(keccak256(buf))
}

/// 快速计算订单的 EIP-712 digest（使用缓存的域分隔符）
pub fn fast_order_digest(order: &Order, chain_id: u64, verifying_contract: &str) -> H256 {
    let sep = domain_separator(chain_id, verifying_contract);
    PreparedOrder::from_order(order).digest(&sep)
}

// ================================================================
// 解析辅助
// ================================================================

/// 解析 20 字节地址
pub fn parse_address_20(addr: &str) -> [u8; 20] {
    let s = addr.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    let mut out = [0u8; 20];
    // hex::decode_to_slice 要求长度精确匹配，先做长度校验
    if s.len() == 40 {
        let _ = hex::decode_to_slice(s, &mut out);
    } else if let Ok(bytes) = hex::decode(s) {
        let n = bytes.len().min(20);
        out[20 - n..].copy_from_slice(&bytes[bytes.len() - n..]);
    }
    out
}

/// 解析 U256（先十进制、后十六进制）
pub fn parse_uint256(s: &str) -> U256 {
    let t = s.trim();
    if t.is_empty() {
        return U256::zero();
    }
    // ethers::U256 的 FromStr 仅接受 "0x..." 格式，
    // 所以先用 u64 做十进制兜底，再走 hex 路径。
    if let Ok(u) = t.parse::<u64>() {
        return U256::from(u);
    }
    let h = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    U256::from_str_radix(h, 16).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clob::constants::get_exchange_address;
    use crate::types::enums::Chain;

    fn sample_order() -> Order {
        Order {
            salt: "1234567890".to_string(),
            maker: "0x1111111111111111111111111111111111111111".to_string(),
            signer: "0x2222222222222222222222222222222222222222".to_string(),
            taker: "0x0000000000000000000000000000000000000000".to_string(),
            token_id: "12345".to_string(),
            maker_amount: "1000000".to_string(),
            taker_amount: "500000".to_string(),
            expiration: "0".to_string(),
            nonce: "0".to_string(),
            fee_rate_bps: "0".to_string(),
            side: 0,
            signature_type: 0,
        }
    }

    #[test]
    fn test_name_hash_follows_eip712_spec() {
        // EIP-712 要求 string 直接对 UTF-8 字节取哈希
        let expected = keccak256(b"Polymarket CTF Exchange");
        assert_eq!(name_hash(), &expected);

        // 明确验证它与「ABI 编码后再哈希」不同（原实现的 bug）
        let abi_encoded = keccak256(ethers::abi::encode(&[ethers::abi::Token::String(
            PROTOCOL_NAME.to_string(),
        )]));
        assert_ne!(
            name_hash(),
            &abi_encoded,
            "ABI 编码后哈希应当与规范实现不同，说明旧实现确有 bug"
        );
    }

    #[test]
    fn test_parse_address_20() {
        let a = parse_address_20("0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E");
        assert_eq!(a[0], 0x4b);
        assert_eq!(a[19], 0x2E);

        let zero = parse_address_20("0x0000000000000000000000000000000000000000");
        assert_eq!(zero, [0u8; 20]);

        // 不带 0x 前缀
        let b = parse_address_20("4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E");
        assert_eq!(a, b);
    }

    #[test]
    fn test_parse_uint256() {
        assert_eq!(parse_uint256("12345"), U256::from(12345));
        assert_eq!(parse_uint256("0xff"), U256::from(255));
        assert_eq!(parse_uint256(""), U256::zero());
    }

    #[test]
    fn test_domain_separator_deterministic() {
        let addr = get_exchange_address(Chain::Polygon, false);
        let a = domain_separator(137, addr);
        let b = domain_separator(137, addr);
        assert_eq!(a, b);
    }

    #[test]
    fn test_domain_separator_differs_by_contract() {
        let normal = domain_separator(137, get_exchange_address(Chain::Polygon, false));
        let neg = domain_separator(137, get_exchange_address(Chain::Polygon, true));
        assert_ne!(normal, neg, "negRisk 合约应产生不同的域分隔符");
    }

    #[test]
    fn test_domain_separator_differs_by_chain() {
        let addr = get_exchange_address(Chain::Polygon, false);
        assert_ne!(domain_separator(137, addr), domain_separator(80002, addr));
    }

    #[test]
    fn test_domain_cache_hits() {
        let addr = get_exchange_address(Chain::Polygon, false);
        // 先跑一次确保已入缓存
        let _ = domain_separator(137, addr);
        // 重置计数器（全局 AtomicU64 可能被其他并行测试污染）
        DOMAIN_HITS.store(0, Ordering::Relaxed);
        for _ in 0..100 {
            let _ = domain_separator(137, addr);
        }
        let (h1, _) = domain_cache_stats();
        assert!(
            h1 >= 100,
            "100 次调用应至少命中缓存 100 次（实际 {}，可能受并行测试影响）",
            h1
        );
    }

    #[test]
    fn test_prepared_order_roundtrip() {
        let order = sample_order();
        let p = PreparedOrder::from_order(&order);
        assert_eq!(p.salt, U256::from(1234567890u64));
        assert_eq!(p.token_id, U256::from(12345));
        assert_eq!(p.maker_amount, U256::from(1000000));
        assert_eq!(p.maker[0], 0x11);
        assert_eq!(p.signer[0], 0x22);
        assert_eq!(p.taker, [0u8; 20]);
    }

    #[test]
    fn test_struct_hash_deterministic() {
        let p = PreparedOrder::from_order(&sample_order());
        assert_eq!(p.struct_hash(), p.struct_hash());
    }

    #[test]
    fn test_struct_hash_sensitive_to_fields() {
        let mut o = sample_order();
        let h1 = PreparedOrder::from_order(&o).struct_hash();
        o.maker_amount = "1000001".to_string();
        let h2 = PreparedOrder::from_order(&o).struct_hash();
        assert_ne!(h1, h2);

        let mut o2 = sample_order();
        o2.side = 1;
        assert_ne!(h1, PreparedOrder::from_order(&o2).struct_hash());

        let mut o3 = sample_order();
        o3.signature_type = 1;
        assert_ne!(h1, PreparedOrder::from_order(&o3).struct_hash());
    }

    #[test]
    fn test_digest_deterministic() {
        let order = sample_order();
        let addr = get_exchange_address(Chain::Polygon, false);
        let d1 = fast_order_digest(&order, 137, addr);
        let d2 = fast_order_digest(&order, 137, addr);
        assert_eq!(d1, d2);
    }

    #[test]
    fn test_prewarm_domains() {
        prewarm_domains();
        let (_, m0) = domain_cache_stats();
        prewarm_domains();
        let (_, m1) = domain_cache_stats();
        assert_eq!(m0, m1, "二次预热不应产生新的 miss");
    }
}
