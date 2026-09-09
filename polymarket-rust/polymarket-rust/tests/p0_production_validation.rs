//! P0 生产验证集成测试（上线前必须全部通过）。
//!
//! 每个测试对应 P0 清单中的一项，通过 crate 公共 API 验证关键安全行为。
//! 与 `src/**` 内的单元测试互补：单元测试覆盖内部实现，本目录从外部验证公共契约。
//!
//! 运行：
//! ```bash
//! cargo test --test p0_production_validation
//! ```

#[path = "p0_production_validation/order_dedup.rs"]
mod order_dedup;
#[path = "p0_production_validation/risk_engine.rs"]
mod risk_engine;
#[path = "p0_production_validation/trading_gate.rs"]
mod trading_gate;
#[path = "p0_production_validation/fast_executor_init.rs"]
mod fast_executor_init;
