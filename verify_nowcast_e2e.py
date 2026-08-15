"""临近修正策略端到端验证。

使用真实 NOAA 预报数据 + 手动构造的 RadarObservation，
验证完整管线在各种场景下的行为。

场景:
  1. 预报低概率 + 实况大雨 -> 正修正 -> BUY_YES
  2. 预报高概率 + 实况干燥 -> 负修正 -> BUY_NO
  3. 距截止 8h -> 超出窗口不运行
  4. 距截止 0.3h -> 临近衰减
"""

import os
import sys
import json
from datetime import datetime, timedelta, timezone

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from strategy_engine.config import StrategyConfig, NowcastParams
from strategy_engine.models import EventType, MarketSnapshot
from strategy_engine.strategies.nowcast_correction import NowcastCorrectionStrategy
from weather_data_ingestion.models import WeatherForecast, RadarObservation

PYTHON = r"C:\Users\32389\.workbuddy\binaries\python\envs\default\Scripts\python.exe"


def load_forecast(filepath: str) -> WeatherForecast:
    with open(filepath, "r", encoding="utf-8") as f:
        data = json.load(f)
    return WeatherForecast.from_dict(data)


def main():
    # 加载真实 NOAA 预报
    forecast = load_forecast("data/raw/noaa_NewYork_20260729T131101Z.json")
    print(f"预报加载: {forecast.location_name}, {forecast.period_count} 个周期")
    print(f"数据源: {forecast.source.source}")

    # 提取预报降水概率（用于参考）
    from strategy_engine.strategies.forecast_utils import extract_probability
    fc_prob = extract_probability(forecast, "rain", 24)
    print(f"24h 降水预报概率: {fc_prob:.4f}" if fc_prob else "无法提取降水概率")

    # 构造策略（放宽阈值确保能发信号）
    params = NowcastParams(
        correction_window_hours=6.0,
        correction_strength=0.5,
        max_correction=0.3,
        min_deviation_abs=0.01,
        min_confidence=0.0,
        signal_ttl_seconds=1800,
    )
    strategy = NowcastCorrectionStrategy(params)

    now = datetime.now(timezone.utc)

    print(f"\n{'='*60}")
    print("  临近修正策略端到端验证")
    print(f"{'='*60}")

    # --- 场景 1: 预报低概率 + 实况大雨 ---
    print(f"\n--- 场景 1: 预报低概率 + 实况大雨 ---")
    obs_rain = RadarObservation(
        station_id="KJFK",
        station_name="John F Kennedy Intl",
        lat=40.64, lon=-73.78,
        timestamp=now.strftime("%Y-%m-%dT%H:%M:%SZ"),
        temperature_c=18.5,
        precip_last_hour_mm=6.2,
        precipitation_type="rain",
        wind_speed_kph=22.0,
        wind_dir_deg=190.0,
        visibility_km=4.5,
        humidity=0.85,
    )
    market_low = MarketSnapshot(
        token_id="tok_NYC_rain_20260806",
        yes_price=0.30,
        no_price=0.70,
        market_volume=15000.0,
        spread=0.03,
    )
    deadline_2h = now + timedelta(hours=2.0)
    sig1 = strategy.evaluate(forecast, obs_rain, market_low, EventType.RAIN, deadline_2h)

    if sig1:
        print(f"  方向: {sig1.direction.value}")
        print(f"  预报概率: {sig1.metadata['forecast_prob']:.4f}")
        print(f"  实况信号: {sig1.metadata['obs_signal']:.4f}")
        print(f"  修正值:   {sig1.metadata['correction']:+.4f}")
        print(f"  修正概率: {sig1.metadata['corrected_prob']:.4f}")
        print(f"  市场价格: {sig1.market_price:.4f}")
        print(f"  偏差:     {sig1.deviation:+.4f}")
        print(f"  置信度:   {sig1.confidence:.4f}")
        print(f"  时间权重: {sig1.metadata['time_weight']:.4f}")
        print(f"  建议数量: {sig1.quantity:.2f}")
        print(f"  距截止:   {sig1.metadata['hours_to_deadline']:.1f}h")
    else:
        print(f"  未发信号")

    # --- 场景 2: 预报高概率 + 实况干燥 ---
    print(f"\n--- 场景 2: 预报高概率 + 实况干燥 ---")
    obs_dry = RadarObservation(
        station_id="KJFK",
        station_name="John F Kennedy Intl",
        lat=40.64, lon=-73.78,
        timestamp=now.strftime("%Y-%m-%dT%H:%M:%SZ"),
        temperature_c=22.0,
        precip_last_hour_mm=0.0,
        precipitation_type="none",
        wind_speed_kph=8.0,
        wind_dir_deg=270.0,
        visibility_km=15.0,
        humidity=0.40,
    )
    market_high = MarketSnapshot(
        token_id="tok_NYC_rain_20260806",
        yes_price=0.75,
        no_price=0.25,
        market_volume=15000.0,
        spread=0.03,
    )
    sig2 = strategy.evaluate(forecast, obs_dry, market_high, EventType.RAIN, deadline_2h)

    if sig2:
        print(f"  方向: {sig2.direction.value}")
        print(f"  预报概率: {sig2.metadata['forecast_prob']:.4f}")
        print(f"  实况信号: {sig2.metadata['obs_signal']:.4f}")
        print(f"  修正值:   {sig2.metadata['correction']:+.4f}")
        print(f"  修正概率: {sig2.metadata['corrected_prob']:.4f}")
        print(f"  市场价格: {sig2.market_price:.4f}")
        print(f"  偏差:     {sig2.deviation:+.4f}")
        print(f"  置信度:   {sig2.confidence:.4f}")
    else:
        print(f"  未发信号")

    # --- 场景 3: 超出修正窗口 ---
    print(f"\n--- 场景 3: 距截止 8h (超出窗口) ---")
    deadline_8h = now + timedelta(hours=8.0)
    sig3 = strategy.evaluate(forecast, obs_rain, market_low, EventType.RAIN, deadline_8h)
    print(f"  结果: {'未发信号 (PASS)' if sig3 is None else '不应发信号!'}")

    # --- 场景 4: 临近衰减 ---
    print(f"\n--- 场景 4: 距截止 0.3h (临近衰减) ---")
    deadline_03h = now + timedelta(hours=0.3)
    sig4 = strategy.evaluate(forecast, obs_rain, market_low, EventType.RAIN, deadline_03h)
    if sig4:
        print(f"  方向: {sig4.direction.value}")
        print(f"  置信度: {sig4.confidence:.4f} (衰减 vs 时间权重增长的平衡)")
        print(f"  时间权重: {sig4.metadata['time_weight']:.4f}")
    else:
        print(f"  未发信号 (置信度过低被过滤)")

    # --- 场景 5: 多策略聚合 ---
    print(f"\n--- 场景 5: 多策略聚合 (预报偏差 + 临近修正) ---")
    from strategy_engine.pipeline import StrategyPipeline
    from strategy_engine.aggregator.models import RiskState

    config = StrategyConfig.from_env()
    config.deviation.deviation_threshold_sigma = 0.3
    config.deviation.min_deviation_abs = 0.01
    config.nowcast.min_deviation_abs = 0.01
    config.nowcast.min_confidence = 0.0
    pipeline = StrategyPipeline(config)

    all_signals = []

    # 临近修正信号
    nc_sig = strategy.evaluate(forecast, obs_rain, market_low, EventType.RAIN, deadline_2h)
    if nc_sig:
        all_signals.append(nc_sig)

    # 聚合
    if all_signals:
        orders = pipeline.aggregate_signals(all_signals, RiskState())
        print(f"  输入 {len(all_signals)} 个信号, 输出 {len(orders)} 个聚合指令")
        for o in orders:
            print(f"    {o.direction.value} {o.token_id[:20]} "
                  f"qty={o.quantity:.2f} conf={o.confidence:.4f} "
                  f"status={o.status.value}")
            print(f"    贡献策略: {', '.join(o.contributing_strategies)}")
    else:
        print(f"  无信号可聚合")

    print(f"\n{'='*60}")
    print(f"  验证完成")
    print(f"{'='*60}")


if __name__ == "__main__":
    main()
