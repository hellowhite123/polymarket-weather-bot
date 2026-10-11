//! P0-007 / P0-011：TradingGate 状态机 + 启动对账进 READY。
//!
//! 验证交易闸门在上线前的关键安全行为：非 READY 必须拒绝下单，
//! 只有仓位与订单都同步后才能进入 READY。

use polymarket_rust::fast::trading_gate::{TradingGate, TradingGateState};

#[test]
fn p0_007_gate_default_not_ready() {
    let gate = TradingGate::new();
    assert_ne!(gate.state(), TradingGateState::Ready);
    assert!(gate.ensure_ready().is_err(), "默认状态不应放行交易");
}

#[test]
fn p0_007_ensure_ready_blocks_reconciling() {
    let gate = TradingGate::new();
    gate.set_state(TradingGateState::Reconciling);
    assert!(gate.ensure_ready().is_err(), "对账中不应放行交易");
}

#[test]
fn p0_011_reconcile_both_sync_enters_ready() {
    let gate = TradingGate::new();
    gate.reconcile_complete(true, true);
    assert_eq!(gate.state(), TradingGateState::Ready);
    assert!(gate.ensure_ready().is_ok(), "对账通过后应进入 READY 并放行");
}

#[test]
fn p0_011_reconcile_any_mismatch_stays_reconciling() {
    for (pos, ord) in [(false, true), (true, false), (false, false)] {
        let gate = TradingGate::new();
        gate.reconcile_complete(pos, ord);
        assert_eq!(
            gate.state(),
            TradingGateState::Reconciling,
            "positions_in_sync={pos}, orders_in_sync={ord} 应保持 Reconciling"
        );
        assert!(gate.ensure_ready().is_err());
    }
}

#[test]
fn p0_011_halt_blocks_trading() {
    let gate = TradingGate::new();
    gate.set_state(TradingGateState::Ready);
    gate.halt("manual halt");
    assert_eq!(gate.state(), TradingGateState::Halted);
    assert!(gate.ensure_ready().is_err(), "熔断后应拒绝下单");
}
