"""
observation_source.py - NOAA NWS 实时观测站数据源

接入 NOAA NWS 观测站 API，获取 ASOS/METAR 地面实况数据。
与 NOAADataSource 的区别：这里是已经发生的实况，不是预报。

流程:
  1. GET /points/{lat},{lon}          -> 获取 observationStations URL
  2. GET observationStations URL       -> 获取附近观测站列表，取第一个
  3. GET /stations/{stationId}/observations/latest -> 最新实况观测

观测数据特征:
  - temperature: degC
  - precipitationLastHour: mm
  - windSpeed: km/h
  - visibility: m (需转 km)
  - barometricPressure: Pa (需转 hPa)
  - relativeHumidity: percent (需转 0.0~1.0)

缓存策略:
  - 站点 ID 长期缓存（坐标到站点几乎不变）
  - 观测数据短缓存（默认 5 分钟，临近修正需要较新数据）
"""

from __future__ import annotations

import asyncio
import logging
from typing import Optional

import aiohttp

from ..cache import TTLCache
from ..config import NOAA_BASE_URL, NOAA_USER_AGENT, Location
from ..models import RadarObservation

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# 单位转换（observation_source 专用，与 noaa_source 保持一致）
# ---------------------------------------------------------------------------

def _temp_to_c(value: Optional[float], unit_code: str) -> Optional[float]:
    """温度统一转摄氏度。"""
    if value is None:
        return None
    uc = unit_code.lower()
    if "degf" in uc or "fahrenheit" in uc:
        return round((value - 32) * 5 / 9, 2)
    if uc.endswith(":k") or "kelvin" in uc:
        return round(value - 273.15, 2)
    return round(value, 2)


def _wind_to_kph(value: Optional[float], unit_code: str) -> Optional[float]:
    """风速统一转 km/h。"""
    if value is None:
        return None
    uc = unit_code.lower()
    if "m_s" in uc or "m/s" in uc:
        return round(value * 3.6, 2)
    if "mph" in uc:
        return round(value * 1.60934, 2)
    if "kn" in uc or "knot" in uc:
        return round(value * 1.852, 2)
    return round(value, 2)


def _pressure_to_hpa(value: Optional[float], unit_code: str) -> Optional[float]:
    """气压 Pa -> hPa。"""
    if value is None:
        return None
    uc = unit_code.lower()
    if "pa" in uc:
        return round(value / 100.0, 1)
    return round(value, 1)


def _visibility_to_km(value: Optional[float], unit_code: str) -> Optional[float]:
    """能见度 m -> km。"""
    if value is None:
        return None
    uc = unit_code.lower()
    if "m_" in uc or uc.endswith(":m") or "meter" in uc:
        return round(value / 1000.0, 2)
    return round(value, 2)


class NOAAObservationSource:
    """NOAA NWS 观测站实况数据源。

    不继承 BaseWeatherSource（返回类型不同），但复用相同的 HTTP/缓存/重试模式。
    """

    SOURCE_NAME = "noaa_obs"

    def __init__(
        self,
        session: aiohttp.ClientSession,
        cache: TTLCache,
        timeout: float = 30.0,
        retry_count: int = 3,
        retry_backoff: float = 1.5,
    ):
        self._session = session
        self._cache = cache
        self._timeout = aiohttp.ClientTimeout(total=timeout)
        self._retry_count = retry_count
        self._retry_backoff = retry_backoff
        # 站点 ID 缓存（坐标 → station_id，几乎永久不变）
        self._station_cache: dict[str, str] = {}

    # ------------------------------------------------------------------
    # 公开 API
    # ------------------------------------------------------------------

    async def fetch(self, location: Location) -> RadarObservation:
        """获取指定位置的最新实况观测（带缓存 + 重试）。"""
        cache_key = f"{self.SOURCE_NAME}:{location.lat:.4f},{location.lon:.4f}"
        cached = self._cache.get(cache_key)
        if cached is not None:
            logger.debug("[%s] 缓存命中: %s", self.SOURCE_NAME, location.name)
            return cached

        raw = await self._fetch_with_retry(location)
        obs = self._parse(raw, location)

        self._cache.set(cache_key, obs)
        logger.info(
            "[%s] 拉取成功: %s @ %s, temp=%s°C, precip=%smm",
            self.SOURCE_NAME,
            location.name,
            obs.timestamp,
            obs.temperature_c,
            obs.precip_last_hour_mm,
        )
        return obs

    # ------------------------------------------------------------------
    # 内部：三步请求
    # ------------------------------------------------------------------

    async def _fetch_with_retry(self, location: Location) -> dict:
        """带指数退避重试。"""
        last_exc: Optional[Exception] = None
        for attempt in range(1, self._retry_count + 1):
            try:
                return await self._fetch_raw(location)
            except (aiohttp.ClientError, asyncio.TimeoutError, ValueError) as exc:
                last_exc = exc
                if attempt < self._retry_count:
                    wait = self._retry_backoff ** attempt
                    logger.warning(
                        "[%s] 第 %d/%d 次请求失败 (%s), %.1fs 后重试",
                        self.SOURCE_NAME,
                        attempt,
                        self._retry_count,
                        exc.__class__.__name__,
                        wait,
                    )
                    await asyncio.sleep(wait)
                else:
                    logger.error(
                        "[%s] %s 请求彻底失败",
                        self.SOURCE_NAME,
                        location.name,
                    )
        raise last_exc  # type: ignore[misc]

    async def _fetch_raw(self, location: Location) -> dict:
        """三步获取实况观测数据。"""
        headers = {
            "User-Agent": NOAA_USER_AGENT,
            "Accept": "application/geo+json",
        }

        # Step 1+2: 获取最近观测站 ID（带缓存）
        station_id = await self._get_nearest_station(location, headers)

        # Step 3: 获取最新观测
        url = f"{NOAA_BASE_URL}/stations/{station_id}/observations/latest"
        logger.debug("[%s] 请求最新观测: %s", self.SOURCE_NAME, url)
        data = await self._http_get(url, headers=headers)
        data["_station_id"] = station_id
        return data

    async def _get_nearest_station(
        self, location: Location, headers: dict
    ) -> str:
        """获取坐标对应的最近观测站 ID，带缓存。"""
        cache_key = f"{location.lat:.4f},{location.lon:.4f}"
        if cache_key in self._station_cache:
            return self._station_cache[cache_key]

        # 通过 /points 获取 observationStations URL
        points_url = f"{NOAA_BASE_URL}/points/{location.lat:.4f},{location.lon:.4f}"
        logger.debug("[%s] 请求网格信息: %s", self.SOURCE_NAME, points_url)
        resp = await self._http_get(points_url, headers=headers)
        props = resp.get("properties", {})

        stations_url = props.get("observationStations")
        if not stations_url:
            # 回退：用 grid 信息拼接
            wfo = props.get("gridId")
            grid_x = props.get("gridX")
            grid_y = props.get("gridY")
            if wfo and grid_x and grid_y:
                stations_url = f"{NOAA_BASE_URL}/gridpoints/{wfo}/{grid_x},{grid_y}/stations"
            else:
                raise ValueError(
                    f"无法获取 {location.name} 的观测站 URL: /points 返回缺少 observationStations"
                )

        stations_resp = await self._http_get(stations_url, headers=headers)
        features = stations_resp.get("features", [])
        if not features:
            raise ValueError(f"{location.name} 附近无观测站")

        station_id = features[0].get("properties", {}).get("stationIdentifier", "")
        if not station_id:
            raise ValueError("观测站数据缺少 stationIdentifier")

        self._station_cache[cache_key] = station_id
        logger.debug("[%s] 观测站: %s -> %s", self.SOURCE_NAME, location.name, station_id)
        return station_id

    async def _http_get(self, url: str, headers: dict | None = None) -> dict:
        """执行 GET 请求并返回 JSON。"""
        async with self._session.get(
            url, headers=headers, timeout=self._timeout
        ) as resp:
            if resp.status != 200:
                body = await resp.text()
                raise aiohttp.ClientResponseError(
                    request_info=resp.request_info,
                    history=resp.history,
                    status=resp.status,
                    message=f"HTTP {resp.status}: {body[:200]}",
                )
            return await resp.json(content_type=None)

    # ------------------------------------------------------------------
    # 解析
    # ------------------------------------------------------------------

    @staticmethod
    def _extract_value(field: dict) -> Optional[float]:
        """从 NOAA observation 字段中提取数值。

        NOAA 格式: {"value": 12.3, "unitCode": "wmoUnit:degC", "qualityControl": "V"}
        """
        if not field:
            return None
        val = field.get("value")
        if val is None:
            return None
        # 质量控制：跳过明显异常值
        qc = field.get("qualityControl", "")
        if qc in ("Z", "X"):  # Z=未校准, X=缺失
            return None
        return float(val)

    def _parse(self, raw: dict, location: Location) -> RadarObservation:
        """将 NOAA observations/latest 原始 JSON 解析为 RadarObservation。"""
        props = raw.get("properties", {})
        geometry = raw.get("geometry", {})
        coords = geometry.get("coordinates", [0, 0])

        # 温度
        temp_field = props.get("temperature", {})
        dewpoint_field = props.get("dewpoint", {})
        # 体感温度：优先 heatIndex，回退 windChill
        apparent_field = props.get("heatIndex") or props.get("windChill") or {}

        temp_val = self._extract_value(temp_field)
        dewpoint_val = self._extract_value(dewpoint_field)
        apparent_val = self._extract_value(apparent_field)

        # 降水
        precip_val = self._extract_value(props.get("precipitationLastHour", {}))

        # 风
        wind_speed_val = self._extract_value(props.get("windSpeed", {}))
        wind_dir_val = self._extract_value(props.get("windDirection", {}))
        wind_gust_val = self._extract_value(props.get("windGust", {}))

        # 其他
        visibility_val = self._extract_value(props.get("visibility", {}))
        humidity_val = self._extract_value(props.get("relativeHumidity", {}))
        pressure_val = self._extract_value(props.get("barometricPressure", {}))

        # 推断降水类型
        precip_type = self._infer_precip_type(temp_val, precip_val)

        return RadarObservation(
            station_id=raw.get("_station_id", ""),
            station_name=props.get("textDescription", ""),
            lat=coords[1] if len(coords) >= 2 else location.lat,
            lon=coords[0] if len(coords) >= 2 else location.lon,
            timestamp=props.get("timestamp", ""),
            temperature_c=_temp_to_c(temp_val, temp_field.get("unitCode", "wmoUnit:degC")),
            dewpoint_c=_temp_to_c(dewpoint_val, dewpoint_field.get("unitCode", "wmoUnit:degC")),
            apparent_temp_c=_temp_to_c(apparent_val, apparent_field.get("unitCode", "wmoUnit:degC")),
            precip_last_hour_mm=precip_val,
            precipitation_type=precip_type,
            wind_speed_kph=_wind_to_kph(wind_speed_val, props.get("windSpeed", {}).get("unitCode", "wmoUnit:km_h-1")),
            wind_dir_deg=round(wind_dir_val, 1) if wind_dir_val is not None else None,
            wind_gust_kph=_wind_to_kph(wind_gust_val, props.get("windGust", {}).get("unitCode", "wmoUnit:km_h-1")),
            visibility_km=_visibility_to_km(visibility_val, props.get("visibility", {}).get("unitCode", "wmoUnit:m")),
            humidity=round(humidity_val / 100.0, 4) if humidity_val is not None else None,
            pressure_hpa=_pressure_to_hpa(pressure_val, props.get("barometricPressure", {}).get("unitCode", "wmoUnit:Pa")),
            cloud_cover=None,
            radar_reflectivity_dbz=_precip_to_dbz(precip_val),  # Z-R 关系估算雷达反射率
        )

    @staticmethod
    def _infer_precip_type(
        temp_c: Optional[float], precip_mm: Optional[float]
    ) -> Optional[str]:
        """根据温度和降水量推断降水类型。"""
        if precip_mm is None or precip_mm <= 0:
            return "none"
        if temp_c is None:
            return "rain"
        if temp_c < -2:
            return "snow"
        if temp_c < 2:
            return "sleet"
        return "rain"


def _precip_to_dbz(precip_mmh: Optional[float]) -> Optional[float]:
    """
    将降水强度 (mm/h) 转换为雷达反射率 (dBZ)。

    使用 Marshall-Palmer Z-R 关系:
      Z = 200 * R^1.6
      dBZ = 10 * log10(Z)

    对应关系:
      小雨 (1mm/h)  → ~23 dBZ
      中雨 (5mm/h)  → ~35 dBZ
      大雨 (20mm/h) → ~48 dBZ
      暴雨 (50mm/h) → ~57 dBZ
    """
    if precip_mmh is None or precip_mmh <= 0:
        return 0.0
    import math
    R = precip_mmh
    Z = 200.0 * (R ** 1.6)
    dbz = 10.0 * math.log10(max(Z, 1.0))
    return round(dbz, 1)
