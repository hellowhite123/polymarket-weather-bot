//! P0-003：RiskEngine pre_check 超仓拒单。
//!
//! 验证下单前风控在仓位超限时拒绝下单，杜绝超仓 / 超市场限制 / 风控失效。

use polymarket_rust::fast::risk_engine::{RiskConfig, RiskEngine};
use polymarket_rust::types::enums::Side;

#[test]
fn p0_003_position_limit_per_market_rejected() {
    let mut cfg = RiskConfig::default();
    cfg.max_position_per_market = 500.0;
    cfg.max_global_exposure = 100_000.0; // 放宽全局，聚焦单市场
    let engine = RiskEngine::new(cfg);

    // 开仓 400（< 500，通过）
    assert!(engine.pre_check("m1", Side::Buy, 400.0, 400.0, 50).is_ok());
    engine.post_check("m1", Side::Buy, 400.0, 400.0, 0.0, 10_000.0);

    // 再开 200 → 600 > 500，应拒单
    assert!(
        engine.pre_check("m1", Side::Buy, 200.0, 200.0, 50).is_err(),
        "单市场仓位超限应拒单"
    );
}

#[test]
fn p0_003_global_exposure_rejected() {
    let mut cfg = RiskConfig::default();
    cfg.max_position_per_market = 10_000.0; // 放宽单市场
    cfg.max_global_exposure = 1_000.0;
    let engine = RiskEngine::new(cfg);

    assert!(engine.pre_check("m1", Side::Buy, 600.0, 600.0, 50).is_ok());
    engine.post_check("m1", Side::Buy, 600.0, 600.0, 0.0, 10_000.0);

    // 600 + 500 > 1000 全局上限，应拒单
    assert!(
        engine.pre_check("m2", Side::Buy, 500.0, 500.0, 50).is_err(),
        "全局暴露超限应拒单"
    );
}

#[test]
fn p0_003_sell_always_allowed_to_reduce_exposure() {
    let engine = RiskEngine::new(RiskConfig::default());
    engine.post_check("m1", Side::Buy, 400.0, 400.0, 0.0, 10_000.0);
    // 平仓（Sell）应始终放行，即使市场已有仓位
    assert!(engine.pre_check("m1", Side::Sell, 100.0, 100.0, 50).is_ok());
}

/// P0-003（原子预留版）：`check_and_reserve` 超仓拒单时 reserve 不得泄漏。
///
/// 与上面只读 `pre_check` 版本互补——这里验证的是原子「检查 + 预留」路径：
/// 超仓拒单必须发生在 `reserved += notional` 之前，否则被拒订单会把幽灵预留
/// 留在风控里，后续合法订单会被误拒。
#[test]
fn p0_003_check_and_reserve_over_limit_no_reserve_leak() {
    let mut cfg = RiskConfig::default();
    cfg.max_position_per_market = 500.0;
    cfg.max_global_exposure = 100_000.0; // 放宽全局，聚焦单市场
    let engine = RiskEngine::new(cfg);

    // 预留 400（成功，reserved = 400）
    assert!(engine
        .check_and_reserve("m1", Side::Buy, 400.0, 400.0, 50)
        .is_ok());

    // 再预留 200 → 400 + 200 = 600 > 500，应拒单
    assert!(engine
        .check_and_reserve("m1", Side::Buy, 200.0, 200.0, 50)
        .is_err());

    // 关键断言：若上面的超仓拒单错误地把 200 记入 reserved，
    // 则当前 reserved 会变成 600，再预留 100 会变成 700 > 500 被误拒。
    // 这里能成功，证明超仓拒单没有泄漏 reserve。
    assert!(
        engine
            .check_and_reserve("m1", Side::Buy, 100.0, 100.0, 50)
            .is_ok(),
        "超仓拒单后 reserve 不应泄漏，否则合法订单会被误拒"
    );
}

/// P0-003（原子预留版·全局维度）：全局暴露超限拒单时 reserve 不泄漏。
#[test]
fn p0_003_check_and_reserve_global_over_limit_no_reserve_leak() {
    let mut cfg = RiskConfig::default();
    cfg.max_position_per_market = 10_000.0; // 放宽单市场
    cfg.max_global_exposure = 1_000.0;
    let engine = RiskEngine::new(cfg);

    // 市场 m1 预留 600（成功，全局 reserved = 600）
    assert!(engine
        .check_and_reserve("m1", Side::Buy, 600.0, 600.0, 50)
        .is_ok());

    // 市场 m2 再预留 500 → 全局 600 + 500 = 1100 > 1000，应拒单
    assert!(engine
        .check_and_reserve("m2", Side::Buy, 500.0, 500.0, 50)
        .is_err());

    // 关键断言：全局超限拒单若泄漏 reserve，全局会变成 1600，
    // 则 m2 再预留 400（600 + 400 = 1000）本应恰好到上限却会被误拒。
    // 这里能成功，证明全局超限拒单没有泄漏 reserve。
    assert!(
        engine
            .check_and_reserve("m2", Side::Buy, 400.0, 400.0, 50)
            .is_ok(),
        "全局超限拒单后 reserve 不应泄漏"
    );
}
