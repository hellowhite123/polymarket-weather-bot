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
