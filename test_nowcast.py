"""临近修正策略单元测试。

测试覆盖:
  1. 实况信号计算 (降水/温度/风)
  2. 时间窗口约束 (超出修正窗口不运行)
  3. 修正逻辑 (正修正/负修正/无修正)
  4. 信号生成 (方向/置信度/仓位)
  5. 批量评估
  6. 无数据场景 (观测缺失)
"""

import sys
import os
from datetime import datetime, timedelta, timezone

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from strategy_engine.config import NowcastParams
from strategy_engine.models import (
    EventType,
    MarketSnapshot,
    SignalDirection,
    TradingSignal,
)
from strategy_engine.strategies.nowcast_correction import NowcastCorrectionStrategy
from weather_data_ingestion.models import (
    ForecastPeriod,
    Precipitation,
    RadarObservation,
    SourceMeta,
    Temperature,
    WeatherForecast,
    Wind,
)


# ============================================================
# 工具函数
# ============================================================


def make_forecast(
    location: str = "NewYork",
    lat: float = 40.71,
    lon: float = -74.01,
    precip_prob: float = 0.3,
    temp_c: float = 20.0,
) -> WeatherForecast:
    """构造一个简单的 WeatherForecast。"""
    now = datetime.now(timezone.utc)
    periods = []
    for h in range(48):
        t = now + timedelta(hours=h)
        periods.append(ForecastPeriod(
            valid_time=t.strftime("%Y-%m-%dT%H:%M:%SZ"),
            valid_time_end=(t + timedelta(hours=1)).strftime("%Y-%m-%dT%H:%M:%SZ"),
            temperature=Temperature(value_c=temp_c),
            precipitation=Precipitation(
                probability=precip_prob,
                amount_mm=precip_prob * 5.0 if precip_prob > 0 else 0.0,
                type="rain" if precip_prob > 0 else "none",
            ),
            wind=Wind(speed_kph=15.0, direction_deg=180.0),
            humidity=0.5,
        ))
    return WeatherForecast(
        location_name=location,
        lat=lat,
        lon=lon,
        source=SourceMeta(source="noaa"),
        periods=periods,
    )


def make_observation(
    station_id: str = "KJFK",
    precip_mm: float = 0.0,
    temp_c: float = 20.0,
    visibility_km: float = 10.0,
    wind_speed_kph: float = 15.0,
) -> RadarObservation:
    """构造一个 RadarObservation。"""
    return RadarObservation(
        station_id=station_id,
        station_name="Test Station",
        lat=40.71,
        lon=-74.01,
        timestamp=datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        temperature_c=temp_c,
        precip_last_hour_mm=precip_mm,
        precipitation_type="rain" if precip_mm > 0 else "none",
        wind_speed_kph=wind_speed_kph,
        wind_dir_deg=180.0,
        visibility_km=visibility_km,
        humidity=0.5,
    )


def make_market(
    token_id: str = "tok_NYC_rain",
    yes_price: float = 0.4,
) -> MarketSnapshot:
    """构造一个 MarketSnapshot。"""
    return MarketSnapshot(
        token_id=token_id,
        yes_price=yes_price,
        no_price=1.0 - yes_price,
        market_volume=10000.0,
        spread=0.02,
    )


def run_test(name: str):
    """测试装饰器。"""
    def decorator(func):
        def wrapper():
            print(f"\n--- {name} ---")
            func()
            print(f"  PASS")
        wrapper.__name__ = func.__name__
        return wrapper
    return decorator


# ============================================================
# 测试 1: 实况信号计算
# ============================================================


@run_test("实况信号 -- 降水")
def test_precip_signal():
    strategy = NowcastCorrectionStrategy()

    # 有降水 -> 高信号
    obs_rain = make_observation(precip_mm=5.0)
    signal = strategy._compute_observation_signal(obs_rain, EventType.RAIN)
    assert signal is not None
    assert 0.5 < signal <= 1.0, f"有降水信号应 > 0.5, 实际 {signal}"
    print(f"  有降水(5mm): signal={signal:.4f}")

    # 无降水 + 能见度好 -> 低信号
    obs_dry = make_observation(precip_mm=0.0, visibility_km=10.0)
    signal = strategy._compute_observation_signal(obs_dry, EventType.RAIN)
    assert signal is not None
    assert signal < 0.1, f"无降水信号应 < 0.1, 实际 {signal}"
    print(f"  无降水(vis=10km): signal={signal:.4f}")

    # 无降水 + 能见度差 -> 略高信号
    obs_fog = make_observation(precip_mm=0.0, visibility_km=2.0)
    signal = strategy._compute_observation_signal(obs_fog, EventType.RAIN)
    assert signal is not None
    assert 0.05 < signal < 0.15, f"低能见度信号应 0.05~0.15, 实际 {signal}"
    print(f"  无降水(vis=2km): signal={signal:.4f}")

    # 无数据 -> None
    obs_nodata = RadarObservation(station_id="test")
    signal = strategy._compute_observation_signal(obs_nodata, EventType.RAIN)
    assert signal is None, "无降水数据应返回 None"
    print(f"  无数据: signal=None")


@run_test("实况信号 -- 温度")
def test_temp_signal():
    strategy = NowcastCorrectionStrategy()

    obs_cold = make_observation(temp_c=-5.0)
    signal = strategy._compute_observation_signal(obs_cold, EventType.TEMPERATURE)
    assert signal is not None
    # P0-07: 概率语义，低温(-5°C) → P(T<0) 接近 1
    assert signal > 0.99, f"低温信号应接近 1, 实际 {signal}"
    print(f"  -5°C: signal={signal}")

    obs_warm = make_observation(temp_c=30.0)
    signal = strategy._compute_observation_signal(obs_warm, EventType.TEMPERATURE)
    assert signal is not None
    # P0-07: 高温(30°C) → P(T<0) 接近 0
    assert signal < 0.01, f"高温信号应接近 0, 实际 {signal}"
    print(f"  30°C: signal={signal}")


@run_test("实况信号 -- 风")
def test_wind_signal():
    strategy = NowcastCorrectionStrategy()

    obs_calm = make_observation(wind_speed_kph=10.0)
    signal = strategy._compute_observation_signal(obs_calm, EventType.WIND)
    assert signal is not None
    assert signal < 0.3, f"低风速信号应 < 0.3, 实际 {signal}"
    print(f"  10km/h: signal={signal:.4f}")

    obs_gale = make_observation(wind_speed_kph=70.0)
    signal = strategy._compute_observation_signal(obs_gale, EventType.WIND)
    assert signal is not None
    # P0-07: 概率语义，大风(70km/h) → P(>30km/h) 接近 1
    assert signal > 0.999, f"大风信号应接近 1, 实际 {signal}"
    print(f"  70km/h: signal={signal}")


# ============================================================
# 测试 2: 时间窗口约束
# ============================================================


@run_test("时间窗口 -- 超出窗口不运行")
def test_outside_window():
    strategy = NowcastCorrectionStrategy(NowcastParams(
        correction_window_hours=6.0,
    ))
    forecast = make_forecast(precip_prob=0.3)
    obs = make_observation(precip_mm=5.0)
    market = make_market(yes_price=0.4)

    # 距截止 8 小时 -> 超出窗口
    deadline = datetime.now(timezone.utc) + timedelta(hours=8.0)
    signal = strategy.evaluate(forecast, obs, market, EventType.RAIN, deadline)
    assert signal is None, "距截止 8h 应不运行"
    print(f"  距截止 8h: 不运行 (PASS)")

    # 距截止 0.5 小时 -> 在窗口内
    deadline = datetime.now(timezone.utc) + timedelta(hours=0.5)
    signal = strategy.evaluate(forecast, obs, market, EventType.RAIN, deadline)
    assert signal is not None, "距截止 0.5h 应运行"
    print(f"  距截止 0.5h: 运行, conf={signal.confidence:.4f}")


@run_test("时间窗口 -- 事件已过期")
def test_expired():
    strategy = NowcastCorrectionStrategy()
    forecast = make_forecast()
    obs = make_observation(precip_mm=5.0)
    market = make_market()

    deadline = datetime.now(timezone.utc) - timedelta(hours=1.0)
    signal = strategy.evaluate(forecast, obs, market, EventType.RAIN, deadline)
    assert signal is None, "已过期事件应不运行"
    print(f"  过期事件: 不运行 (PASS)")


# ============================================================
# 测试 3: 修正逻辑
# ============================================================


@run_test("修正 -- 正修正 (实况更湿)")
def test_positive_correction():
    """预报说不太会下雨(30%)但实况已在下大雨 -> 上修概率 -> BUY_YES。"""
    strategy = NowcastCorrectionStrategy(NowcastParams(
        correction_window_hours=6.0,
        correction_strength=0.5,
        max_correction=0.3,
        min_deviation_abs=0.01,
        min_confidence=0.0,
    ))
    forecast = make_forecast(precip_prob=0.3)
    obs = make_observation(precip_mm=8.0)  # 大雨
    market = make_market(yes_price=0.35)
    deadline = datetime.now(timezone.utc) + timedelta(hours=2.0)

    signal = strategy.evaluate(forecast, obs, market, EventType.RAIN, deadline)

    assert signal is not None, "应生成信号"
    assert signal.direction == SignalDirection.BUY_YES, \
        f"正修正应 BUY_YES, 实际 {signal.direction}"
    assert signal.metadata["correction"] > 0, "修正值应为正"
    print(f"  预报=0.30, 实况=大雨(8mm), 市场=0.35")
    print(f"  修正={signal.metadata['correction']:+.4f}")
    print(f"  修正概率={signal.metadata['corrected_prob']:.4f}")
    print(f"  方向={signal.direction.value}, conf={signal.confidence:.4f}")


@run_test("修正 -- 负修正 (实况更干)")
def test_negative_correction():
    """预报说会下雨(70%)但实况无降水且能见度好 -> 下修概率 -> BUY_NO。"""
    strategy = NowcastCorrectionStrategy(NowcastParams(
        correction_window_hours=6.0,
        correction_strength=0.5,
        max_correction=0.3,
        min_deviation_abs=0.01,
        min_confidence=0.0,
    ))
    forecast = make_forecast(precip_prob=0.7)
    obs = make_observation(precip_mm=0.0, visibility_km=10.0)  # 无降水
    market = make_market(yes_price=0.65)
    deadline = datetime.now(timezone.utc) + timedelta(hours=2.0)

    signal = strategy.evaluate(forecast, obs, market, EventType.RAIN, deadline)

    assert signal is not None, "应生成信号"
    assert signal.direction == SignalDirection.BUY_NO, \
        f"负修正应 BUY_NO, 实际 {signal.direction}"
    assert signal.metadata["correction"] < 0, "修正值应为负"
    print(f"  预报=0.70, 实况=无降水(vis=10km), 市场=0.65")
    print(f"  修正={signal.metadata['correction']:+.4f}")
    print(f"  修正概率={signal.metadata['corrected_prob']:.4f}")
    print(f"  方向={signal.direction.value}, conf={signal.confidence:.4f}")


@run_test("修正 -- 无修正 (实况与预报一致)")
def test_no_correction():
    """预报概率与实况信号一致 -> 修正≈0 -> 偏差小 -> 不发信号。"""
    strategy = NowcastCorrectionStrategy(NowcastParams(
        correction_window_hours=6.0,
        correction_strength=0.5,
        max_correction=0.3,
        min_deviation_abs=0.05,
        min_confidence=0.3,
    ))
    # 预报 50% 降水, 实况有轻微降水 -> 信号约 0.5
    forecast = make_forecast(precip_prob=0.5)
    obs = make_observation(precip_mm=0.1)  # 轻微降水 -> signal ≈ 0.51
    market = make_market(yes_price=0.50)  # 市场也 50%
    deadline = datetime.now(timezone.utc) + timedelta(hours=4.0)

    signal = strategy.evaluate(forecast, obs, market, EventType.RAIN, deadline)

    if signal is None:
        print(f"  预报=0.50, 实况=轻微降水, 市场=0.50 -> 不发信号 (PASS)")
    else:
        # 即使发了信号，修正值应该很小
        assert abs(signal.metadata["correction"]) < 0.05, \
            f"修正值应很小, 实际 {signal.metadata['correction']}"
        print(f"  修正={signal.metadata['correction']:+.4f} (小修正, PASS)")


# ============================================================
# 测试 4: 信号特征
# ============================================================


@run_test("信号 -- 有效期和元数据")
def test_signal_metadata():
    strategy = NowcastCorrectionStrategy(NowcastParams(
        signal_ttl_seconds=1800,
        correction_window_hours=6.0,
        min_confidence=0.0,
        min_deviation_abs=0.01,
    ))
    forecast = make_forecast(precip_prob=0.3)
    obs = make_observation(precip_mm=5.0)
    market = make_market(yes_price=0.35)
    deadline = datetime.now(timezone.utc) + timedelta(hours=3.0)

    signal = strategy.evaluate(forecast, obs, market, EventType.RAIN, deadline)

    assert signal is not None
    assert signal.strategy == "nowcast_correction"
    assert signal.expires_at is not None
    ttl = (signal.expires_at - signal.generated_at).total_seconds()
    assert 1790 <= ttl <= 1810, f"TTL 应约 1800s, 实际 {ttl}"
    assert "forecast_prob" in signal.metadata
    assert "corrected_prob" in signal.metadata
    assert "obs_signal" in signal.metadata
    assert "correction" in signal.metadata
    assert "time_weight" in signal.metadata
    assert "hours_to_deadline" in signal.metadata
    print(f"  策略名={signal.strategy}")
    print(f"  TTL={ttl:.0f}s")
    print(f"  metadata keys={list(signal.metadata.keys())}")


@run_test("信号 -- 置信度随时间衰减")
def test_confidence_decay():
    """距截止越近，时间权重越大，置信度越高。"""
    strategy = NowcastCorrectionStrategy(NowcastParams(
        correction_window_hours=6.0,
        correction_strength=0.5,
        max_correction=0.3,
        min_confidence=0.0,
        min_deviation_abs=0.001,
    ))
    forecast = make_forecast(precip_prob=0.3)
    obs = make_observation(precip_mm=5.0)
    market = make_market(yes_price=0.35)

    # 距截止 5 小时
    deadline_far = datetime.now(timezone.utc) + timedelta(hours=5.0)
    signal_far = strategy.evaluate(forecast, obs, market, EventType.RAIN, deadline_far)

    # 距截止 1 小时
    deadline_near = datetime.now(timezone.utc) + timedelta(hours=1.0)
    signal_near = strategy.evaluate(forecast, obs, market, EventType.RAIN, deadline_near)

    assert signal_far is not None and signal_near is not None
    tw_far = signal_far.metadata["time_weight"]
    tw_near = signal_near.metadata["time_weight"]
    assert tw_near > tw_far, \
        f"近时间权重({tw_near})应 > 远时间权重({tw_far})"
    print(f"  距截止 5h: tw={tw_far:.4f}, conf={signal_far.confidence:.4f}")
    print(f"  距截止 1h: tw={tw_near:.4f}, conf={signal_near.confidence:.4f}")


# ============================================================
# 测试 5: 批量评估
# ============================================================


@run_test("批量评估")
def test_batch():
    strategy = NowcastCorrectionStrategy(NowcastParams(
        correction_window_hours=6.0,
        min_confidence=0.0,
        min_deviation_abs=0.01,
    ))
    deadline = datetime.now(timezone.utc) + timedelta(hours=2.0)

    forecasts = [
        make_forecast("NewYork", precip_prob=0.3),
        make_forecast("LosAngeles", precip_prob=0.7, lat=34.05, lon=-118.24),
        make_forecast("Chicago", precip_prob=0.5, lat=41.88, lon=-87.63),
    ]
    observations = [
        make_observation("KJFK", precip_mm=5.0),
        make_observation("KLAX", precip_mm=0.0, visibility_km=10.0),
        make_observation("KORD", precip_mm=0.1),
    ]
    markets = [
        make_market("tok_NYC_rain", 0.35),
        make_market("tok_LA_rain", 0.65),
        make_market("tok_CHI_rain", 0.50),
    ]
    event_types = [EventType.RAIN] * 3
    deadlines = [deadline] * 3

    signals = strategy.evaluate_batch(
        forecasts, observations, markets, event_types, deadlines
    )

    assert len(signals) >= 2, f"应至少 2 个信号, 实际 {len(signals)}"
    print(f"  输入 3 个市场, 产出 {len(signals)} 个信号")
    for s in signals:
        print(f"    {s.location} {s.direction.value} corr={s.metadata['correction']:+.4f}")


# ============================================================
# 测试 6: 无数据场景
# ============================================================


@run_test("无数据 -- 观测缺失不发信号")
def test_no_observation_data():
    strategy = NowcastCorrectionStrategy()
    forecast = make_forecast(precip_prob=0.3)
    obs = RadarObservation(station_id="test")  # 完全空
    market = make_market(yes_price=0.35)
    deadline = datetime.now(timezone.utc) + timedelta(hours=2.0)

    signal = strategy.evaluate(forecast, obs, market, EventType.RAIN, deadline)
    assert signal is None, "无观测数据应不发信号"
    print(f"  空观测: 不发信号 (PASS)")


# ============================================================
# 主入口
# ============================================================


if __name__ == "__main__":
    print("=" * 60)
    print("  临近修正策略单元测试")
    print("=" * 60)

    tests = [
        test_precip_signal,
        test_temp_signal,
        test_wind_signal,
        test_outside_window,
        test_expired,
        test_positive_correction,
        test_negative_correction,
        test_no_correction,
        test_signal_metadata,
        test_confidence_decay,
        test_batch,
        test_no_observation_data,
    ]

    passed = 0
    failed = 0
    for test in tests:
        try:
            test()
            passed += 1
        except Exception as e:
            print(f"\n--- {test.__name__} ---")
            print(f"  FAIL: {e}")
            import traceback
            traceback.print_exc()
            failed += 1

    print(f"\n{'=' * 60}")
    print(f"  结果: {passed}/{passed + failed} 通过")
    if failed:
        print(f"  失败: {failed}")
    print(f"{'=' * 60}")
