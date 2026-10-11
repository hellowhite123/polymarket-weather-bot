//! P0-007：StateSync 启动 reconciliation 集成测试。
//!
//! 用 `wiremock` 本地 mock CLOB / Data API 端点，离线复现 `server.rs` 的启动
//! 对账决策链，验证三个关闭场景：
//!
//! 1. **正常同步 → Ready**：远端仓位 / 订单与本地一致，`reconcile_complete`
//!    进入 READY。
//! 2. **critical diff → 不允许 Ready**：本地存在远端没有的非零仓位（或反之），
//!    对账报 Critical，TradingGate 保持 Reconciling。
//! 3. **recovery unresolved → fail-closed**：本地 pending 订单在远端消失、
//!    且成交记录拉取失败（无法分类终态），订单保留 pending，二次对账报
//!    `order_missing_remote` Critical，TradingGate 不得进入 Ready。
//!
//! 决策逻辑本体（`TradingGate::reconcile_complete` / `StateSync::reconcile_*` /
//! `build_recovery_plan`）已由 `src/**` 单元测试覆盖；本文件从 crate 公共 API
//! 验证「启动对账结果 → TradingGate 状态」这条真实接线（含网络解析）。

use std::collections::{HashMap, HashSet};

use ethers::signers::LocalWallet;
use polymarket_rust::fast::state_sync::StateSync;
use polymarket_rust::fast::trading_gate::{TradingGate, TradingGateState};
use polymarket_rust::types::ApiKeyCreds;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 测试私钥（与 `state_sync.rs` 单元测试一致，仅用于构造签名器，不触链）。
const TEST_PK: &str = "bd8024c85d743569c738586185ca743b40af25c0144f8bf1cc7c719bd987a56b";

/// 构造一个指向 mock server 的 StateSync（clob_host 与 data_api_host 同源，
/// 用不同 path 区分：`/positions` 走 data_api，`/data/orders`、`/data/trades`
/// 走 clob）。
fn make_sync(host: String) -> StateSync {
    let wallet: LocalWallet = TEST_PK.parse().expect("测试私钥解析失败");
    let creds = ApiKeyCreds {
        key: "k".into(),
        secret: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
        passphrase: "p".into(),
    };
    StateSync::new(
        &host,
        &host,
        "0x0000000000000000000000000000000000000000",
        wallet,
        creds,
    )
}

/// 空仓位响应（PaginatedResponse 包装对象）。
fn empty_positions() -> serde_json::Value {
    serde_json::json!({ "limit": 500, "count": 0, "data": [] })
}

/// 空订单响应（PaginatedResponse 包装对象）。
fn empty_orders() -> serde_json::Value {
    serde_json::json!({ "limit": 500, "count": 0, "data": [] })
}

/// 场景 1：正常同步 → Ready。
///
/// 远端仓位空 + 订单空，本地也空 → 两个报告都 in_sync → READY。
#[tokio::test]
async fn p0_007_startup_normal_sync_enters_ready() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/positions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_positions()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/data/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_orders()))
        .mount(&server)
        .await;

    let sync = make_sync(server.uri());
    let local_positions: HashMap<String, f64> = HashMap::new();
    let local_pending: HashSet<String> = HashSet::new();

    // 复现 server.rs：启动对账（正常同步场景无消失订单，不拉 trades）
    let (pos_report, order_report) = sync
        .full_reconcile(&local_positions, &local_pending)
        .await
        .expect("对账应成功");

    assert!(pos_report.in_sync, "空对空仓位应 in_sync");
    assert!(order_report.in_sync, "空对空订单应 in_sync");

    let gate = TradingGate::new();
    gate.reconcile_complete(pos_report.in_sync, order_report.in_sync);
    assert_eq!(gate.state(), TradingGateState::Ready);
    assert!(gate.ensure_ready().is_ok());
}

/// 场景 2：critical diff → 不允许 Ready。
///
/// 本地有 token-1 非零仓位（250），远端仓位为空 → `missing_remote` Critical。
/// 即使订单维度同步，TradingGate 也必须保持 Reconciling，不得放行。
#[tokio::test]
async fn p0_007_startup_critical_position_diff_not_ready() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/positions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_positions()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/data/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_orders()))
        .mount(&server)
        .await;

    let sync = make_sync(server.uri());
    // 本地有 token-1 = 250 非零仓位，远端没有
    let local_positions: HashMap<String, f64> =
        [("token-1".to_string(), 250.0)].into_iter().collect();
    let local_pending: HashSet<String> = HashSet::new();

    let (pos_report, order_report) = sync
        .full_reconcile(&local_positions, &local_pending)
        .await
        .expect("对账应成功");

    assert!(!pos_report.in_sync, "本地非零仓位在远端缺失应报差异");
    assert!(
        pos_report.has_critical_diff(),
        "missing_remote 应判定为 Critical"
    );
    assert!(order_report.in_sync, "订单维度应同步");

    let gate = TradingGate::new();
    gate.reconcile_complete(pos_report.in_sync, order_report.in_sync);
    assert_eq!(gate.state(), TradingGateState::Reconciling);
    assert!(gate.ensure_ready().is_err(), "critical diff 不得放行交易");
}

/// 场景 3：recovery unresolved → fail-closed。
///
/// 本地 pending 订单 o1 在远端 open orders 中消失，且 `/data/trades` 拉取失败
/// （无法分类 Filled/NonFilled）→ `build_recovery_plan` 把 o1 放入
/// `unresolved_orders`。订单保留 pending，二次对账报 `order_missing_remote`
/// Critical → TradingGate 不得进入 Ready。
#[tokio::test]
async fn p0_007_startup_unresolved_fail_closed() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/positions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_positions()))
        .mount(&server)
        .await;
    // 远端 open orders 为空：o1 已消失
    Mock::given(method("GET"))
        .and(path("/data/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(empty_orders()))
        .mount(&server)
        .await;
    // trades 拉取失败 → 消失订单无法分类 → unresolved（fail-closed）
    Mock::given(method("GET"))
        .and(path("/data/trades"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let sync = make_sync(server.uri());
    let local_positions: HashMap<String, f64> = HashMap::new();
    let local_pending: HashSet<String> = ["o1".to_string()].into_iter().collect();

    // 复现 server.rs 第 1 步：full_reconcile_with_recovery
    let (_, _, recovery) = sync
        .full_reconcile_with_recovery(&local_positions, &local_pending)
        .await
        .expect("带恢复的对账应成功");

    // 消失订单因 trades 拉取失败而无法分类 → unresolved
    assert!(
        recovery.has_unresolved(),
        "trades 拉取失败时消失订单应进入 unresolved"
    );
    assert!(
        recovery.unresolved_orders.contains(&"o1".to_string()),
        "o1 应在 unresolved 列表中"
    );

    // 复现 server.rs：apply_recovery 不处理 unresolved 订单（它不在 actions 里），
    // 因此 o1 仍留在本地 pending。二次 full_reconcile 会再次检测到
    // order_missing_remote（Critical），最终 fail-closed。
    let (pos_report, order_report) = sync
        .full_reconcile(&local_positions, &local_pending)
        .await
        .expect("二次对账应成功");

    assert!(!order_report.in_sync, "unresolved 订单保留 pending 应导致订单维度失同步");
    assert!(order_report.has_critical_diff());

    let gate = TradingGate::new();
    gate.reconcile_complete(pos_report.in_sync, order_report.in_sync);
    assert_eq!(gate.state(), TradingGateState::Reconciling);
    assert!(gate.ensure_ready().is_err(), "unresolved 必须 fail-closed，不得 Ready");
}
