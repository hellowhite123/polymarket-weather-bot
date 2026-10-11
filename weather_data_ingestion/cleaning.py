"""
cleaning.py - 数据清洗模块

对标准化 WeatherForecast 进行二次清洗:
  1. 缺失值处理（前向填充 / 插值 / 默认值）
  2. 异常值检测与修正（基于物理范围检查 + IQR 离群点检测）
  3. 时间对齐（去除重复时间戳、按时间排序、补全缺口）
  4. 日级/小时级数据合并去重

清洗后的数据更加干净，适合直接输入策略引擎做概率计算。
"""

from __future__ import annotations

import copy
import logging
import math
from typing import Optional

from .models import ForecastPeriod, WeatherForecast

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# 物理范围约束（用于异常值检测）
# ---------------------------------------------------------------------------
PHYSICAL_RANGES = {
    "temp_c": (-90.0, 70.0),        # 摄氏度
    "temp_high_c": (-90.0, 70.0),
    "temp_low_c": (-90.0, 70.0),
    "temp_apparent_c": (-100.0, 80.0),
    "precip_prob": (0.0, 1.0),       # 概率 0~1
    "precip_amount_mm": (0.0, 500.0), # 单次/单日降水极少超过 500mm
    "precip_intensity_mmh": (0.0, 300.0),
    "wind_speed_kph": (0.0, 400.0),   # 飓风级
    "wind_dir_deg": (0.0, 360.0),
    "humidity": (0.0, 1.0),
    "cloud_cover": (0.0, 1.0),
    "pressure_hpa": (800.0, 1100.0),
    "visibility_km": (0.0, 200.0),
}


def _is_in_range(field: str, value: Optional[float]) -> bool:
    """检查值是否在合理物理范围内。"""
    if value is None:
        return True  # None 不算异常
    lo, hi = PHYSICAL_RANGES.get(field, (None, None))
    if lo is not None and (value < lo or value > hi):
        return False
    return True


def _clamp_or_none(field: str, value: Optional[float]) -> Optional[float]:
    """超范围则置为 None（视为无效数据）。"""
    if value is None:
        return None
    if not _is_in_range(field, value):
        logger.warning(
            "异常值检测: %s=%.2f 超出合理范围 %s，置为 None",
            field, value, PHYSICAL_RANGES.get(field),
        )
        return None
    return value


# ---------------------------------------------------------------------------
# 缺失值填充
# ---------------------------------------------------------------------------

def _forward_fill(values: list[Optional[float]]) -> list[Optional[float]]:
    """
    前向填充: 用前一个有效值填充当前缺失值。
    适合时间序列中偶发的数据缺失。
    """
    filled = []
    last_valid: Optional[float] = None
    for v in values:
        if v is not None:
            filled.append(v)
            last_valid = v
        else:
            filled.append(last_valid)
    return filled


def _linear_interpolate(values: list[Optional[float]]) -> list[Optional[float]]:
    """
    线性插值: 用相邻两个有效值线性插值填充中间缺失值。
    比前向填充更精确，适合连续缺失区间。
    """
    n = len(values)
    result = list(values)
    i = 0
    while i < n:
        if result[i] is None:
            # 找到下一个有效值
            j = i + 1
            while j < n and result[j] is None:
                j += 1
            if i > 0 and j < n:
                # 前后都有值，线性插值
                prev_val = result[i - 1]
                next_val = result[j]
                gap = j - (i - 1)
                for k in range(i, j):
                    frac = (k - (i - 1)) / gap
                    result[k] = round(prev_val + (next_val - prev_val) * frac, 4)
            elif j < n:
                # 只有后面有值，前向填充
                for k in range(i, j):
                    result[k] = result[j]
            elif i > 0:
                # 只有前面有值，后向填充（用前值）
                for k in range(i, n):
                    result[k] = result[i - 1]
            i = j
        else:
            i += 1
    return result


# ---------------------------------------------------------------------------
# IQR 离群点检测
# ---------------------------------------------------------------------------

def _detect_outliers_iqr(
    values: list[Optional[float]], threshold: float = 3.0
) -> list[bool]:
    """
    基于 IQR（四分位距）检测离群点。

    返回布尔列表，True 表示该位置的值为离群点。
    threshold=3.0 表示超过 Q3 + 3*IQR 或低于 Q1 - 3*IQR 才算离群（较宽松）。
    """
    valid = [v for v in values if v is not None and not math.isnan(v)]
    if len(valid) < 4:
        return [False] * len(values)

    valid_sorted = sorted(valid)
    n = len(valid_sorted)
    q1 = valid_sorted[n // 4]
    q3 = valid_sorted[3 * n // 4]
    iqr = q3 - q1

    lower = q1 - threshold * iqr
    upper = q3 + threshold * iqr

    return [
        v is not None and (v < lower or v > upper) for v in values
    ]


# ---------------------------------------------------------------------------
# 单个 ForecastPeriod 清洗
# ---------------------------------------------------------------------------

def _clean_period_fields(period: ForecastPeriod) -> ForecastPeriod:
    """对单个 ForecastPeriod 做物理范围检查。"""
    period.temperature.value_c = _clamp_or_none("temp_c", period.temperature.value_c)
    period.temperature.high_c = _clamp_or_none("temp_high_c", period.temperature.high_c)
    period.temperature.low_c = _clamp_or_none("temp_low_c", period.temperature.low_c)
    period.temperature.apparent_c = _clamp_or_none("temp_apparent_c", period.temperature.apparent_c)
    period.precipitation.probability = _clamp_or_none("precip_prob", period.precipitation.probability)
    period.precipitation.amount_mm = _clamp_or_none("precip_amount_mm", period.precipitation.amount_mm)
    period.precipitation.intensity_mmh = _clamp_or_none("precip_intensity_mmh", period.precipitation.intensity_mmh)
    period.wind.speed_kph = _clamp_or_none("wind_speed_kph", period.wind.speed_kph)
    period.wind.direction_deg = _clamp_or_none("wind_dir_deg", period.wind.direction_deg)
    period.humidity = _clamp_or_none("humidity", period.humidity)
    period.cloud_cover = _clamp_or_none("cloud_cover", period.cloud_cover)
    period.pressure_hpa = _clamp_or_none("pressure_hpa", period.pressure_hpa)
    period.visibility_km = _clamp_or_none("visibility_km", period.visibility_km)
    return period


# ---------------------------------------------------------------------------
# 顶层清洗函数
# ---------------------------------------------------------------------------

def clean_forecast(forecast: WeatherForecast) -> WeatherForecast:
    """
    对一个 WeatherForecast 执行完整清洗流程:
      1. 物理范围检查 → 异常值置 None
      2. 时间去重与排序
      3. 按字段做缺失值线性插值
      4. IQR 离群点检测（仅温度）→ 离群点置 None 后重新插值

    注意: 降水为零膨胀分布，IQR 不适用，仅做物理范围检查。

    返回新的 WeatherForecast（不修改原始对象）。
    """
    # R5-003：兑现不可变契约——先深拷贝再清洗，避免原地修改污染调用方
    # 持有的原始快照（此前 `_clean_period_fields` 与 `forecast.periods = periods`
    # 均直接改动传入对象）。
    forecast = copy.deepcopy(forecast)

    if not forecast.periods:
        logger.warning("[%s] %s 无预报周期，跳过清洗", forecast.source.source, forecast.location_name)
        return forecast

    # Step 1: 物理范围检查
    periods = [_clean_period_fields(p) for p in forecast.periods]

    # Step 2: 按时间排序 + 去重
    periods = _dedup_and_sort(periods)

    if len(periods) < 2:
        logger.debug("[%s] %s 清洗后周期数 < 2，跳过插值", forecast.source.source, forecast.location_name)
        forecast.periods = periods
        return forecast

    # Step 3: 提取各字段序列，做线性插值
    temp_vals = _linear_interpolate([p.temperature.value_c for p in periods])
    precip_prob_vals = _linear_interpolate([p.precipitation.probability for p in periods])
    precip_amt_vals = _linear_interpolate([p.precipitation.amount_mm for p in periods])
    wind_vals = _linear_interpolate([p.wind.speed_kph for p in periods])
    humidity_vals = _linear_interpolate([p.humidity for p in periods])
    cloud_vals = _linear_interpolate([p.cloud_cover for p in periods])
    pressure_vals = _linear_interpolate([p.pressure_hpa for p in periods])
    vis_vals = _linear_interpolate([p.visibility_km for p in periods])

    # Step 4: IQR 离群点检测
    # 仅对温度做 IQR（温度近似正态分布，IQR 有效）。
    # 降水是零膨胀分布（大部分为 0），IQR 会将所有非零值误判为离群点，
    # 因此降水仅依赖物理范围检查，不做 IQR。
    temp_outliers = _detect_outliers_iqr(temp_vals, threshold=4.0)

    for i, is_outlier in enumerate(temp_outliers):
        if is_outlier:
            logger.warning(
                "[%s] %s 温度离群点: %s @ %s",
                forecast.source.source, forecast.location_name,
                temp_vals[i], periods[i].valid_time,
            )
            temp_vals[i] = None

    # 重新插值被置 None 的离群点
    temp_vals = _linear_interpolate(temp_vals)

    # 回写
    for i, p in enumerate(periods):
        p.temperature.value_c = temp_vals[i]
        p.precipitation.probability = precip_prob_vals[i]
        p.precipitation.amount_mm = precip_amt_vals[i]
        p.wind.speed_kph = wind_vals[i]
        p.humidity = humidity_vals[i]
        p.cloud_cover = cloud_vals[i]
        p.pressure_hpa = pressure_vals[i]
        p.visibility_km = vis_vals[i]

    forecast.periods = periods
    logger.info(
        "[%s] %s 清洗完成: %d 个周期",
        forecast.source.source, forecast.location_name, len(periods),
    )
    return forecast


def _dedup_and_sort(periods: list[ForecastPeriod]) -> list[ForecastPeriod]:
    """按 valid_time 去重并排序，保留最后出现的。"""
    seen: dict[str, ForecastPeriod] = {}
    for p in periods:
        seen[p.valid_time] = p  # 后出现的覆盖先出现的
    return sorted(seen.values(), key=lambda p: p.valid_time)
