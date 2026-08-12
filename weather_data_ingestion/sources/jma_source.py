"""
jma_source.py - 日本气象厅（JMA）数据源客户端

接入日本气象厅（気象庁）公开预报数据。

免费数据来源：
  1. JMA 网格点预报（GRIB2 格式）— 东京官网下载
  2. Open-Meteo（JMA seamless 模型）— 零配置备用端点

数据流：
  1. 尝试 JMA 公开 HTTP 端点
  2. 失败 → 回退 Open-Meteo (models=jma_seamless)
  3. 映射到 WeatherForecast 标准格式

特点：
  - JMA 在东亚/西太平洋区域的预报精度极高
  - 对台风路径和日本列岛降水的预测全球领先
"""

from __future__ import annotations

import logging
from datetime import datetime, timedelta, timezone
from typing import Optional

from ..config import Location
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


class JMADataSource(BaseWeatherSource):
    """
    JMA 数据源客户端。

    优先尝试 JMA 公开端点，失败自动回退 Open-Meteo。
    """

    SOURCE_NAME: str = "jma"

    def __init__(self, session, cache, timeout=30.0, retry_count=3, retry_backoff=1.5):
        super().__init__(session, cache, timeout, retry_count, retry_backoff)

    # ------------------------------------------------------------------
    # 公开 API
    # ------------------------------------------------------------------

    async def fetch(self, location: Location) -> WeatherForecast:
        """获取 JMA 预报（优先直接端点，失败回退 Open-Meteo）。"""
        try:
            return await super().fetch(location)
        except Exception:
            logger.warning("[jma] 直接端点失败，回退 Open-Meteo")
            return await self._fetch_via_fallback(location)

    # ------------------------------------------------------------------
    # 方法 1: JMA 公开 HTTP 端点
    # ------------------------------------------------------------------

    async def _fetch_raw(self, location: Location) -> dict:
        """
        通过 JMA 公开展示接口获取网格预报。

        JMA 不提供纯 REST API，但对外提供日文网页展示。
        此处通过 JMA 的实际公开数据接口拉取。
        """
        # JMA 网格点预报的非官方 JSON 端点
        # 日本国内坐标可获取精确数据，海外坐标回退到 Open-Meteo
        if 24.0 <= location.lat <= 46.0 and 123.0 <= location.lon <= 148.0:
            url = (
                f"https://www.jma.go.jp/bosai/forecast/data/forecast/"
                f"{self._jma_region_code(location)}.json"
            )
            try:
                return await self._http_get(url)
            except Exception:
                raise  # 让基类重试
        raise ValueError(f"坐标 {location} 超出 JMA 覆盖范围，请使用 ECMWF/NOAA")

    def _jma_region_code(self, location: Location) -> str:
        """根据坐标映射到 JMA 区域代码。"""
        lat, lon = location.lat, location.lon
        # JMA 区域代码映射（简化版，正式版需完整映射表）
        if lat >= 35.0 and lon >= 139.0:
            return "130000"  # 东京
        elif lat >= 34.0 and lon >= 135.0:
            return "270000"  # 大阪
        elif lat >= 42.0:
            return "016000"  # 札幌
        elif lat >= 33.0:
            return "400000"  # 福冈
        return "130000"  # 默认东京

    # ------------------------------------------------------------------
    # 方法 2: Open-Meteo 回退（JMA seamless 模型）
    # ------------------------------------------------------------------

    async def _fetch_via_fallback(self, location: Location) -> WeatherForecast:
        """通过 Open-Meteo 的 JMA seamless 模型获取预报。"""
        url = (
            f"https://api.open-meteo.com/v1/forecast"
            f"?latitude={location.lat}&longitude={location.lon}"
            f"&hourly=temperature_2m,precipitation_probability,precipitation,"
            f"windspeed_10m,winddirection_10m,relativehumidity_2m,"
            f"cloudcover,surface_pressure"
            f"&models=jma_seamless"
            f"&timezone=UTC&forecast_days=3"
        )
        raw = await self._http_get(url)
        return self._parse(raw, location)

    # ------------------------------------------------------------------
    # 解析
    # ------------------------------------------------------------------

    def _parse(self, raw: dict, location: Location) -> WeatherForecast:
        """解析 JMA / Open-Meteo 响应为标准 WeatherForecast。"""
        hourly = raw.get("hourly", {})
        time_list = hourly.get("time", [])
        periods = []

        temps = hourly.get("temperature_2m", [])
        precip_prob = hourly.get("precipitation_probability", [])
        precip_amt = hourly.get("precipitation", [])
        wind_speed = hourly.get("windspeed_10m", [])
        wind_dir = hourly.get("winddirection_10m", [])
        humidity = hourly.get("relativehumidity_2m", [])
        cloud = hourly.get("cloudcover", [])
        pressure = hourly.get("surface_pressure", [])

        for i, t_str in enumerate(time_list):
            valid_time = datetime.fromisoformat(t_str)
            if valid_time.tzinfo is None:
                valid_time = valid_time.replace(tzinfo=timezone.utc)

            period = ForecastPeriod(
                valid_time=valid_time.isoformat(),
                valid_time_end=(valid_time + timedelta(hours=1)).isoformat(),
                temperature=Temperature(
                    value_c=self._safe_get(temps, i),
                ),
                precipitation=Precipitation(
                    probability=self._normalize(self._safe_get(precip_prob, i)),
                    amount_mm=self._safe_get(precip_amt, i),
                ),
                wind=Wind(
                    speed_kph=self._safe_get(wind_speed, i),
                    direction_deg=self._safe_get(wind_dir, i),
                ),
                humidity=self._normalize(self._safe_get(humidity, i)),
                cloud_cover=self._normalize(self._safe_get(cloud, i)),
                pressure_hpa=self._safe_get(pressure, i),
            )
            periods.append(period)

        return WeatherForecast(
            location_name=location.name,
            lat=location.lat,
            lon=location.lon,
            timezone=location.timezone,
            source=SourceMeta(
                source="jma",
                issue_time=now_utc_iso(),
                fetch_time=now_utc_iso(),
                raw_endpoint="JMA (Grid Point / Open-Meteo fallback)",
            ),
            periods=periods,
        )

    # ------------------------------------------------------------------
    # 工具
    # ------------------------------------------------------------------

    @staticmethod
    def _safe_get(arr: list, idx: int) -> Optional[float]:
        try:
            val = arr[idx]
            return float(val) if val is not None else None
        except (IndexError, TypeError, ValueError):
            return None

    @staticmethod
    def _normalize(val: Optional[float]) -> Optional[float]:
        """百分比 0~100 → 小数 0.0~1.0。"""
        if val is None:
            return None
        if val > 1.0:
            return val / 100.0
        return val
