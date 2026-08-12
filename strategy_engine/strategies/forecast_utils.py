"""预报概率提取公共工具。

从 WeatherForecast 对象中提取目标时间窗口内的天气概率,
被预报偏差策略和多源验证策略共同复用。
"""

import logging
from datetime import datetime, timedelta, timezone
from typing import List, Optional

logger = logging.getLogger(__name__)


def extract_probability(
    forecast,
    event_type_str: str,
    target_hours_ahead: int = 24,
) -> Optional[float]:
    """从 WeatherForecast 中提取目标时间窗口的气象概率。

    Args:
        forecast: WeatherForecast 对象
        event_type_str: 事件类型 "rain"/"temperature"/"wind"/"humidity"
        target_hours_ahead: 目标时间窗口（小时），从当前时间起算

    Returns:
        概率值 (0.0~1.0)，或 None（无数据）
    """
    now = datetime.now(timezone.utc)
    window_end = now + timedelta(hours=target_hours_ahead)

    # 过滤目标窗口内的预报周期
    target_periods = []
    for period in forecast.periods:
        try:
            valid_time = datetime.fromisoformat(
                period.valid_time.replace("Z", "+00:00")
            )
        except (ValueError, AttributeError):
            continue

        if now <= valid_time <= window_end:
            target_periods.append(period)

    if not target_periods:
        logger.debug(
            "预报 %s 在窗口 [%s, %s] 内无数据",
            getattr(forecast, "location_name", "unknown"),
            now.isoformat(),
            window_end.isoformat(),
        )
        return None

    if event_type_str == "rain":
        probs = [
            p.precipitation.probability
            for p in target_periods
            if p.precipitation is not None
            and p.precipitation.probability is not None
        ]
        if not probs:
            return None
        # 取窗口内最大降水概率（最悲观估计）
        return max(probs)

    elif event_type_str == "temperature":
        temps = [
            p.temperature.value_c
            for p in target_periods
            if p.temperature is not None
            and p.temperature.value_c is not None
        ]
        if not temps:
            return None
        return sum(temps) / len(temps)

    elif event_type_str == "wind":
        speeds = [
            p.wind.speed_kph
            for p in target_periods
            if p.wind is not None
            and p.wind.speed_kph is not None
        ]
        if not speeds:
            return None
        return max(speeds)

    elif event_type_str == "humidity":
        humidities = []
        for p in target_periods:
            if (
                p.temperature is not None
                and hasattr(p.temperature, "humidity")
                and p.temperature.humidity is not None
            ):
                humidities.append(p.temperature.humidity / 100.0)
        if not humidities:
            return None
        return sum(humidities) / len(humidities)

    else:
        # 默认：降水概率
        probs = [
            p.precipitation.probability
            for p in target_periods
            if p.precipitation is not None
            and p.precipitation.probability is not None
        ]
        if not probs:
            return None
        return max(probs)


def get_location(forecast) -> str:
    """从 WeatherForecast 提取位置标识。"""
    if hasattr(forecast, "location_name") and forecast.location_name:
        return forecast.location_name
    if hasattr(forecast, "lat") and hasattr(forecast, "lon"):
        return f"{forecast.lat:.2f},{forecast.lon:.2f}"
    return "unknown"


def get_source_name(forecast) -> str:
    """从 WeatherForecast 提取数据源名称。"""
    if hasattr(forecast, "source") and hasattr(forecast.source, "source"):
        return forecast.source.source
    return "unknown"
