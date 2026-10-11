//! P0-001：FastExecutor 初始化调用链（离线 mock CLOB 验收）。
//!
//! 用 `wiremock` 本地 mock CLOB HTTP 端点，验证：
//! 1. `create_or_derive_api_key` 的「先 create，400 回退 derive」决策；
//! 2. 非 400 错误直接传播（FAIL-CLOSED）；
//! 3. `init_api_key` 成功链路的四步预热顺序（creds 已 set + 热钱包已预热）；
//! 4. `init_api_key` 失败时后续预热不执行（credits 未 set + 热钱包未预热）。
//!
//! 这四条共同坐实 P0-001 的「测试」证据：初始化任一环失败 → 错误向上传播，
//! 结合 `server.rs` 的 `halt + return`（调用链证据）→ 进程在 `serve()` 前退出，
//! 不会进入可交易状态。

use polymarket_rust::clob::client::{ClobClient, ClobConfig};
use polymarket_rust::clob::error::ClobError;
use polymarket_rust::fast::executor::FastExecutor;
use polymarket_rust::wallet::WalletManager;
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_PK: &str = "0xbd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b";

/// 构造内存钱包（tempdir 在返回时 drop，但 `unlock` 走内存 keystore，不受影响）
fn make_wallet() -> (Arc<WalletManager>, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ks.json");
    let mgr = WalletManager::new(&path);
    let addr = mgr.import_private_key(TEST_PK, "pw").unwrap();
    (Arc::new(mgr), addr)
}

fn mock_config(host: String) -> ClobConfig {
    ClobConfig {
        host,
        ..Default::default()
    }
}

/// 有效 API Key 凭证的 JSON（与 `ApiKeyCreds` 的 serde 字段对齐：apiKey/secret/passphrase）
fn creds_json(key: &str) -> serde_json::Value {
    serde_json::json!({
        "apiKey": key,
        "secret": "AAAA",
        "passphrase": "pass"
    })
}

/// P0-001-A：create 返回 400（已存在）→ 回退 derive 并拿到凭证
#[tokio::test]
async fn p0_001_create_then_derive_on_400() {
    let (mgr, addr) = make_wallet();
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/auth/api-key"))
        .respond_with(ResponseTemplate::new(400).set_body_string("key exists"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/derive-api-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(creds_json("derived-key")))
        .mount(&server)
        .await;

    let client = ClobClient::new_fast(mgr, mock_config(server.uri())).unwrap();
    let creds = client
        .create_or_derive_api_key(&addr, "pw")
        .await
        .expect("400 回退 derive 应成功");

    assert_eq!(creds.key, "derived-key", "应返回派生出的凭证");
}

/// P0-001-B：create 返回 500 → 错误直接传播（不尝试 derive）
#[tokio::test]
async fn p0_001_create_500_propagates() {
    let (mgr, addr) = make_wallet();
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/auth/api-key"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let client = ClobClient::new_fast(mgr, mock_config(server.uri())).unwrap();
    let err = client
        .create_or_derive_api_key(&addr, "pw")
        .await
        .expect_err("500 应直接报错");

    assert!(
        matches!(err, ClobError::Api { status: 500, .. }),
        "应得到 Api{{status:500}}，实际: {err:?}"
    );
}

/// P0-001-C：init_api_key 成功 → creds 已 set + 热钱包已预热
#[tokio::test]
async fn p0_001_init_api_key_success_prewarms_all() {
    let (mgr, addr) = make_wallet();
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/auth/api-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(creds_json("k1")))
        .mount(&server)
        .await;
    // HTTP warmup 会发 GET /，返回任意状态即可（warmup 只看 TCP+TLS 是否建立）
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let mut fe = FastExecutor::new(mgr, mock_config(server.uri())).unwrap();
    let creds = fe
        .init_api_key(&addr, "pw")
        .await
        .expect("init_api_key 应成功");

    assert_eq!(creds.key, "k1");
    // set_api_creds 已生效
    let stored = fe.clob_client().api_creds().expect("api_creds 应已设置");
    assert_eq!(stored.key, "k1");
    // HotWallet prewarm 已执行
    assert!(
        fe.wallet(&addr).is_some(),
        "init_api_key 成功后热钱包应已预热"
    );
}

/// P0-001-D：init_api_key 失败 → creds 未 set + 热钱包未预热（FAIL-CLOSED）
#[tokio::test]
async fn p0_001_init_api_key_failure_no_prewarm() {
    let (mgr, addr) = make_wallet();
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/auth/api-key"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let mut fe = FastExecutor::new(mgr, mock_config(server.uri())).unwrap();
    let err = fe
        .init_api_key(&addr, "pw")
        .await
        .expect_err("初始化失败应报错");

    assert!(
        matches!(err, ClobError::Api { status: 500, .. }),
        "应得到 Api{{status:500}}，实际: {err:?}"
    );
    // 失败时凭证未写入
    assert!(
        fe.clob_client().api_creds().is_none(),
        "初始化失败不应写入 API 凭证"
    );
    // 失败时热钱包未预热（prewarm 在 create_or_derive 之后，短路由直接返回）
    assert!(
        fe.wallet(&addr).is_none(),
        "初始化失败不应预热热钱包"
    );
}
