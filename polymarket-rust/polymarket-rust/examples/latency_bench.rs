//! 下单链路本地阶段延迟基准（微基准）
//!
//! 用途：在**无外网**环境下，量化「信号 → 下单请求体」这段纯本地计算
//! 各阶段的耗时，找出本地瓶颈，为「把签名/构建移出热路径」等优化提供
//! 数据依据。
//!
//! 覆盖的微基准：
//!   1. wallet_pbkdf2      —— 冷解锁（PBKDF2-HMAC-SHA256 210k 轮，历史主瓶颈）
//!   2. wallet_hot_hit     —— 热钱包缓存命中
//!   3. domain_cold        —— EIP-712 域分隔符冷计算（含 keccak）
//!   4. domain_hot         —— 域分隔符热缓存命中
//!   5. build_order        —— 订单结构构建（金额换算 + salt）
//!   6. sign_eip712        —— 完整 EIP-712 签名（keccak + secp256k1）
//!   7. digest_only        —— 仅 struct hash + digest（优化后签名里剩的 keccak）
//!   8. serialize_json     —— 下单请求体 JSON 序列化
//!   9. l2_hmac            —— L2 认证 HMAC-SHA256
//!  10. presign_take       —— 预签名订单池取单（快路径）
//!
//! 用法：
//!   PROTOC=C:/Users/32389/tools/protoc/bin/protoc.exe \
//!     cargo run --release --example latency_bench
//!
//! 输出为机器可读的 `name<TAB>mean_ms<TAB>p50_ms<TAB>p95_ms<TAB>p99_ms<TAB>max_ms<TAB>n`
//! 一行一条，便于汇总到报告。

use std::time::Instant;

use polymarket_rust::clob::client::{ClobClient, ClobConfig};
use polymarket_rust::clob::constants::get_exchange_address;
use polymarket_rust::clob::signing::{build_poly_hmac_signature, sign_order};
use polymarket_rust::fast::hot_wallet::HotWalletCache;
use polymarket_rust::fast::presign::{PreSignedOrder, PreSignedPool};
use polymarket_rust::fast::signing_cache::{self, PreparedOrder};
use polymarket_rust::types::clob_types::{Order, SignedOrder};
use polymarket_rust::types::enums::{Chain, OrderType, Side, TickSize};
use polymarket_rust::types::order::CreateOrderOptions;
use polymarket_rust::wallet::WalletManager;
use std::sync::Arc;

const TEST_PK: &str = "0xbd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b";
const TEST_PW: &str = "pw";

/// 分位数统计结果（单位：纳秒）
struct Stat {
    mean: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
    min: f64,
    n: usize,
}

fn stats(mut samples: Vec<f64>) -> Stat {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = samples.len();
    let pick = |q: f64| -> f64 {
        let idx = ((q * n as f64).ceil() as usize)
            .saturating_sub(1)
            .min(n - 1);
        samples[idx]
    };
    let sum: f64 = samples.iter().sum();
    Stat {
        mean: sum / n as f64,
        p50: pick(0.50),
        p95: pick(0.95),
        p99: pick(0.99),
        max: samples[n - 1],
        min: samples[0],
        n,
    }
}

fn emit(name: &str, s: &Stat) {
    println!(
        "{}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{}",
        name,
        s.mean / 1e6,
        s.p50 / 1e6,
        s.p95 / 1e6,
        s.p99 / 1e6,
        s.max / 1e6,
        s.n,
    );
}

/// 循环计时：把 `f` 执行 `n` 次，收集每次耗时（ns）
fn bench<F: FnMut()>(n: usize, mut f: F) -> Stat {
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        f();
        samples.push(t.elapsed().as_nanos() as f64);
    }
    stats(samples)
}

fn main() {
    println!("Polymarket 下单链路本地阶段延迟基准");
    println!("(release 模式结果；单位 ms；n=样本数)\n");

    // ---------- 初始化：临时 keystore + 钱包 ----------
    let tmp = std::env::temp_dir().join(format!("latency_bench_{}.json", std::process::id()));
    let mut mgr = WalletManager::new(&tmp);
    let address = mgr
        .import_private_key(TEST_PK, TEST_PW)
        .expect("导入私钥失败");
    let mgr = Arc::new(mgr);

    let cache = HotWalletCache::new(mgr.clone());
    cache.prewarm(&address, TEST_PW).expect("预热失败");
    let wallet = cache.get(&address).expect("热钱包未命中");

    let client = ClobClient::new(mgr.clone(), ClobConfig::default());
    let chain = Chain::Polygon;
    let verifying = get_exchange_address(chain, false);

    // ---------- 1. PBKDF2 冷解锁 ----------
    // 每次 unlock 都触发一次 PBKDF2（约 210k 轮）
    let s = bench(10, || {
        let _ = mgr.unlock(&address, TEST_PW).expect("解锁失败");
    });
    emit("wallet_pbkdf2", &s);

    // ---------- 2. 热钱包命中 ----------
    let s = bench(200_000, || {
        let _ = cache.get(&address);
    });
    emit("wallet_hot_hit", &s);

    // ---------- 3. 域分隔符冷计算 ----------
    // 用不同地址强制 cache miss，每次都是完整 keccak
    let mut cold_i = 0u64;
    let s = bench(10_000, || {
        cold_i += 1;
        let fake = format!("0x{:040x}", cold_i);
        let _ = signing_cache::domain_separator(137, &fake);
    });
    emit("domain_cold", &s);

    // ---------- 4. 域分隔符热缓存命中 ----------
    let _ = signing_cache::domain_separator(137, verifying);
    let s = bench(200_000, || {
        let _ = signing_cache::domain_separator(137, verifying);
    });
    emit("domain_hot", &s);

    // ---------- 构建一份样本订单（供后续 benchmark 复用） ----------
    let user_order = polymarket_rust::types::clob_types::UserOrder {
        token_id: "12345".to_string(),
        price: 0.5,
        size: 100.0,
        side: Side::Buy,
        fee_rate_bps: None,
        nonce: None,
        expiration: None,
        taker: None,
    };
    let options = CreateOrderOptions {
        tick_size: TickSize::Point01,
        neg_risk: false,
    };

    // ---------- 5. 订单构建 ----------
    let s = bench(100_000, || {
        let _ = client
            .build_unsigned_order(&wallet, &user_order, &options)
            .expect("构建失败");
    });
    emit("build_order", &s);

    // ---------- 6. 完整 EIP-712 签名 ----------
    let order: Order = client
        .build_unsigned_order(&wallet, &user_order, &options)
        .expect("构建失败");
    let s = bench(5_000, || {
        let _ = sign_order(&wallet, &order, chain.id(), verifying).expect("签名失败");
    });
    emit("sign_eip712", &s);

    // ---------- 7. 仅 struct hash + digest（优化后签名的 keccak 部分） ----------
    let prepared = PreparedOrder::from_order(&order);
    let sep = signing_cache::domain_separator(137, verifying);
    let s = bench(200_000, || {
        let _ = prepared.digest(&sep);
    });
    emit("digest_only", &s);

    // ---------- 8. JSON 序列化 ----------
    let signature = sign_order(&wallet, &order, chain.id(), verifying).expect("签名失败");
    let signed = SignedOrder {
        order: order.clone(),
        signature,
    };
    let s = bench(100_000, || {
        let _ = client
            .build_order_body(&wallet, &signed, OrderType::GTC)
            .expect("序列化失败");
    });
    emit("serialize_json", &s);

    // ---------- 9. L2 HMAC ----------
    let body = client
        .build_order_body(&wallet, &signed, OrderType::GTC)
        .expect("序列化失败");
    let secret = "dGVzdC1zZWNyZXQta2V5LWZvci1wb2x5bWFya2V0";
    let s = bench(200_000, || {
        let _ = build_poly_hmac_signature(secret, 1234567890, "POST", "/order", Some(&body));
    });
    emit("l2_hmac", &s);

    // ---------- 10. 预签名池取单 ----------
    let pool = PreSignedPool::new();
    for i in 0..1_000u64 {
        let mut o = order.clone();
        o.salt = i.to_string();
        let so = SignedOrder {
            order: o,
            signature: "0x00".to_string(),
        };
        pool.insert(PreSignedOrder::new("12345", Side::Buy, 0.5, 100.0, so));
    }
    let mut refill_i = 0u64;
    let s = bench(100_000, || {
        let _ = pool.take("12345", Side::Buy, 0.5);
        // 补回，保证池不空（take 是移除语义）
        refill_i += 1;
        let mut o = order.clone();
        o.salt = format!("r{}", refill_i);
        let so = SignedOrder {
            order: o,
            signature: "0x00".to_string(),
        };
        pool.insert(PreSignedOrder::new("12345", Side::Buy, 0.5, 100.0, so));
    });
    emit("presign_take", &s);

    // 清理临时 keystore
    let _ = std::fs::remove_file(&tmp);

    println!("\n完成。");
}
