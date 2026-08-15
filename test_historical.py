"""历史统计策略单元测试。

测试覆盖:
  1. ClimateTracker 基本操作 (录入/查询/持久化)
  2. 气候学平滑 (月份加权)
  3. 混合权重计算 (lead time 衰减)
  4. 季节性偏差信号生成 (BUY_YES / BUY_NO)
  5. 无气候数据时不发信号
  6. 样本不足时不发信号
  7. 偏差不足时不发信号
  8. 批量评估
  9. 信号元数据完整性
 10. 端到端: 多策略信号 -> 聚合器
"""

import json
import os
import sys
import tempfile
from datetime import datetime, timedelta, timezone

# 确保项目根目录在 path 中
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from strategy_engine.config import HistoricalParams
from strategy_engine.models import (
    EventType,
    MarketSnapshot,
    SignalDirection,
    TradingSignal,
)
from strategy_engine.statistics.climate_tracker import (
    ClimateStats,
    ClimateTracker,
)
from strategy_engine.strategies.historical_statistical import (
    HistoricalStatisticalStrategy,
)


# ============================================================
# 测试辅助
# ============================================================


def make_tracker_with_data(
    location: str = "NewYork",
    event_type: EventType = EventType.RAIN,
    month: int = 8,
    n_samples: int = 10,
    base_prob: float = 0.30,
    spread: float = 0.08,
) -> ClimateTracker:
    """构造有数据的 ClimateTracker。"""
    import random
    random.seed(42)
    tracker = ClimateTracker()
    for _ in range(n_samples):
        prob = max(0.0, min(1.0, base_prob + random.gauss(0, spread)))
        tracker.add_observation(location, event_type, month, round(prob, 4))
    return tracker


def make_forecast(prob: float = 0.70, location: str = "NewYork"):
    """构造一个简单的 WeatherForecast Mock 对象。"""
    from weather_data_ingestion.models import (
        WeatherForecast,
        ForecastPeriod,
        Temperature,
        Precipitation,
        SourceMeta,
    )

    now = datetime.now(timezone.utc)
    periods = []
    for i in range(6):
        period = ForecastPeriod(
            valid_time=(now + timedelta(hours=i * 4)).strftime("%Y-%m-%dT%H:%M:%SZ"),
            temperature=Temperature(value_c=22.0 + i),
            precipitation=Precipitation(
                probability=prob,
                amount_mm=prob * 5,
                type="rain" if prob > 0.3 else "none",
            ),
        )
        periods.append(period)

    return WeatherForecast(
        location_name=location,
        lat=40.71,
        lon=-74.01,
        source=SourceMeta(source="noaa", fetch_time=now.strftime("%Y-%m-%dT%H:%M:%SZ")),
        periods=periods,
    )


def make_market(yes_price: float = 0.75, token_id: str = "tok_test") -> MarketSnapshot:
    """构造 MarketSnapshot。"""
    return MarketSnapshot(
        token_id=token_id,
        yes_price=yes_price,
        no_price=round(1.0 - yes_price, 4),
        market_volume=10000.0,
        spread=0.01,
    )


# ============================================================
# 1. ClimateTracker 基本操作
# ============================================================


def test_climate_tracker_basic():
    """测试基本录入和查询。"""
    tracker = ClimateTracker()
    tracker.add_observation("NYC", EventType.RAIN, 7, 0.30)
    tracker.add_observation("NYC", EventType.RAIN, 7, 0.40)
    tracker.add_observation("NYC", EventType.RAIN, 7, 0.35)

    stats = tracker.get_stats("NYC", EventType.RAIN, 7)
    assert stats is not None, "应有统计数据"
    assert stats.count == 3
    assert 0.34 < stats.mean < 0.36, f"mean={stats.mean}"
    assert stats.std > 0, "应有标准差"

    result = tracker.get_climatology("NYC", EventType.RAIN, 7)
    assert result is not None
    mean, std, count = result
    assert count == 3

    # 查询不存在的 key
    none_result = tracker.get_climatology("LA", EventType.RAIN, 7)
    assert none_result is None

    print(f"  PASS: 基本操作 count={stats.count} mean={stats.mean:.4f} std={stats.std:.4f}")


def test_climate_tracker_persistence():
    """测试 JSON 持久化。"""
    tracker = ClimateTracker()
    for i in range(10):
        tracker.add_observation("Boston", EventType.SNOW, 1, 0.60 + i * 0.01)

    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".json", delete=False
    ) as f:
        tmp_path = f.name

    try:
        tracker.save(tmp_path)
        loaded = ClimateTracker.load(tmp_path)

        assert loaded is not None
        stats = loaded.get_stats("Boston", EventType.SNOW, 1)
        assert stats is not None
        assert stats.count == 10
        assert abs(stats.mean - 0.645) < 0.01
        print(f"  PASS: 持久化 count={stats.count} mean={stats.mean:.4f}")
    finally:
        os.unlink(tmp_path)


def test_climate_tracker_smoothed():
    """测试平滑气候学（月份加权）。"""
    tracker = ClimateTracker()
    # 7 月: 0.40
    for _ in range(5):
        tracker.add_observation("Miami", EventType.RAIN, 7, 0.40)
    # 6 月: 0.30
    for _ in range(5):
        tracker.add_observation("Miami", EventType.RAIN, 6, 0.30)
    # 8 月: 0.50
    for _ in range(5):
        tracker.add_observation("Miami", EventType.RAIN, 8, 0.50)

    result = tracker.get_smoothed_climatology("Miami", EventType.RAIN, 7, window=1)
    assert result is not None
    mean, std, count = result

    # 平滑后的均值应该在 0.30~0.50 之间，且接近 0.40（中心月权重最高）
    assert 0.35 < mean < 0.45, f"smoothed mean={mean}"
    print(f"  PASS: 平滑 mean={mean:.4f} std={std:.4f} count={count}")


# ============================================================
# 2. 混合权重计算
# ============================================================


def test_blend_weight_short_lead():
    """短 lead time -> 预报权重高。"""
    tracker = ClimateTracker()
    strategy = HistoricalStatisticalStrategy(tracker, HistoricalParams())

    w = strategy._compute_blend_weight(1.0)  # 1 小时
    assert w >= 0.84, f"1h lead weight={w}, 应接近上限 0.85"
    print(f"  PASS: 1h lead -> forecast_weight={w:.4f}")


def test_blend_weight_long_lead():
    """长 lead time -> 气候学权重高。"""
    tracker = ClimateTracker()
    strategy = HistoricalStatisticalStrategy(tracker, HistoricalParams())

    w = strategy._compute_blend_weight(168.0)  # 7 天
    assert w <= 0.20, f"168h lead weight={w}, 应接近下限 0.15"
    print(f"  PASS: 168h lead -> forecast_weight={w:.4f}")


def test_blend_weight_medium_lead():
    """中等 lead time -> 中间权重。"""
    tracker = ClimateTracker()
    strategy = HistoricalStatisticalStrategy(tracker, HistoricalParams())

    w = strategy._compute_blend_weight(72.0)  # 3 天
    assert 0.55 < w < 0.75, f"72h lead weight={w}, 应在 0.6 附近"
    print(f"  PASS: 72h lead -> forecast_weight={w:.4f}")


# ============================================================
# 3. 信号生成
# ============================================================


def test_buy_yes_signal():
    """预报概率高 + 气候学低 + 市场跟随气候学 -> BUY_YES。

    场景: 预报说 70% 降雨，历史同期 30%，市场定价 35%（跟随气候学）。
    混合概率约 50-55%，偏差 +15~20% -> BUY_YES。
    """
    tracker = make_tracker_with_data(
        base_prob=0.30, n_samples=10, month=datetime.now().month,
    )
    strategy = HistoricalStatisticalStrategy(
        tracker, HistoricalParams(
            min_deviation_abs=0.01,
            min_confidence=0.0,
            min_climatology_samples=5,
        ),
    )

    forecast = make_forecast(prob=0.70)
    market = make_market(yes_price=0.35, token_id="tok_buy_yes")

    signal = strategy.evaluate_sync(
        forecast, market, EventType.RAIN, target_hours_ahead=24,
    )

    assert signal is not None, "应生成信号"
    assert signal.direction == SignalDirection.BUY_YES
    assert signal.deviation > 0, f"deviation={signal.deviation}, 应为正"
    assert signal.confidence > 0.3
    meta = signal.metadata
    assert meta["forecast_prob"] == 0.70
    assert 0.25 < meta["climatology_prob"] < 0.40
    assert meta["seasonal_deviation"] > 0.3
    print(
        f"  PASS: BUY_YES dev={signal.deviation:+.4f} "
        f"conf={signal.confidence:.4f} blend={meta['blended_prob']:.4f}"
    )


def test_buy_no_signal():
    """预报概率低 + 气候学高 + 市场跟随预报 -> BUY_NO。

    场景: 预报说 20% 降雨，历史同期 60%，市场定价 25%（跟随预报）。
    混合概率约 35-40%，偏差 -10~15% -> BUY_NO。
    """
    tracker = make_tracker_with_data(
        base_prob=0.60, n_samples=10, month=datetime.now().month,
    )
    strategy = HistoricalStatisticalStrategy(
        tracker, HistoricalParams(
            min_deviation_abs=0.01,
            min_confidence=0.0,
            min_climatology_samples=5,
        ),
    )

    forecast = make_forecast(prob=0.20)
    market = make_market(yes_price=0.50, token_id="tok_buy_no")

    signal = strategy.evaluate_sync(
        forecast, market, EventType.RAIN, target_hours_ahead=24,
    )

    assert signal is not None, "应生成信号"
    assert signal.direction == SignalDirection.BUY_NO
    assert signal.deviation < 0, f"deviation={signal.deviation}, 应为负"
    print(
        f"  PASS: BUY_NO dev={signal.deviation:+.4f} "
        f"conf={signal.confidence:.4f}"
    )


def test_no_climatology_data():
    """无气候学数据 -> 不发信号。"""
    tracker = ClimateTracker()  # 空
    strategy = HistoricalStatisticalStrategy(tracker, HistoricalParams())

    forecast = make_forecast(prob=0.50)
    market = make_market(yes_price=0.45)

    signal = strategy.evaluate_sync(
        forecast, market, EventType.RAIN, target_hours_ahead=24,
    )
    assert signal is None, "无气候数据时不应生成信号"
    print("  PASS: 无气候数据 -> 不发信号")


def test_insufficient_samples():
    """样本不足 -> 不发信号。"""
    tracker = ClimateTracker()
    tracker.add_observation("NYC", EventType.RAIN, datetime.now().month, 0.30)
    tracker.add_observation("NYC", EventType.RAIN, datetime.now().month, 0.35)
    # 只有 2 个样本 < min_climatology_samples=5

    strategy = HistoricalStatisticalStrategy(
        tracker, HistoricalParams(min_climatology_samples=5),
    )
    forecast = make_forecast(prob=0.70)
    market = make_market(yes_price=0.35)

    signal = strategy.evaluate_sync(
        forecast, market, EventType.RAIN, target_hours_ahead=24,
    )
    assert signal is None, "样本不足时不应生成信号"
    print("  PASS: 样本不足 -> 不发信号")


def test_small_deviation_filtered():
    """偏差不足 -> 不发信号。"""
    # 预报和气候学接近 -> 混合概率接近市场 -> 偏差小
    tracker = make_tracker_with_data(
        base_prob=0.50, n_samples=10, month=datetime.now().month,
    )
    strategy = HistoricalStatisticalStrategy(
        tracker, HistoricalParams(
            min_deviation_abs=0.20,  # 高阈值
            min_confidence=0.0,
            min_climatology_samples=5,
        ),
    )

    forecast = make_forecast(prob=0.52)
    market = make_market(yes_price=0.50)

    signal = strategy.evaluate_sync(
        forecast, market, EventType.RAIN, target_hours_ahead=24,
    )
    assert signal is None, "偏差不足时不应生成信号"
    print("  PASS: 偏差不足 -> 不发信号")


# ============================================================
# 4. 批量评估
# ============================================================


def test_batch_evaluate():
    """批量评估多个 (预报, 市场) 对。"""
    import asyncio

    tracker = make_tracker_with_data(
        base_prob=0.30, n_samples=10, month=datetime.now().month,
    )
    strategy = HistoricalStatisticalStrategy(
        tracker, HistoricalParams(
            min_deviation_abs=0.01,
            min_confidence=0.0,
            min_climatology_samples=5,
        ),
    )

    forecasts = [
        make_forecast(prob=0.70, location="NYC"),
        make_forecast(prob=0.20, location="LA"),
    ]
    markets = [
        make_market(yes_price=0.35, token_id="tok_nyc"),
        make_market(yes_price=0.25, token_id="tok_la"),
    ]
    event_types = [EventType.RAIN, EventType.RAIN]

    signals = asyncio.run(strategy.evaluate_batch(
        forecasts=forecasts,
        markets=markets,
        event_types=event_types,
        target_hours_ahead=24,
    ))

    # NYC 有气候数据 -> 可能有信号
    # LA 无气候数据 -> 无信号
    assert len(signals) <= 1, f"应最多 1 个信号 (只有 NYC 有数据), 实际 {len(signals)}"
    if signals:
        assert signals[0].location == "NYC"
    print(f"  PASS: 批量评估 {len(forecasts)} 个输入 -> {len(signals)} 个信号")


# ============================================================
# 5. 信号元数据完整性
# ============================================================


def test_signal_metadata():
    """信号元数据包含所有必要字段。"""
    tracker = make_tracker_with_data(
        base_prob=0.30, n_samples=15, month=datetime.now().month,
    )
    strategy = HistoricalStatisticalStrategy(
        tracker, HistoricalParams(
            min_deviation_abs=0.01,
            min_confidence=0.0,
        ),
    )

    forecast = make_forecast(prob=0.70)
    market = make_market(yes_price=0.35, token_id="tok_meta")

    signal = strategy.evaluate_sync(
        forecast, market, EventType.RAIN, target_hours_ahead=48,
    )

    assert signal is not None
    meta = signal.metadata
    required_keys = [
        "forecast_prob", "climatology_prob", "climatology_std",
        "climatology_count", "blended_prob", "forecast_weight",
        "climatology_weight", "seasonal_deviation", "target_hours", "month",
    ]
    for key in required_keys:
        assert key in meta, f"元数据缺少 {key}"

    assert meta["target_hours"] == 48
    assert meta["forecast_prob"] == 0.70
    assert meta["climatology_count"] >= 10
    assert 0 < meta["forecast_weight"] < 1
    assert meta["climatology_weight"] == 1.0 - meta["forecast_weight"]
    print(f"  PASS: 元数据完整 ({len(meta)} 个字段)")


# ============================================================
# 6. 端到端: 多策略信号 -> 聚合器
# ============================================================


def test_e2e_with_aggregator():
    """端到端: 历史统计信号 -> 聚合器 -> 可执行指令。"""
    from strategy_engine.aggregator.aggregator import (
        SignalAggregator,
        AggregatorParams,
    )
    from strategy_engine.aggregator.models import RiskState

    tracker = make_tracker_with_data(
        base_prob=0.30, n_samples=15, month=datetime.now().month,
    )
    strategy = HistoricalStatisticalStrategy(
        tracker, HistoricalParams(
            min_deviation_abs=0.01,
            min_confidence=0.3,
        ),
    )

    # 构造两个市场的信号 (location 与 tracker 一致)
    forecasts = [
        make_forecast(prob=0.75, location="NewYork"),
        make_forecast(prob=0.15, location="NewYork"),
    ]
    markets = [
        make_market(yes_price=0.35, token_id="tok_e2e_yes"),
        make_market(yes_price=0.50, token_id="tok_e2e_no"),
    ]

    import asyncio
    signals = asyncio.run(strategy.evaluate_batch(
        forecasts=forecasts,
        markets=markets,
        event_types=[EventType.RAIN, EventType.RAIN],
        target_hours_ahead=24,
    ))

    print(f"  策略产出 {len(signals)} 个信号")
    for s in signals:
        print(
            f"    {s.direction.value} {s.token_id} "
            f"dev={s.deviation:+.4f} conf={s.confidence:.4f}"
        )

    # 聚合
    aggregator = SignalAggregator(AggregatorParams(
        base_capital=2000.0,
        max_position_per_market=2000.0,
        kelly_fraction=0.5,
    ))
    orders = aggregator.aggregate(signals, RiskState())

    actionable = [o for o in orders if o.is_actionable]
    print(f"  聚合结果: {len(orders)} 个指令, {len(actionable)} 个可执行")
    for o in orders:
        print(
            f"    {o.direction.value} {o.token_id} "
            f"qty={o.quantity:.2f} conf={o.confidence:.4f} "
            f"actionable={o.is_actionable}"
        )

    assert len(orders) >= 1, "应有至少 1 个市场"
    assert len(actionable) >= 1, "应有至少 1 个可执行"
    print(f"  PASS: 端到端 -> {len(actionable)} 个可执行指令")


# ============================================================
# 运行所有测试
# ============================================================


if __name__ == "__main__":
    print("=" * 60)
    print("  历史统计策略单元测试")
    print("=" * 60)
    print()

    tests = [
        ("ClimateTracker 基本操作", test_climate_tracker_basic),
        ("ClimateTracker 持久化", test_climate_tracker_persistence),
        ("ClimateTracker 平滑", test_climate_tracker_smoothed),
        ("混合权重-短lead", test_blend_weight_short_lead),
        ("混合权重-长lead", test_blend_weight_long_lead),
        ("混合权重-中lead", test_blend_weight_medium_lead),
        ("BUY_YES 信号", test_buy_yes_signal),
        ("BUY_NO 信号", test_buy_no_signal),
        ("无气候数据", test_no_climatology_data),
        ("样本不足", test_insufficient_samples),
        ("偏差不足", test_small_deviation_filtered),
        ("批量评估", test_batch_evaluate),
        ("信号元数据", test_signal_metadata),
        ("端到端聚合", test_e2e_with_aggregator),
    ]

    passed = 0
    failed = 0

    for name, func in tests:
        print(f"\n[{name}]")
        try:
            func()
            passed += 1
        except Exception as e:
            print(f"  FAIL: {e}")
            import traceback
            traceback.print_exc()
            failed += 1

    print(f"\n{'=' * 60}")
    print(f"  结果: {passed}/{passed + failed} 通过")
    print(f"{'=' * 60}")
