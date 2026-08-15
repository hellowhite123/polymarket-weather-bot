"""P0 修复回归测试 — 概率语义 + 回测防泄漏。

覆盖：
  - P0-05: 市场快照不选未来数据（look-ahead 防护）
  - P0-06: 回测不用 actual_data 当 oracle 概率
  - P0-07: 天气原始值转事件概率（normal_cdf / probability_above）
"""

import sys
import os
from datetime import datetime, timedelta, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from strategy_engine.probability import (
    normal_cdf,
    probability_above,
    probability_below,
    EventProbability,
)
from strategy_engine.backtest.market_simulator import MarketSimulator
from strategy_engine.backtest.models import HistoricalEvent, EventOutcome

PASS = 0
FAIL = 0


def check(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print(f"  [PASS] {name}")
    else:
        FAIL += 1
        print(f"  [FAIL] {name}  {detail}")


def test_probability_functions():
    print("\n== P0-07: 概率转换 ==")
    # normal_cdf(0) = 0.5
    check("normal_cdf(0)=0.5", abs(normal_cdf(0.0) - 0.5) < 1e-9)
    # P(X > 0) = 0.5 (mean=0, std=1)
    check("probability_above(0,1,0)=0.5",
          abs(probability_above(0.0, 1.0, 0.0) - 0.5) < 1e-9)
    # P(X < 0) = 0.5
    check("probability_below(0,1,0)=0.5",
          abs(probability_below(0.0, 1.0, 0.0) - 0.5) < 1e-9)
    # 温度阈值：mean=32.2, std=1.5, threshold=32.2 → P(>32.2)=0.5
    p = probability_above(32.2, 1.5, 32.2)
    check("P(T>32.2)=0.5", abs(p - 0.5) < 1e-9, str(p))
    # mean 远高于阈值 → 概率接近 1
    p = probability_above(40.0, 1.5, 32.2)
    check("P(T>32.2|mean=40)≈1", p > 0.99, str(p))
    # mean 远低于阈值 → 概率接近 0
    p = probability_above(20.0, 1.5, 32.2)
    check("P(T>32.2|mean=20)≈0", p < 0.01, str(p))
    # std <= 0 退化为阶跃
    check("std=0 阶跃", probability_above(35.0, 0.0, 32.2) == 1.0)
    check("std=0 阶跃(不超)", probability_above(30.0, 0.0, 32.2) == 0.0)


def test_event_probability():
    print("\n== P0-07: EventProbability ==")
    ep = EventProbability(probability=1.5, uncertainty=0.1, source="noaa")
    check("clamped 裁剪到 1.0", ep.clamped().probability == 1.0)
    ep2 = EventProbability(probability=-0.3)
    check("clamped 裁剪到 0.0", ep2.clamped().probability == 0.0)
    d = EventProbability(probability=0.7).to_dict()
    check("to_dict 含 probability", d["probability"] == 0.7)


def _make_event(market_history, forecast_history=None):
    return HistoricalEvent(
        token_id="bt_test",
        location="NewYork",
        event_type="rain",
        deadline=datetime.now(timezone.utc) + timedelta(days=1),
        outcome=EventOutcome.PENDING,
        market_history=market_history,
        forecast_history=forecast_history or [],
        actual_data={"prob": 0.9},  # oracle 值，信号生成不得使用
    )


def test_no_future_snapshot():
    print("\n== P0-05: 未来快照防护 ==")
    now = datetime(2026, 8, 1, 12, 0, tzinfo=timezone.utc)
    history = [
        {"timestamp": "2026-08-01T11:00:00+00:00", "yes_price": 0.60,
         "no_price": 0.40, "spread": 0.01},
        {"timestamp": "2026-08-01T13:00:00+00:00", "yes_price": 0.80,
         "no_price": 0.20, "spread": 0.01},  # 未来快照
    ]
    sim = MarketSimulator()
    event = _make_event(history)
    prices = sim.get_market_price(event, now)
    check("当前 12:00 应取 11:00 快照", prices is not None and prices[0] == 0.60,
          str(prices))
    check("不应取 13:00 未来快照", prices is not None and prices[0] != 0.80,
          str(prices))


def test_no_future_snapshot_all_future():
    print("\n== P0-05b: 全未来快照返回 None ==")
    now = datetime(2026, 8, 1, 10, 0, tzinfo=timezone.utc)
    history = [
        {"timestamp": "2026-08-01T11:00:00+00:00", "yes_price": 0.60,
         "no_price": 0.40, "spread": 0.01},
    ]
    sim = MarketSimulator()
    event = _make_event(history)
    prices = sim.get_market_price(event, now)
    check("全是未来快照返回 None", prices is None, str(prices))


def test_forecast_asof():
    print("\n== P0-06: as-of 预报概率 ==")
    now = datetime(2026, 8, 1, 12, 0, tzinfo=timezone.utc)
    forecast = [
        {"timestamp": "2026-08-01T11:00:00+00:00", "prob": 0.55},
        {"timestamp": "2026-08-01T13:00:00+00:00", "prob": 0.90},  # 未来预报
    ]
    event = _make_event([], forecast)
    p = event.get_forecast_probability_at(now)
    check("取 11:00 预报概率 0.55", p is not None and abs(p - 0.55) < 1e-9, str(p))
    check("不取 13:00 未来预报", p is not None and abs(p - 0.90) > 0.1, str(p))

    # 无可用预报返回 None
    event2 = _make_event([])
    check("无预报返回 None", event2.get_forecast_probability_at(now) is None)


def test_r2_yes_no_routing():
    print("\n== R2-P0-01: YES/NO token 路由 ==")
    from strategy_engine.models import MarketSnapshot, SignalDirection

    market = MarketSnapshot(
        condition_id="c1",
        yes_token_id="YES123",
        no_token_id="NO456",
        yes_price=0.70,
        no_price=0.30,
    )
    # token_for：方向 → 正确 outcome token
    check("BUY_YES → YES token",
          market.token_for(SignalDirection.BUY_YES) == "YES123")
    check("BUY_NO → NO token",
          market.token_for(SignalDirection.BUY_NO) == "NO456")
    # price_for：方向 → 对应价格
    check("BUY_YES → yes_price",
          abs(market.price_for(SignalDirection.BUY_YES) - 0.70) < 1e-9)
    check("BUY_NO → no_price",
          abs(market.price_for(SignalDirection.BUY_NO) - 0.30) < 1e-9)
    # token_id 兼容 property 返回 YES token
    check("token_id 兼容 = YES token", market.token_id == "YES123")


def main():
    print("P0 修复回归测试开始")
    test_probability_functions()
    test_event_probability()
    test_no_future_snapshot()
    test_no_future_snapshot_all_future()
    test_forecast_asof()
    test_r2_yes_no_routing()
    print(f"\n结果: {PASS} 通过, {FAIL} 失败")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
