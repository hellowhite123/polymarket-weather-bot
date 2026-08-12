"""
noaa_source.py - NOAA NWS API 数据源客户端

接入美国国家气象局（NWS）公共 API: https://api.weather.gov
免费、无需 Token，但需设置 User-Agent。

流程:
  1. GET /points/{lat},{lon}       → 获取该坐标的网格信息 (office, gridX, gridY)
  2. GET /gridpoints/{office}/{x},{y} → 获取原始模型网格预报（小时级时间序列）

原始数据特征:
  - 时间使用 ISO 8601 duration 格式: "2024-01-01T00:00:00+00:00/PT1H"
  - 各参数带有 uom（计量单位）字段，需统一转换为国际标准单位
  - probabilityOfPrecipitation 为 0~100 整数，需转为 0.0~1.0
  - windSpeed 可能是 m/s，需转为 km/h
  - temperature 可能是 degC 或 degF，需转为 degC
"""

from __future__ import annotations

import logging
import re
from datetime import datetime, timedelta, timezone
from typing import Optional

from ..config import NOAA_BASE_URL, NOAA_USER_AGENT, Location
from ..models import (
    ForecastPeriod,
    Precipitation,
    SourceMeta,
    Temperature,
    WeatherForecast,
    Wind,
    now_utc_iso,
)
from .base import BaseWeatherSource

logger = logging.getLogger(__name__)

# ISO 8601 duration 正则: P[n]DT[n]H[n]M[n]S 或 P[n]D 等
_DURATION_RE = re.compile(
    r"^P(?:(?P<days>\d+)D)?(?:T(?:(?P<hours>\d+)H)?(?:(?P<minutes>\d+)M)?(?:(?P<seconds>\d+)S)?)?$"
)


def _parse_noaa_duration(duration_str: str) -> timedelta:
    """
    解析 ISO 8601 持续时间字符串（如 "PT1H", "PT6H", "P1D"）。

    返回 timedelta。
    """
    if duration_str == "PT" or duration_str == "P":
        return timedelta(hours=1)  # 默认 1 小时
    match = _DURATION_RE.match(duration_str)
    if not match:
        logger.warning("无法解析 NOAA duration: %s, 默认按 1 小时", duration_str)
        return timedelta(hours=1)
    parts = {k: int(v) for k, v in match.groupdict(default="0").items()}
    return timedelta(
        days=parts["days"],
        hours=parts["hours"],
        minutes=parts["minutes"],
        seconds=parts["seconds"],
    )


def _parse_noaa_validtime(validtime_str: str) -> tuple[datetime, datetime]:
    """
    解析 NOAA 的 "startTime/PT1H" 格式。

    返回 (start, end) 两个 UTC datetime。
    """
    parts = validtime_str.split("/")
    start_str = parts[0]
    duration_str = parts[1] if len(parts) > 1 else "PT1H"

    # NOAA 返回的时间可能带时区偏移，统一转 UTC
    start_dt = datetime.fromisoformat(start_str)
    if start_dt.tzinfo is None:
        start_dt = start_dt.replace(tzinfo=timezone.utc)
    else:
        start_dt = start_dt.astimezone(timezone.utc)

    duration = _parse_noaa_duration(duration_str)
    end_dt = start_dt + duration
    return start_dt, end_dt


def _to_utc_iso(dt: datetime) -> str:
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


# ---------------------------------------------------------------------------
# 单位转换
# ---------------------------------------------------------------------------

def _temp_to_c(value: Optional[float], uom: str) -> Optional[float]:
    """温度统一转摄氏度。"""
    if value is None:
        return None
    uom_lower = uom.lower()
    if "degf" in uom_lower or "fahrenheit" in uom_lower:
        return round((value - 32) * 5 / 9, 2)
    if "k" in uom_lower and "deg" not in uom_lower:
        return round(value - 273.15, 2)
    # 默认 degC
    return round(value, 2)


def _wind_to_kph(value: Optional[float], uom: str) -> Optional[float]:
    """风速统一转 km/h。"""
    if value is None:
        return None
    uom_lower = uom.lower()
    if "m_s" in uom_lower or "m/s" in uom_lower:
        return round(value * 3.6, 2)
    if "mph" in uom_lower:
        return round(value * 1.60934, 2)
    if "kn" in uom_lower or "knot" in uom_lower:
        return round(value * 1.852, 2)
    # 默认 km/h
    return round(value, 2)


def _prob_to_ratio(value: Optional[float]) -> Optional[float]:
    """降水概率 0~100 → 0.0~1.0。"""
    if value is None:
        return None
    return round(value / 100.0, 4)


def _precip_to_mm(value: Optional[float], uom: str) -> Optional[float]:
    """降水量统一转毫米。NOAA quantitativePrecipitation 通常已是 mm。"""
    if value is None:
        return None
    uom_lower = uom.lower()
    if "in" in uom_lower and "kg" not in uom_lower:
        return round(value * 25.4, 3)
    # 默认 mm (kg/m^2 ≈ mm)
    return round(value, 3)


class NOAADataSource(BaseWeatherSource):
    """NOAA NWS API 数据源。"""

    SOURCE_NAME = "noaa"

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        # 网格信息缓存（office/gridX/gridY 按坐标变化极小，可长期缓存）
        self._grid_cache: dict[str, tuple[str, int, int]] = {}

    async def _fetch_raw(self, location: Location) -> dict:
        """
        两步获取 NOAA 数据:
          1. /points/{lat},{lon} → 网格信息
          2. /gridpoints/{office}/{x},{y} → 原始预报
        """
        headers = {
            "User-Agent": NOAA_USER_AGENT,
            "Accept": "application/geo+json",
            "Feature-Flags": "forecast_temperature_quantile_5,forecast_temperature_quantile_95",  # 可选增强
        }

        # 第一步: 获取网格信息
        grid_info = await self._get_grid_info(location, headers)

        # 第二步: 获取网格预报
        office, grid_x, grid_y = grid_info
        url = f"{NOAA_BASE_URL}/gridpoints/{office}/{grid_x},{grid_y}"
        logger.debug("[NOAA] 请求网格预报: %s", url)
        data = await self._http_get(url, headers=headers)
        data["_grid_url"] = url
        return data

    async def _get_grid_info(
        self, location: Location, headers: dict
    ) -> tuple[str, int, int]:
        """获取坐标对应的 NWS 网格 (office, gridX, gridY)，带缓存。"""
        cache_key = f"{location.lat:.4f},{location.lon:.4f}"
        if cache_key in self._grid_cache:
            return self._grid_cache[cache_key]

        url = f"{NOAA_BASE_URL}/points/{location.lat:.4f},{location.lon:.4f}"
        logger.debug("[NOAA] 请求网格信息: %s", url)
        resp = await self._http_get(url, headers=headers)

        props = resp.get("properties", {})
        office = props.get("gridId")
        grid_x = props.get("gridX")
        grid_y = props.get("gridY")

        if not all([office, grid_x, grid_y]):
            raise ValueError(
                f"NOAA /points 返回缺少网格信息: gridId={office}, gridX={grid_x}, gridY={grid_y}"
            )

        grid_tuple = (office, int(grid_x), int(grid_y))
        self._grid_cache[cache_key] = grid_tuple
        return grid_tuple

    def _parse(self, raw: dict, location: Location) -> WeatherForecast:
        """将 NOAA gridpoints 原始 JSON 解析为标准化 WeatherForecast。"""
        props = raw.get("properties", {})

        # 提取各参数序列
        temp_series = self._extract_series(props.get("temperature", {}))
        dewpoint_series = self._extract_series(props.get("dewpoint", {}))
        precip_prob_series = self._extract_series(
            props.get("probabilityOfPrecipitation", {})
        )
        precip_qpf_series = self._extract_series(
            props.get("quantitativePrecipitation", {})
        )
        wind_speed_series = self._extract_series(props.get("windSpeed", {}))
        wind_dir_series = self._extract_series(props.get("windDirection", {}))
        humidity_series = self._extract_series(props.get("relativeHumidity", {}))
        cloud_series = self._extract_series(
            props.get("skyCover", props.get("cloudCover", {}))
        )
        pressure_series = self._extract_series(props.get("pressure", {}))
        visibility_series = self._extract_series(props.get("visibility", {}))

        # 获取 uom
        temp_uom = props.get("temperature", {}).get("uom", "wmoUnit:degC")
        wind_uom = props.get("windSpeed", {}).get("uom", "wmoUnit:km_h-1")
        precip_uom = props.get(
            "quantitativePrecipitation", {}
        ).get("uom", "wmoUnit:mm")

        # 按时间戳对齐所有参数
        all_times = self._collect_timestamps([
            temp_series, precip_prob_series, precip_qpf_series,
            wind_speed_series, wind_dir_series, humidity_series,
            cloud_series, dewpoint_series, pressure_series, visibility_series,
        ])

        periods: list[ForecastPeriod] = []
        for time_key in all_times:
            start_dt, end_dt = time_key

            temp_val = self._lookup(temp_series, start_dt)
            precip_prob = self._lookup(precip_prob_series, start_dt)
            precip_qpf = self._lookup(precip_qpf_series, start_dt)
            wind_spd = self._lookup(wind_speed_series, start_dt)
            wind_dir = self._lookup(wind_dir_series, start_dt)
            humidity = self._lookup(humidity_series, start_dt)
            cloud = self._lookup(cloud_series, start_dt)
            pressure = self._lookup(pressure_series, start_dt)
            visibility = self._lookup(visibility_series, start_dt)

            period = ForecastPeriod(
                valid_time=_to_utc_iso(start_dt),
                valid_time_end=_to_utc_iso(end_dt),
                temperature=Temperature(value_c=_temp_to_c(temp_val, temp_uom)),
                precipitation=Precipitation(
                    probability=_prob_to_ratio(precip_prob),
                    amount_mm=_precip_to_mm(precip_qpf, precip_uom),
                    type=self._infer_precip_type(temp_val, precip_qpf),
                ),
                wind=Wind(
                    speed_kph=_wind_to_kph(wind_spd, wind_uom),
                    direction_deg=round(wind_dir, 1) if wind_dir is not None else None,
                ),
                humidity=round(humidity / 100.0, 4) if humidity is not None else None,
                cloud_cover=round(cloud / 100.0, 4) if cloud is not None else None,
                pressure_hpa=round(pressure, 1) if pressure is not None else None,
                visibility_km=round(visibility / 1000.0, 2)
                if visibility is not None
                else None,
            )
            periods.append(period)

        # 提取预报发布时间
        valid_times_str = props.get("validTimes", "")
        issue_time = None
        if valid_times_str:
            try:
                issue_time, _ = _parse_noaa_validtime(valid_times_str)
                issue_time = _to_utc_iso(issue_time)
            except Exception:
                pass

        return WeatherForecast(
            location_name=location.name,
            lat=location.lat,
            lon=location.lon,
            timezone=location.timezone,
            source=SourceMeta(
                source=self.SOURCE_NAME,
                issue_time=issue_time,
                fetch_time=now_utc_iso(),
                raw_endpoint=raw.get("_grid_url", ""),
            ),
            periods=periods,
        )

    # ------------------------------------------------------------------
    # NOAA 时间序列解析工具
    # ------------------------------------------------------------------

    @staticmethod
    def _extract_series(param_obj: dict) -> list[tuple[datetime, datetime, Optional[float]]]:
        """
        从 NOAA 参数对象中提取 (start, end, value) 序列。

        NOAA 格式:
            {"uom": "...", "values": [{"validTime": ".../PT1H", "value": 12.5}]}
        """
        values = param_obj.get("values", [])
        result = []
        for entry in values:
            vt = entry.get("validTime", "")
            val = entry.get("value")
            try:
                start, end = _parse_noaa_validtime(vt)
            except Exception:
                continue
            result.append((start, end, val))
        return result

    @staticmethod
    def _collect_timestamps(
        series_list: list[list[tuple[datetime, datetime, Optional[float]]]],
    ) -> list[tuple[datetime, datetime]]:
        """收集所有时间序列中出现的时间戳，去重并排序。"""
        seen: dict[datetime, datetime] = {}
        for series in series_list:
            for start, end, _ in series:
                if start not in seen:
                    seen[start] = end
        return sorted(seen.items(), key=lambda x: x[0])

    @staticmethod
    def _lookup(
        series: list[tuple[datetime, datetime, Optional[float]]],
        target: datetime,
    ) -> Optional[float]:
        """在时间序列中查找目标时间点的值。NOAA 使用阶梯式时间区间。"""
        for start, end, val in series:
            if start <= target < end:
                return val
        return None

    @staticmethod
    def _infer_precip_type(temp_c: Optional[float], precip_mm: Optional[float]) -> Optional[str]:
        """根据温度推断降水类型。"""
        if precip_mm is None or precip_mm <= 0:
            return "none"
        if temp_c is None:
            return "rain"
        if temp_c < -2:
            return "snow"
        if temp_c < 2:
            return "sleet"
        return "rain"
