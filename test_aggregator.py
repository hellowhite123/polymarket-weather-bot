"""信号聚合器单元测试 + 端到端验证。

覆盖:
  1. 冲突仲裁 (同方向合并 / 反方向胜出 / 双向相消)
  2. 置信度融合 (加权平均 / 目标价格)
  3. 风控闸门 (结算窗口 / 每日亏损 / 仓位上限 / 全局暴露)
  4. 仓位计算 (Kelly 正/负预期 / 固定比例)
  5. 多市场并发聚合
  6. 指令序列化往返
  7. 空输入边界
"""

import os
import sys
import traceback
from datetime import datetime, timedelta, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from strategy_engine.aggregator.aggregator import AggregatorParams, SignalAggregator
from strategy_engine.aggregator.models import (
    AggregatedOrder,
    OrderStatus,
    RiskState,
)
from strategy_engine.aggregator.position_sizer import PositionSizer
from strategy_engine.models import EventType, SignalDirection, TradingSignal


# ================================================================
# 辅助
# ================================================================


def make_signal(
    strategy: str,
    direction: SignalDirection,
    token_id: str,
    confidence: float,
    target_price: float,
    market_price: float,
    event_type: str = "rain",
    location: str = "NewYork",
) -> TradingSignal:
    return TradingSignal(
        strategy=strategy,
        direction=direction,
        token_id=token_id,
        event_type=EventType(event_type),
        location=location,
        target_price=target_price,
        market_price=market_price,
        confidence=confidence,
    )


# ================================================================
# 冲突仲裁
# ================================================================


def test_same_direction_merge():
    """同方向信号应合并, 置信度加权融合。"""
    signals = [
        make_signal("forecast_deviation", SignalDirection.BUY_YES, "tok_A", 0.80, 0.85, 0.70),
        make_signal("multi_source", SignalDirection.BUY_YES, "tok_A", 0.90, 0.88, 0.70),
    ]
    agg = SignalAggregator(AggregatorParams())
    orders = agg.aggregate(signals)

    assert len(orders) == 1
    o = orders[0]
    assert o.direction == SignalDirection.BUY_YES
    assert o.is_actionable
    assert "forecast_deviation" in o.contributing_strategies
    assert "multi_source" in o.contributing_strategies
    # 加权: (1.0*0.80 + 1.2*0.90) / (1.0+1.2) = 1.88/2.2 = 0.8545
    assert abs(o.confidence - 0.8545) < 0.01
    print(f"  PASS: 同方向合并 conf={o.confidence:.4f} "
          f"target={o.target_price:.4f} qty={o.quantity:.2f}")


def test_conflict_winner():
    """反方向冲突, 加权置信度高者胜出。"""
    signals = [
        make_signal("forecast_deviation", SignalDirection.BUY_YES, "tok_B", 0.95, 0.90, 0.50),
        make_signal("multi_source", SignalDirection.BUY_NO, "tok_B", 0.40, 0.10, 0.50),
    ]
    # YES score = 1.0 * 0.95 = 0.95
    # NO  score = 1.2 * 0.40 = 0.48  -> diff=0.47 > 0.15 -> YES wins
    agg = SignalAggregator(AggregatorParams())
    orders = agg.aggregate(signals)

    assert len(orders) == 1
    assert orders[0].direction == SignalDirection.BUY_YES
    assert orders[0].is_actionable
    print(f"  PASS: 冲突仲裁 -> {orders[0].direction.value} 胜出")


def test_conflict_void():
    """双向旗鼓相当 (diff < 阈值) -> 相消不交易。"""
    signals = [
        make_signal("forecast_deviation", SignalDirection.BUY_YES, "tok_C", 0.60, 0.65, 0.50),
        make_signal("multi_source", SignalDirection.BUY_NO, "tok_C", 0.55, 0.45, 0.50),
    ]
    # YES score = 1.0 * 0.60 = 0.60
    # NO  score = 1.2 * 0.55 = 0.66  -> diff=0.06 < 0.15 -> void
    agg = SignalAggregator(AggregatorParams())
    orders = agg.aggregate(signals)

    assert len(orders) == 1
    assert orders[0].status == OrderStatus.CONFLICT_VOID
    assert orders[0].block_reason
    print(f"  PASS: 冲突相消 -> {orders[0].block_reason}")


# ================================================================
# 置信度与风控
# ================================================================


def test_low_confidence_blocked():
    """综合置信度低于下限 -> 拦截。"""
    signals = [
        make_signal("forecast_deviation", SignalDirection.BUY_YES, "tok_D", 0.30, 0.60, 0.55),
    ]
    agg = SignalAggregator(AggregatorParams(min_aggregated_confidence=0.5))
    orders = agg.aggregate(signals)

    assert orders[0].status == OrderStatus.BLOCKED
    assert "置信度" in orders[0].block_reason
    print(f"  PASS: 置信度不足 -> {orders[0].block_reason}")


def test_settlement_window_blocked():
    """距结算 < lock_hours -> 禁新开仓。"""
    signals = [
        make_signal("forecast_deviation", SignalDirection.BUY_YES, "tok_E", 0.85, 0.80, 0.60),
    ]
    risk_state = RiskState(
        settlement_times={
            "tok_E": datetime.now(timezone.utc) + timedelta(hours=1)
        }
    )
    agg = SignalAggregator(AggregatorParams(settlement_lock_hours=2.0))
    orders = agg.aggregate(signals, risk_state)

    assert orders[0].status == OrderStatus.BLOCKED
    assert "距结算" in orders[0].block_reason
    print(f"  PASS: 结算窗口拦截 -> {orders[0].block_reason}")


def test_daily_loss_circuit_breaker():
    """当日亏损触及上限 -> 熔断。"""
    signals = [
        make_signal("forecast_deviation", SignalDirection.BUY_YES, "tok_F", 0.85, 0.80, 0.60),
    ]
    risk_state = RiskState(daily_pnl=-150.0, daily_circuit_breaker=True)
    agg = SignalAggregator(AggregatorParams(daily_loss_limit=100.0))
    orders = agg.aggregate(signals, risk_state)

    assert orders[0].status == OrderStatus.BLOCKED
    print(f"  PASS: 每日亏损熔断 -> {orders[0].block_reason}")


def test_position_limit_blocked():
    """单市场持仓 + 新增 > 上限 -> 拦截。"""
    signals = [
        make_signal("forecast_deviation", SignalDirection.BUY_YES, "tok_G", 0.95, 0.90, 0.20),
    ]
    risk_state = RiskState(positions={"tok_G": 950.0})
    agg = SignalAggregator(AggregatorParams(
        max_position_per_market=1000.0,
        max_global_exposure=100000.0,   # 放宽, 隔离 position_limit
        base_capital=5000.0,
        kelly_fraction=0.5,
    ))
    orders = agg.aggregate(signals, risk_state)

    blocked = [o for o in orders if o.status == OrderStatus.BLOCKED]
    assert blocked, f"应被仓位上限拦截, 实际 qty={orders[0].quantity:.2f}"
    failed_rules = [r.rule for r in blocked[0].risk_checks if not r.passed]
    assert "position_limit" in failed_rules, f"应触发 position_limit, 实际 {failed_rules}"
    print(f"  PASS: 仓位上限拦截 -> {blocked[0].block_reason}")


def test_global_exposure_blocked():
    """全局暴露 + 新增 > 上限 -> 拦截 (单市场上限放宽以隔离变量)。"""
    signals = [
        make_signal("forecast_deviation", SignalDirection.BUY_YES, "tok_H", 0.95, 0.90, 0.10),
    ]
    risk_state = RiskState(positions={"tok_other": 4800.0})
    agg = SignalAggregator(AggregatorParams(
        max_position_per_market=100000.0,   # 放宽, 让 position_limit 不触发
        max_global_exposure=5000.0,
        base_capital=5000.0,
        kelly_fraction=0.5,
    ))
    orders = agg.aggregate(signals, risk_state)

    blocked = [o for o in orders if o.status == OrderStatus.BLOCKED]
    assert blocked, f"应被全局暴露拦截, 实际 qty={orders[0].quantity:.2f}"
    failed_rules = [r.rule for r in blocked[0].risk_checks if not r.passed]
    assert "global_exposure" in failed_rules, f"应触发 global_exposure, 实际 {failed_rules}"
    print(f"  PASS: 全局暴露拦截 -> {blocked[0].block_reason}")


# ================================================================
# 仓位计算
# ================================================================


def test_kelly_positive():
    """Kelly 正预期 -> 计算仓位。"""
    sizer = PositionSizer(method="kelly", kelly_fraction=0.25, base_capital=1000.0)
    # p=0.80, price=0.30 -> b=2.333, f*=0.714, f=0.179, qty=595
    qty, detail = sizer.size(0.80, 0.30, 1.0, SignalDirection.BUY_YES)
    assert qty > 0
    assert abs(detail["f_star"] - 0.7143) < 0.01
    assert abs(qty - 595.2) < 5.0
    print(f"  PASS: Kelly正预期 qty={qty:.2f} f*={detail['f_star']:.4f} "
          f"b={detail['b']:.4f}")


def test_kelly_negative():
    """Kelly 负预期 (赔率太低) -> 不下单。"""
    sizer = PositionSizer(method="kelly", kelly_fraction=0.25, base_capital=1000.0)
    # p=0.10, price=0.80 -> b=0.25, f*=-3.5 -> qty=0
    qty, detail = sizer.size(0.10, 0.80, 1.0, SignalDirection.BUY_YES)
    assert qty == 0.0
    assert "f_star" in detail and detail["f_star"] <= 0
    print(f"  PASS: Kelly负预期不下单 f*={detail['f_star']:.4f}")


def test_fixed_fraction():
    """固定比例模式。"""
    sizer = PositionSizer(
        method="fixed_fraction", fixed_fraction=0.05, base_capital=1000.0
    )
    # qty = 1000 * 0.05 * 0.80 / 0.30 = 133.33
    qty, detail = sizer.size(0.80, 0.30, 0.80, SignalDirection.BUY_YES)
    assert abs(qty - 133.33) < 0.5
    print(f"  PASS: 固定比例 qty={qty:.2f}")


def test_kelly_buy_no():
    """买 NO 的 Kelly 计算 (赔率方向反转)。"""
    sizer = PositionSizer(method="kelly", kelly_fraction=0.25, base_capital=1000.0)
    # 买 NO: 成本=1-price=0.70, 赢得 1, 赔率 b=price/(1-price)=0.30/0.70=0.4286
    # p=0.80 -> f*=(0.4286*0.80-0.20)/0.4286=(0.3429-0.20)/0.4286=0.3333
    qty, detail = sizer.size(0.80, 0.30, 1.0, SignalDirection.BUY_NO)
    assert qty > 0
    assert abs(detail["f_star"] - 0.3333) < 0.01
    print(f"  PASS: Kelly买NO qty={qty:.2f} f*={detail['f_star']:.4f} "
          f"b={detail['b']:.4f}")


def test_p0_kelly_uses_model_probability():
    """P0-08: Kelly 用 model_probability 作为 p，confidence 只作缩放。"""
    sizer = PositionSizer(method="kelly", kelly_fraction=0.25, base_capital=1000.0)
    qty_full, d1 = sizer.size(0.80, 0.30, 1.0, SignalDirection.BUY_YES)
    qty_half, d2 = sizer.size(0.80, 0.30, 0.50, SignalDirection.BUY_YES)
    # f_star 相同（都由 model_probability 决定），qty 按 confidence 线性缩放
    assert abs(d1["f_star"] - d2["f_star"]) < 1e-9
    assert abs(qty_half - qty_full * 0.5) < 1e-6
    print("  PASS: P0-08 Kelly 用概率, confidence 只缩放")


def test_p0_confidence_is_not_probability():
    """P0-08: confidence 降低不应改变 Kelly 的 f_star（它不是概率）。"""
    sizer = PositionSizer(method="kelly", kelly_fraction=0.25, base_capital=1000.0)
    q1, d1 = sizer.size(0.70, 0.40, 0.90, SignalDirection.BUY_YES)
    q2, d2 = sizer.size(0.70, 0.40, 0.30, SignalDirection.BUY_YES)
    assert abs(d1["f_star"] - d2["f_star"]) < 1e-9
    assert abs(q2 - q1 * (0.30 / 0.90)) < 1e-6
    print("  PASS: P0-08 confidence 不是概率")


# ================================================================
# 综合
# ================================================================


def test_multi_market_aggregation():
    """多市场并发聚合。"""
    signals = [
        make_signal("forecast_deviation", SignalDirection.BUY_YES, "tok_M1", 0.85, 0.80, 0.60),
        make_signal("multi_source", SignalDirection.BUY_YES, "tok_M2", 0.90, 0.75, 0.50),
        make_signal("forecast_deviation", SignalDirection.BUY_NO, "tok_M3", 0.88, 0.20, 0.70),
    ]
    agg = SignalAggregator(AggregatorParams())
    orders = agg.aggregate(signals)

    assert len(orders) == 3
    actionable = [o for o in orders if o.is_actionable]
    token_ids = {o.token_id for o in orders}
    assert token_ids == {"tok_M1", "tok_M2", "tok_M3"}
    print(f"  PASS: 多市场聚合 -> {len(orders)} 市场, "
          f"{len(actionable)} 可执行")


def test_empty_input():
    """空输入边界。"""
    agg = SignalAggregator(AggregatorParams())
    orders = agg.aggregate([])
    assert orders == []
    print(f"  PASS: 空输入 -> 0 个指令")


def test_order_serialization():
    """AggregatedOrder to_dict / from_dict 往返。"""
    signals = [
        make_signal("forecast_deviation", SignalDirection.BUY_YES, "tok_S", 0.85, 0.80, 0.60),
    ]
    agg = SignalAggregator(AggregatorParams())
    orders = agg.aggregate(signals)

    d = orders[0].to_dict()
    restored = AggregatedOrder.from_dict(d)
    assert restored.token_id == orders[0].token_id
    assert restored.direction == orders[0].direction
    assert abs(restored.confidence - orders[0].confidence) < 1e-9
    assert abs(restored.quantity - orders[0].quantity) < 1e-9
    print(f"  PASS: 序列化往返 token={restored.token_id} "
          f"qty={restored.quantity:.2f}")


def test_risk_state_helpers():
    """RiskState 辅助方法。"""
    state = RiskState(
        positions={"A": 100.0, "B": -50.0, "C": 200.0},
        daily_pnl=30.0,
    )
    assert state.has_position("A")
    assert not state.has_position("D")
    assert state.get_position("B") == -50.0
    # 全局暴露 = |100| + |-50| + |200| = 350
    assert abs(state.get_global_exposure() - 350.0) < 1e-9
    assert not state.is_trading_halted()
    state.manual_halt = True
    assert state.is_trading_halted()
    print(f"  PASS: RiskState 辅助方法 exposure={state.get_global_exposure():.2f}")


# ================================================================
# 运行
# ================================================================


if __name__ == "__main__":
    tests = [
        test_same_direction_merge,
        test_conflict_winner,
        test_conflict_void,
        test_low_confidence_blocked,
        test_settlement_window_blocked,
        test_daily_loss_circuit_breaker,
        test_position_limit_blocked,
        test_global_exposure_blocked,
        test_kelly_positive,
        test_kelly_negative,
        test_fixed_fraction,
        test_kelly_buy_no,
        test_multi_market_aggregation,
        test_empty_input,
        test_order_serialization,
        test_risk_state_helpers,
    ]

    print("=" * 60)
    print("  信号聚合器测试")
    print("=" * 60)

    passed = 0
    failed = 0
    for test in tests:
        try:
            print(f"\n[{test.__name__}]")
            test()
            passed += 1
        except Exception as e:
            print(f"  FAIL: {e}")
            traceback.print_exc()
            failed += 1

    print(f"\n{'=' * 60}")
    print(f"  结果: {passed} 通过 / {failed} 失败 / {len(tests)} 总计")
    print(f"{'=' * 60}")
    sys.exit(1 if failed else 0)
