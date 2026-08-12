"""
formatter.py - 标准化格式化输出

将清洗后的 WeatherForecast 转换为多种输出格式:
  1. JSON   - 适合存盘、传输、下游 Python 消费
  2. CSV    - 适合人工检查、导入 Excel
  3. DataFrame (pandas) - 适合策略引擎直接使用

输出字段统一、扁平，可直接对接策略引擎的概率计算。
"""

from __future__ import annotations

import json
import os
from datetime import datetime, timezone
from typing import List

from .models import WeatherForecast

try:
    import pandas as pd
    _HAS_PANDAS = True
except ImportError:
    _HAS_PANDAS = False


def to_json(forecast: WeatherForecast, indent: int = 2) -> str:
    """序列化为 JSON 字符串。"""
    return json.dumps(forecast.to_dict(), indent=indent, ensure_ascii=False, default=str)


def to_json_file(forecast: WeatherForecast, filepath: str) -> str:
    """
    保存为 JSON 文件。

    文件名自动包含 数据源_城市_时间戳，便于按时间归档。
    """
    os.makedirs(os.path.dirname(filepath) or ".", exist_ok=True)
    with open(filepath, "w", encoding="utf-8") as f:
        f.write(to_json(forecast))
    return filepath


def to_dataframe(forecast: WeatherForecast):
    """
    转换为 pandas DataFrame。

    每行一个预报周期，列包括:
        location, source, valid_time, temp_c, precip_prob, precip_amount_mm, ...
    """
    if not _HAS_PANDAS:
        raise ImportError("需要安装 pandas: pip install pandas")

    rows = []
    for p in forecast.periods:
        row = {
            "location": forecast.location_name,
            "source": forecast.source.source,
            "lat": forecast.lat,
            "lon": forecast.lon,
            "valid_time": p.valid_time,
            "valid_time_end": p.valid_time_end,
            "temp_c": p.temperature.value_c,
            "temp_high_c": p.temperature.high_c,
            "temp_low_c": p.temperature.low_c,
            "temp_apparent_c": p.temperature.apparent_c,
            "precip_prob": p.precipitation.probability,
            "precip_amount_mm": p.precipitation.amount_mm,
            "precip_intensity_mmh": p.precipitation.intensity_mmh,
            "precip_type": p.precipitation.type,
            "wind_speed_kph": p.wind.speed_kph,
            "wind_dir_deg": p.wind.direction_deg,
            "humidity": p.humidity,
            "cloud_cover": p.cloud_cover,
            "pressure_hpa": p.pressure_hpa,
            "visibility_km": p.visibility_km,
        }
        rows.append(row)

    df = pd.DataFrame(rows)
    if not df.empty:
        df["valid_time"] = pd.to_datetime(df["valid_time"], utc=True, errors="coerce")
        if "valid_time_end" in df.columns:
            df["valid_time_end"] = pd.to_datetime(df["valid_time_end"], utc=True, errors="coerce")
    return df


def to_csv(forecast: WeatherForecast, filepath: str) -> str:
    """保存为 CSV 文件。"""
    df = to_dataframe(forecast)
    os.makedirs(os.path.dirname(filepath) or ".", exist_ok=True)
    df.to_csv(filepath, index=False, encoding="utf-8-sig")
    return filepath


def generate_filename(forecast: WeatherForecast, ext: str = "json") -> str:
    """
    生成规范化的文件名: {source}_{location}_{timestamp}.{ext}

    例如: noaa_NewYork_20240101T120000Z.json
    """
    source = forecast.source.source or "unknown"
    location = forecast.location_name.replace(" ", "")
    ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    return f"{source}_{location}_{ts}.{ext}"


def summary(forecast: WeatherForecast) -> str:
    """生成人类可读的预报摘要，用于日志/调试。"""
    lines = [
        f"=== 天气预报摘要 ===",
        f"城市: {forecast.location_name} ({forecast.lat:.4f}, {forecast.lon:.4f})",
        f"数据源: {forecast.source.source}",
        f"预报发布: {forecast.source.issue_time}",
        f"本地拉取: {forecast.source.fetch_time}",
        f"预报周期数: {forecast.period_count}",
    ]

    if forecast.minutely_intensity_mmh:
        intensities = forecast.minutely_intensity_mmh
        max_int = max(intensities) if intensities else 0
        rain_minutes = sum(1 for v in intensities if v > 0.1)
        lines.append(f"分钟级降水: 未来 {len(intensities)} 分钟, "
                     f"峰值强度 {max_int:.2f} mm/h, 有雨分钟数 {rain_minutes}")
        if forecast.minutely_description:
            lines.append(f"  描述: {forecast.minutely_description}")

    if forecast.periods:
        temps = [p.temperature.value_c for p in forecast.periods
                 if p.temperature.value_c is not None]
        probs = [p.precipitation.probability for p in forecast.periods
                 if p.precipitation.probability is not None]
        if temps:
            lines.append(f"温度范围: {min(temps):.1f}°C ~ {max(temps):.1f}°C")
        if probs:
            lines.append(f"降水概率范围: {min(probs):.0%} ~ {max(probs):.0%}")

    lines.append("=" * 30)
    return "\n".join(lines)
