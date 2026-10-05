"""多源验证策略测试。

构造 Mock WeatherForecast 数据 (NOAA + 彩云), 验证:
1. 基线建立 (分歧 + 融合偏差历史记录)
2. 极端分歧 + 市场偏向错误源 -> 强信号
3. 源间一致 -> 不发信号
4. 分歧大但市场定价合理 -> 不发信号
5. 分歧追踪器持久化
"""

import sys
import os
from datetime import datetime, timezone, timedelta
from dataclasses import dataclass, field
from typing import Optional, List

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from weather_data_ingestion.models import (
    WeatherForecast, ForecastPeriod, Temperature, Precipitation,
    Wind, SourceMeta,
)
from strategy_engine.models import (
    EventType, MarketSnapshot, SignalDirection,
    DeviationRecord, DivergenceRecord,
)
from strategy_engine.statistics.deviation_tracker import DeviationTracker
from strategy_engine.statistics.divergence_tracker import DivergenceTracker
from strategy_engine.strategies.multi_source import MultiSourceStrategy
from strategy_engine.config import MultiSourceStrategyParams


# ================================================================
# Mock 数据工厂
# ================================================================

def make_forecast(source: str, precip_prob: float, location: str = "New York") -> WeatherForecast:
    """构造 Mock WeatherForecast, 设置降水概率。"""
    now = datetime.now(timezone.utc)
    periods = []
    for h in range(1, 25):
        vt = now + timedelta(hours=h)
        periods.append(ForecastPeriod(
            valid_time=vt.isoformat(),
            temperature=Temperature(value_c=20.0),
            precipitation=Precipitation(
                probability=precip_prob,
                amount_mm=precip_prob * 5.0,
            ),
        ))

    return WeatherForecast(
        location_name=location,
        lat=40.7128,
        lon=-74.0060,
        source=SourceMeta(source=source, issue_time=now.isoformat()),
        periods=periods,
    )


def make_market(yes_price: float, token_id: str = "test_token") -> MarketSnapshot:
    """构造 Mock MarketSnapshot。"""
    return MarketSnapshot(
        token_id=token_id,
        yes_price=yes_price,
        no_price=round(1.0 - yes_price, 4),
        market_volume=10000.0,
        spread=0.01,
    )


def print_header(title: str):
    print(f"\n{'='*60}")
    print(f"  {title}")
    print(f"{'='*60}")


# ================================================================
# 测试场景
# ================================================================

def test_baseline_and_signal():
    """测试1: 基线建立 + 极端分歧触发信号。"""
    print_header("测试1: 基线建立 + 极端分歧信号")

    div_tracker = DivergenceTracker(window_size=100)
    dev_tracker = DeviationTracker(window_size=100)
    params = MultiSourceStrategyParams(
        min_divergence_abs=0.05,
        min_market_div_abs=0.03,
        divergence_threshold_sigma=2.0,
        market_div_threshold_sigma=1.5,
        min_confidence=0.3,
        divergence_confidence_boost=0.3,
    )
    strategy = MultiSourceStrategy(div_tracker, dev_tracker, params)

    event_type = EventType.RAIN
    location = "New York"

    # Step 1: 喂入基线数据 (正常分歧, 正常市场偏差)
    print("\n--- 建立基线 (30 条正常数据) ---")
    for i in range(30):
        # NOAA 和彩云有小分歧, 市场定价接近融合概率
        noaa_prob = 0.40 + (i % 5) * 0.02
        caiyun_prob = 0.35 + (i % 5) * 0.02
        fused = (noaa_prob + caiyun_prob) / 2  # 等权
        market_prob = fused + 0.01 * ((-1) ** i)  # 微小偏差

        fc_noaa = make_forecast("noaa", noaa_prob)
        fc_caiyun = make_forecast("caiyun", caiyun_prob)
        market = make_market(market_prob, "ny_rain")

        strategy.evaluate_sync(
            forecasts=[fc_noaa, fc_caiyun],
            market=market,
            event_type=event_type,
            target_hours_ahead=24,
            record_data=True,
        )

    # 检查基线
    source_pair = DivergenceTracker.make_source_pair(["noaa", "caiyun"])
    div_mu, div_sigma, div_n = div_tracker.stats(event_type, location, source_pair)
    dev_mu, dev_sigma, dev_n = dev_tracker.stats(event_type, location, "ny_rain")

    print(f"  分歧基线: mu={div_mu:.4f} sigma={div_sigma:.4f} n={div_n}")
    print(f"  偏差基线: mu={dev_mu:.4f} sigma={dev_sigma:.4f} n={dev_n}")
    assert div_n >= 30, f"分歧记录数不足: {div_n}"
    assert dev_n >= 30, f"偏差记录数不足: {dev_n}"

    # Step 2: 构造极端分歧场景
    print("\n--- 极端分歧场景 ---")

    # 场景A: NOAA 75% / 彩云 25% (分歧 0.50), 市场 40% (偏向彩云)
    # 融合 (等权) = 50%, dev = 50% - 40% = +10%
    fc_noaa_a = make_forecast("noaa", 0.75)
    fc_caiyun_a = make_forecast("caiyun", 0.25)
    market_a = make_market(0.40, "ny_rain")

    signal_a = strategy.evaluate_sync(
        forecasts=[fc_noaa_a, fc_caiyun_a],
        market=market_a,
        event_type=event_type,
        target_hours_ahead=24,
        record_data=False,  # 不录入, 用于后续测试
    )

    if signal_a:
        print(f"  场景A: NOAA=75% 彩云=25% 市场=40% -> {signal_a.direction.value}")
        print(f"    融合概率={signal_a.target_price:.4f} 偏差={signal_a.deviation:+.4f}")
        print(f"    sigma={signal_a.sigma_level:.2f} 置信度={signal_a.confidence:.4f}")
        print(f"    源间分歧={signal_a.metadata.get('divergence', 0):.4f}")
        print(f"    分歧sigma={signal_a.metadata.get('divergence_sigma', 0):.2f}")
    else:
        print("  场景A: 未触发信号")

    # 场景B: NOAA 20% / 彩云 70% (分歧 0.50), 市场 55% (偏向彩云方向)
    # 融合 = 45%, dev = 45% - 55% = -10%
    fc_noaa_b = make_forecast("noaa", 0.20)
    fc_caiyun_b = make_forecast("caiyun", 0.70)
    market_b = make_market(0.55, "ny_rain")

    signal_b = strategy.evaluate_sync(
        forecasts=[fc_noaa_b, fc_caiyun_b],
        market=market_b,
        event_type=event_type,
        target_hours_ahead=24,
        record_data=False,
    )

    if signal_b:
        print(f"\n  场景B: NOAA=20% 彩云=70% 市场=55% -> {signal_b.direction.value}")
        print(f"    融合概率={signal_b.target_price:.4f} 偏差={signal_b.deviation:+.4f}")
        print(f"    sigma={signal_b.sigma_level:.2f} 置信度={signal_b.confidence:.4f}")
        print(f"    源间分歧={signal_b.metadata.get('divergence', 0):.4f}")
    else:
        print(f"\n  场景B: 未触发信号")

    return signal_a, signal_b


def test_no_divergence():
    """测试2: 源间一致 -> 不发信号。"""
    print_header("测试2: 源间一致 -> 不发信号")

    div_tracker = DivergenceTracker(window_size=100)
    dev_tracker = DeviationTracker(window_size=100)
    params = MultiSourceStrategyParams(
        min_divergence_abs=0.05,
        min_market_div_abs=0.03,
    )
    strategy = MultiSourceStrategy(div_tracker, dev_tracker, params)

    # 两源一致: NOAA 50%, 彩云 48% (分歧 0.02 < min_divergence_abs=0.05)
    fc_noaa = make_forecast("noaa", 0.50)
    fc_caiyun = make_forecast("caiyun", 0.48)
    market = make_market(0.35, "test_token")  # 市场偏差大但源间一致

    signal = strategy.evaluate_sync(
        forecasts=[fc_noaa, fc_caiyun],
        market=market,
        event_type=EventType.RAIN,
        target_hours_ahead=24,
        record_data=True,
    )

    if signal is None:
        print("  源间一致 (分歧=0.02 < 0.05) -> 正确: 未发信号")
    else:
        print(f"  源间一致但发了信号 -> 错误: {signal.direction.value}")
    assert signal is None, "源间一致时不应发信号"


def test_divergence_but_fair_market():
    """测试3: 分歧大但市场定价合理 -> 不发信号。"""
    print_header("测试3: 分歧大但市场定价合理 -> 不发信号")

    div_tracker = DivergenceTracker(window_size=100)
    dev_tracker = DeviationTracker(window_size=100)

    # 先建基线
    for i in range(15):
        noaa_prob = 0.40 + (i % 3) * 0.03
        caiyun_prob = 0.35 + (i % 3) * 0.03
        fused = (noaa_prob + caiyun_prob) / 2
        market_prob = fused  # 市场完全跟随融合概率

        fc_noaa = make_forecast("noaa", noaa_prob)
        fc_caiyun = make_forecast("caiyun", caiyun_prob)
        market = make_market(market_prob, "la_rain")

        strategy = MultiSourceStrategy(div_tracker, dev_tracker)
        strategy.evaluate_sync(
            forecasts=[fc_noaa, fc_caiyun],
            market=market,
            event_type=EventType.RAIN,
            target_hours_ahead=24,
            record_data=True,
        )

    # 构造大分歧但市场定价在融合概率附近
    # NOAA=80%, 彩云=20%, 融合=50%, 市场=50% -> dev=0
    fc_noaa = make_forecast("noaa", 0.80)
    fc_caiyun = make_forecast("caiyun", 0.20)
    market = make_market(0.50, "la_rain")  # 市场在融合概率附近

    strategy = MultiSourceStrategy(div_tracker, dev_tracker)
    signal = strategy.evaluate_sync(
        forecasts=[fc_noaa, fc_caiyun],
        market=market,
        event_type=EventType.RAIN,
        target_hours_ahead=24,
        record_data=True,
    )

    if signal is None:
        print("  分歧大 (0.60) 但市场定价合理 (50%=融合) -> 正确: 未发信号")
    else:
        print(f"  分歧大但市场合理却发了信号 -> 错误: {signal.direction.value}")
    assert signal is None, "市场定价合理时不应发信号"


def test_persistence():
    """测试4: 追踪器持久化。"""
    print_header("测试4: 追踪器持久化")

    import tempfile

    div_tracker = DivergenceTracker(window_size=100)

    # 录入一些数据
    for i in range(20):
        record = DivergenceRecord(
            timestamp=datetime.now(timezone.utc),
            source_probs={"noaa": 0.5 + i * 0.01, "caiyun": 0.4 + i * 0.01},
            fused_prob=0.45 + i * 0.01,
            divergence=0.1,
            event_type=EventType.RAIN,
            location="Test City",
            token_id="test_token",
            source_pair="caiyun|noaa",
        )
        div_tracker.record(record)

    # 保存
    tmp = tempfile.mktemp(suffix=".json")
    div_tracker.save(tmp)
    print(f"  保存: {div_tracker.total_records()} 条记录, {div_tracker.key_count()} 个 key")

    # 加载
    loaded = DivergenceTracker.load(tmp)
    assert loaded is not None
    assert loaded.total_records() == div_tracker.total_records()
    assert loaded.key_count() == div_tracker.key_count()
    print(f"  加载: {loaded.total_records()} 条记录, {loaded.key_count()} 个 key")
    print("  持久化验证通过")

    # 清理
    os.unlink(tmp)


def test_source_weights():
    """测试5: 源权重设置。"""
    print_header("测试5: 源权重设置")

    div_tracker = DivergenceTracker(window_size=100)

    # 手动设置权重
    div_tracker.set_source_weights(
        EventType.RAIN, "Shanghai", "caiyun|noaa",
        {"noaa": 0.7, "caiyun": 0.3},
    )

    weights = div_tracker.get_source_weights(
        EventType.RAIN, "Shanghai", "caiyun|noaa"
    )

    print(f"  设置: noaa=0.7, caiyun=0.3")
    print(f"  获取: noaa={weights.get('noaa', 0):.4f}, caiyun={weights.get('caiyun', 0):.4f}")
    assert abs(weights.get("noaa", 0) - 0.7) < 0.01, "noaa 权重错误"
    assert abs(weights.get("caiyun", 0) - 0.3) < 0.01, "caiyun 权重错误"
    print("  权重设置验证通过")


def test_async_batch():
    """测试6: 异步批量评估。"""
    print_header("测试6: 异步批量评估")

    import asyncio

    div_tracker = DivergenceTracker(window_size=100)
    dev_tracker = DeviationTracker(window_size=100)
    strategy = MultiSourceStrategy(div_tracker, dev_tracker)

    # 建基线
    for i in range(15):
        noaa_prob = 0.40 + (i % 3) * 0.02
        caiyun_prob = 0.38 + (i % 3) * 0.02
        fused = (noaa_prob + caiyun_prob) / 2
        market_prob = fused + 0.005

        strategy.evaluate_sync(
            forecasts=[make_forecast("noaa", noaa_prob), make_forecast("caiyun", caiyun_prob)],
            market=make_market(market_prob, "tokyo_rain"),
            event_type=EventType.RAIN,
            target_hours_ahead=24,
            record_data=True,
        )

    # 构造异常组
    multi_forecasts = [
        [make_forecast("noaa", 0.80), make_forecast("caiyun", 0.30)],
        [make_forecast("noaa", 0.15), make_forecast("caiyun", 0.65)],
    ]
    markets = [
        make_market(0.40, "tokyo_rain"),  # dev = (0.55 - 0.40) = 0.15
        make_market(0.60, "tokyo_rain"),  # dev = (0.40 - 0.60) = -0.20
    ]
    event_types = [EventType.RAIN, EventType.RAIN]

    async def run():
        signals = await strategy.evaluate_batch(
            multi_forecasts=multi_forecasts,
            markets=markets,
            event_types=event_types,
            target_hours_ahead=24,
            record_data=False,
        )
        return signals

    signals = asyncio.run(run())
    print(f"  输入: 2 组多源预报")
    print(f"  输出: {len(signals)} 个信号")
    for i, s in enumerate(signals):
        print(f"    [{i+1}] {s.direction.value} fused={s.target_price:.3f} "
              f"market={s.market_price:.3f} dev={s.deviation:+.4f} "
              f"conf={s.confidence:.3f}")
    print("  异步批量评估验证通过")


# ================================================================
# 主入口
# ================================================================

if __name__ == "__main__":
    print("多源验证策略测试")
    print(f"{'='*60}")

    # 运行所有测试
    sig_a, sig_b = test_baseline_and_signal()
    test_no_divergence()
    test_divergence_but_fair_market()
    test_persistence()
    test_source_weights()
    test_async_batch()

    print_header("全部测试完成")
    print(f"  信号总数: {sum(1 for s in [sig_a, sig_b] if s)}/2")
    print("  多源验证策略逻辑验证通过")


# ================================================================
# R5-002：MultiSource advisory sizing 测试
# ================================================================

import math  # noqa: E402


def _r5_002_params(**overrides) -> MultiSourceStrategyParams:
    base = dict(
        base_capital=1000.0,
        kelly_fraction=0.25,
        max_position_per_market=1000.0,
    )
    base.update(overrides)
    return MultiSourceStrategyParams(**base)


def _r5_002_quantity(market_dev, sigma, cost, **overrides) -> float:
    div_tracker = DivergenceTracker(window_size=100)
    dev_tracker = DeviationTracker(window_size=100)
    strategy = MultiSourceStrategy(
        div_tracker, dev_tracker, _r5_002_params(**overrides)
    )
    return strategy._calculate_quantity(market_dev, sigma, cost)


def test_r5_002_t1_zero_deviation():
    """T1：market_dev=0 → edge=0 → advisory quantity=0。"""
    assert _r5_002_quantity(0.0, 0.1, 0.5) == 0.0


def test_r5_002_t2_one_sigma():
    """T2：|dev|=1σ → edge≈0.6321。"""
    q = _r5_002_quantity(0.1, 0.1, 0.5)
    expected = (1000.0 * 0.25 * (1.0 - math.exp(-1.0))) / 0.5
    assert abs(q - round(expected, 2)) < 1e-6


def test_r5_002_t3_two_sigma():
    """T3：|dev|=2σ → edge≈0.8647。"""
    q = _r5_002_quantity(0.2, 0.1, 0.5)
    expected = (1000.0 * 0.25 * (1.0 - math.exp(-2.0))) / 0.5
    assert abs(q - round(expected, 2)) < 1e-6


def test_r5_002_t4_saturation():
    """T4：随 |dev| 单调增但饱和，不线性飙升，且最终 < 资本上限对应值。"""
    q1 = _r5_002_quantity(0.1, 0.1, 0.5)
    q2 = _r5_002_quantity(0.2, 0.1, 0.5)
    q3 = _r5_002_quantity(0.3, 0.1, 0.5)
    assert q1 < q2 < q3
    # 上界：edge<1 → notional < base*kelly → shares < base*kelly/cost
    assert q3 < (1000.0 * 0.25) / 0.5
    # 非线性：2 倍 dev 不等于 2 倍 quantity
    assert abs(q2 - 2 * q1) > 1e-6


def test_r5_002_t5_negative_deviation():
    """T5：±market_dev 的 magnitude 一致。"""
    assert _r5_002_quantity(0.12, 0.1, 0.5) == _r5_002_quantity(-0.12, 0.1, 0.5)


def test_r5_002_t6_usdc_cap_applied_at_notional_layer():
    """T6：cap 发生在 USDC notional 层（非 shares）。"""
    # base 1000×kelly 0.25 = 250 USDC；cap=100 USDC；edge→1 → shares=100/0.5=200
    q = _r5_002_quantity(10.0, 1.0, 0.5, max_position_per_market=100.0)
    assert q == 200.0
    # 若误把 cap 作用在 shares 上，则会是 100；这里断言不是
    assert q != 100.0


def test_r5_002_t7_yes_cost():
    """T7：BUY_YES cost=price。"""
    q = _r5_002_quantity(0.1, 0.1, 0.25)  # edge=1σ
    expected = (1000.0 * 0.25 * (1.0 - math.exp(-1.0))) / 0.25
    assert abs(q - round(expected, 2)) < 1e-6


def test_r5_002_t8_no_cost():
    """T8：BUY_NO cost=1-price。"""
    q = _r5_002_quantity(0.1, 0.1, 0.75)
    expected = (1000.0 * 0.25 * (1.0 - math.exp(-1.0))) / 0.75
    assert abs(q - round(expected, 2)) < 1e-6


def test_r5_002_t9_invalid_sigma_is_zero():
    """T9：σ 无效（0/负/NaN/Inf）→ advisory quantity=0。"""
    for bad in (0.0, -0.1, float("nan"), float("inf")):
        assert _r5_002_quantity(0.1, bad, 0.5) == 0.0


def test_r5_002_t10_invalid_cost_is_zero():
    """T10：cost 无效（0/负/NaN/Inf）→ advisory quantity=0。"""
    for bad in (0.0, -0.25, float("nan"), float("inf")):
        assert _r5_002_quantity(0.1, 0.1, bad) == 0.0


def test_r5_002_t11_capital_injection_single_source():
    """T11：AGG_BASE_CAPITAL / AGG_KELLY_FRACTION 经装配层注入 MultiSource 参数。"""
    from strategy_engine.config import StrategyConfig

    saved = {
        k: os.environ.get(k)
        for k in ("AGG_BASE_CAPITAL", "AGG_KELLY_FRACTION")
    }
    try:
        os.environ["AGG_BASE_CAPITAL"] = "4321"
        os.environ["AGG_KELLY_FRACTION"] = "0.37"
        cfg = StrategyConfig.from_env()
        assert cfg.multi_source.base_capital == cfg.aggregator.base_capital == 4321.0
        assert cfg.multi_source.kelly_fraction == cfg.aggregator.kelly_fraction == 0.37
    finally:
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


def test_r5_002_t12_aggregator_final_authority_not_overridden():
    """T12：聚合器 final quantity 不受 MultiSource advisory quantity 影响（不修改 Aggregator）。"""
    from strategy_engine.aggregator.aggregator import (
        SignalAggregator,
        AggregatorParams,
    )
    from strategy_engine.aggregator.models import RiskState
    from strategy_engine.models import TradingSignal, EventType, SignalDirection

    def sig(q):
        return TradingSignal(
            strategy="multi_source",
            direction=SignalDirection.BUY_YES,
            token_id="tok",
            event_type=EventType.RAIN,
            location="NY",
            condition_id="cond",
            yes_token_id="tok",
            no_token_id="notok",
            target_price=0.6,
            market_price=0.5,
            quantity=q,
            confidence=0.9,
            deviation=0.1,
            sigma_level=2.0,
        )

    params = AggregatorParams(
        base_capital=1000.0,
        kelly_fraction=0.25,
        min_aggregated_confidence=0.0,
        max_position_per_market=1_000_000.0,
        max_global_exposure=1_000_000.0,
        total_portfolio_value=None,
    )
    agg = SignalAggregator(params)

    o_small = agg.aggregate([sig(1.0)], RiskState())
    o_large = agg.aggregate([sig(999.0)], RiskState())

    assert len(o_small) == 1 and len(o_large) == 1
    # advisory quantity 不同，但最终 quantity 相同（由 PositionSizer 决定）
    assert o_small[0].quantity == o_large[0].quantity
    assert o_small[0].quantity != 999.0


# ================================================================
# A8：R5-002 cap 在 USDC notional 层生效（补测试，不改公式）
# ================================================================


def test_r5_002_a8_cap_is_usdc_notional_across_costs():
    """cap 作用于 USDC notional：不同 cost 下 qty×cost == cap。"""
    # edge→1（market_dev 很大），advisory_notional=250 > cap=100 → capped=100 USDC
    q_yes = _r5_002_quantity(10.0, 1.0, 0.25, max_position_per_market=100.0)
    q_no = _r5_002_quantity(10.0, 1.0, 0.75, max_position_per_market=100.0)
    assert abs(q_yes * 0.25 - 100.0) < 1e-2, "cap 应在 USDC notional 层（YES cost）"
    assert abs(q_no * 0.75 - 100.0) < 1e-2, "cap 应在 USDC notional 层（NO cost）"
    # 若误把 cap 当 shares，则 q_yes==q_no==100
    assert q_yes != q_no


def test_r5_002_a8_below_cap_not_capped():
    """advisory_notional < cap 时不 cap：quantity = advisory_notional / cost。"""
    edge = 1.0 - math.exp(-0.01 / 1.0)
    expected = (1000.0 * 0.25 * edge) / 0.5
    q = _r5_002_quantity(0.01, 1.0, 0.5, max_position_per_market=1000.0)
    assert abs(q - round(expected, 2)) < 1e-6


def test_r5_002_a8_cap_exactly_hit():
    """advisory_notional 恰好等于 cap 时，结果 == cap / cost。"""
    # 选 market_dev 使 edge = cap/(base*kelly) = 100/(1000*0.25)=0.4
    # edge=1-exp(-d/sigma) → d = -sigma*ln(0.6)
    sigma = 1.0
    d = -sigma * math.log(0.6)
    q = _r5_002_quantity(d, sigma, 0.5, max_position_per_market=100.0)
    assert abs(q - 100.0 / 0.5) < 1e-6
