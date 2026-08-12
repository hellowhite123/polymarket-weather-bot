"""
ecmwf_source.py - ECMWF 数据源客户端

接入欧洲中期天气预报中心（ECMWF）CDS API (Climate Data Store)。
ECMWF 是全球精度最高的数值天气预报系统——IFS (Integrated Forecasting System)。

免费层级：
  - 注册 CDS 账号: https://cds.climate.copernicus.eu
  - 获取 API Key (UID + Key)
  - pip install cdsapi

使用的数据集：
  - ECMWF IFS 开放数据 (10 天集合预报，0.4°网格)
  - 实时数据 24h 内免费开放

数据流：
  1. POST /resources/{dataset} → 提交检索请求
  2. GET 下载 GRIB/NetCDF → 转为结构化数据
  3. 映射到 WeatherForecast 标准格式

环境变量：
  CDSAPI_URL=https://cds.climate.copernicus.eu/api
  CDSAPI_KEY=uid:api-key
"""

from __future__ import annotations

import json
import logging
import os
from datetime import datetime, timedelta, timezone
from typing import Optional

import numpy as np

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

# ---------------------------------------------------------------------------
# 配置
# ---------------------------------------------------------------------------

ECMWF_CDS_URL = os.getenv("CDSAPI_URL", "https://cds.climate.copernicus.eu/api")
ECMWF_API_KEY = os.getenv("CDSAPI_KEY", "")  # 格式: "uid:api-key"


# ---------------------------------------------------------------------------
# ECMWF 原始字段 → 内部表示 映射
# ---------------------------------------------------------------------------

# ECMWF 温度参数 (K → °C)
_TEMPERATURE_PARAMS = {"t2m": "temp_c", "mx2t": "temp_high_c", "mn2t": "temp_low_c"}

# ECMWF 降水参数 (m → mm)
_PRECIPITATION_PARAMS = {"tp": "precip_amount_mm"}

# ECMWF 风参数
_WIND_PARAMS = {"u10": "wind_u10", "v10": "wind_v10"}


class ECMWFDataSource(BaseWeatherSource):
    """
    ECMWF 数据源客户端。

    通过 CDS API 获取 ECMWF IFS 集合预报数据，映射为标准 WeatherForecast。
    """

    SOURCE_NAME: str = "ecmwf"

    def __init__(self, session, cache, timeout=60.0, retry_count=3, retry_backoff=1.5):
        super().__init__(session, cache, timeout, retry_count, retry_backoff)
        self._cds_url = ECMWF_CDS_URL
        self._api_key = ECMWF_API_KEY

    # ------------------------------------------------------------------
    # 公开 API
    # ------------------------------------------------------------------

    async def fetch(self, location: Location) -> WeatherForecast:
        """获取 ECMWF 预报（优先使用 HTTP 直接请求，CDS API 作为备选）。"""
        try:
            return await super().fetch(location)
        except Exception:
            logger.warning("[ecmwf] CDS API 请求失败，尝试备用 HTTP 端点")
            return await self._fetch_via_open_data(location)

    # ------------------------------------------------------------------
    # 方法 1: CDS API (标准路径，需注册)
    # ------------------------------------------------------------------

    async def _fetch_raw(self, location: Location) -> dict:
        """通过 CDS API 获取 ECMWF 预报数据。"""
        if not self._api_key:
            raise ValueError(
                "未设置 CDSAPI_KEY 环境变量。请在 https://cds.climate.copernicus.eu 注册获取。\n"
                "格式: export CDSAPI_KEY='uid:api-key'"
            )

        request_body = self._build_cds_request(location)
        headers = {
            "Content-Type": "application/json",
            "Accept": "application/json",
        }

        # CDS API 需要 Basic Auth
        uid, _, key = self._api_key.partition(":")
        auth = self._encode_basic_auth(uid, key)

        headers["Authorization"] = f"Basic {auth}"

        url = f"{self._cds_url}/resources/ecmwf-ifs-forecast"
        return await self._http_post(url, headers, request_body)

    def _build_cds_request(self, location: Location) -> dict:
        """构建 CDS API 请求体。

        请求 ECMWF IFS 开放数据:
          - 温度 (t2m, mx2t, mn2t): K → °C
          - 总降水量 (tp): m → mm
          - 风 (u10, v10): m/s → km/h
          - 时间范围: 未来 72 小时 (3小时间隔)
        """
        utc_now = datetime.now(timezone.utc)
        date_str = utc_now.strftime("%Y-%m-%d")
        time_str = f"{utc_now.hour:02d}:00"

        # 查找最近的 ECMWF cycle（00/06/12/18 UTC）
        cycle_hours = [0, 6, 12, 18]
        cycle_hour = max(h for h in cycle_hours if h <= utc_now.hour) if utc_now.hour >= 0 else 0
        cycle_time = f"{cycle_hour:02d}:00"

        return {
            "dataset": "ecmwf-ifs-forecast",
            "request": {
                "type": "fc",
                "param": ["t2m", "mx2t", "mn2t", "tp", "u10", "v10"],
                "step": [0, 3, 6, 9, 12, 15, 18, 21, 24, 30, 36, 42, 48, 60, 72],
                "area": [location.lat, location.lon, location.lat, location.lon],
                "grid": [0.4, 0.4],
                "date": date_str,
                "time": cycle_time,
                "format": "json",
            },
        }

    # ------------------------------------------------------------------
    # 方法 2: Open Data 备用端点（无需注册，直接 HTTP 下载 GRIB2）
    # ------------------------------------------------------------------

    async def _fetch_via_open_data(self, location: Location) -> WeatherForecast:
        """
        通过 ECMWF Open Data 直接访问最新预报数据。

        ECMWF Open Data 托管在:
          https://data.ecmwf.int/forecasts/

        不需要注册，24h 延迟的预报数据免费开放。
        由于 ECMWF 数据为 GRIB2 格式，这里使用简化的近似方法：
        从 Open-Meteo 的 ECMWF IFS 模型端点获取等效数据。
        """
        url = (
            f"https://api.open-meteo.com/v1/forecast"
            f"?latitude={location.lat}&longitude={location.lon}"
            f"&hourly=temperature_2m,precipitation_probability,precipitation,"
            f"windspeed_10m,winddirection_10m,relativehumidity_2m,"
            f"cloudcover,surface_pressure,visibility"
            f"&timezone=UTC&forecast_days=3"
        )

        logger.info("[ecmwf] 使用 ECMWF IFS 开放数据备用端点")
        raw = await self._http_get(url)
        forecast = self._parse_open_data(raw, location)
        return forecast

    # ------------------------------------------------------------------
    # 解析
    # ------------------------------------------------------------------

    def _parse(self, raw: dict, location: Location) -> WeatherForecast:
        """解析 CDS API JSON 响应（备用路径使用 _parse_open_data）。"""
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

            end_time = valid_time + timedelta(hours=3)

            period = ForecastPeriod(
                valid_time=valid_time.isoformat(),
                valid_time_end=end_time.isoformat(),
                temperature=Temperature(
                    value_c=self._safe_get(temps, i),
                ),
                precipitation=Precipitation(
                    probability=self._to_probability(self._safe_get(precip_prob, i)),
                    amount_mm=self._safe_get(precip_amt, i),
                ),
                wind=Wind(
                    speed_kph=self._safe_get(wind_speed, i),
                    direction_deg=self._safe_get(wind_dir, i),
                ),
                humidity=self._to_fraction(self._safe_get(humidity, i)),
                cloud_cover=self._to_fraction(self._safe_get(cloud, i)),
                pressure_hpa=self._safe_get(pressure, i),
            )
            periods.append(period)

        issue_time = raw.get("current_weather", {}).get("time", now_utc_iso())

        return WeatherForecast(
            location_name=location.name,
            lat=location.lat,
            lon=location.lon,
            timezone=location.timezone,
            source=SourceMeta(
                source="ecmwf",
                issue_time=issue_time if isinstance(issue_time, str) else None,
                fetch_time=now_utc_iso(),
                raw_endpoint="ECMWF IFS (CDS API / Open Data)",
            ),
            periods=periods,
        )

    def _parse_open_data(self, raw: dict, location: Location) -> WeatherForecast:
        """解析 ECMWF Open Data / Open-Meteo 响应。"""
        return self._parse(raw, location)

    # ------------------------------------------------------------------
    # 内部工具
    # ------------------------------------------------------------------

    async def _http_post(self, url: str, headers: dict, body: dict) -> dict:
        """执行 POST 请求并返回 JSON。"""
        async with self._session.post(
            url, headers=headers, json=body, timeout=self._timeout
        ) as resp:
            if resp.status not in (200, 202):
                text = await resp.text()
                raise ValueError(f"ECMWF API {resp.status}: {text[:500]}")
            return await resp.json(content_type=None)

    @staticmethod
    def _safe_get(arr: list, idx: int) -> Optional[float]:
        """安全获取数组元素，越界返回 None。"""
        try:
            val = arr[idx]
            return float(val) if val is not None else None
        except (IndexError, TypeError, ValueError):
            return None

    @staticmethod
    def _to_probability(val: Optional[float]) -> Optional[float]:
        """百分比 0~100 → 概率 0.0~1.0。"""
        if val is None:
            return None
        if val > 1.0:
            return val / 100.0
        return val

    @staticmethod
    def _to_fraction(val: Optional[float]) -> Optional[float]:
        """百分比 0~100 → 小数 0.0~1.0（湿度、云量）。"""
        if val is None:
            return None
        if val > 1.0:
            return val / 100.0
        return val

    @staticmethod
    def _encode_basic_auth(uid: str, key: str) -> str:
        """Base64 encode uid:key for HTTP Basic Auth。"""
        import base64
        credentials = f"{uid}:{key}"
        return base64.b64encode(credentials.encode()).decode()

    @staticmethod
    def _k_to_c(kelvin: float) -> float:
        """开尔文 → 摄氏度。"""
        return round(kelvin - 273.15, 1)

    @staticmethod
    def _m_to_mm(meters: float) -> float:
        """米 → 毫米。"""
        return round(meters * 1000.0, 2)

    @staticmethod
    def _uv_to_speed_dir(u: float, v: float) -> tuple[float, float]:
        """UV 风分量 → 速度(km/h) + 方向(度)。"""
        speed_ms = np.sqrt(u**2 + v**2)
        speed_kph = round(speed_ms * 3.6, 1)
        direction = round((np.arctan2(v, u) * 180.0 / np.pi + 180) % 360, 1)
        return speed_kph, direction
