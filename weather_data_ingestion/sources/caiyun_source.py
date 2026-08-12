"""
caiyun_source.py - 彩云天气 API 数据源客户端

接入彩云天气 v2.6 API: https://api.caiyunapp.com
需要 API Token（从 https://dashboard.caiyunapp.com 申请）。

特点:
  - 端点: GET /v2.6/{token}/{lng},{lat}/weather.json
  - 坐标顺序为 经度,纬度（lng,lat），与 NOAA 的 lat,lon 相反
  - 提供 minutely（分钟级）降水预报，独有优势，适合临近修正策略
  - 提供 hourly（48h）和 daily（15d）预报
  - 温度: 摄氏度 | 降水: mm | 湿度/云量: 0~1 | 气压: Pa | 能见度: 米
"""

from __future__ import annotations

import logging
import os
from datetime import datetime, timezone
from typing import Optional

from ..config import CAIYUN_API_TOKEN, CAIYUN_BASE_URL, Location
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


def _caiyun_to_utc_iso(dt_str: str) -> str:
    """
    彩云时间格式 "2024-01-01T00:00+08:00" → UTC ISO 8601 "2024-01-01T00:00:00Z"。
    """
    try:
        dt = datetime.fromisoformat(dt_str)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        dt = dt.astimezone(timezone.utc)
        return dt.strftime("%Y-%m-%dT%H:%M:%SZ")
    except Exception:
        return dt_str


def _safe_float(val) -> Optional[float]:
    """安全转浮点。"""
    if val is None:
        return None
    try:
        return float(val)
    except (TypeError, ValueError):
        return None


class CaiyunDataSource(BaseWeatherSource):
    """彩云天气 API 数据源。"""

    SOURCE_NAME = "caiyun"

    async def _fetch_raw(self, location: Location) -> dict:
        """
        调用彩云天气 API。

        注意: 彩云坐标顺序为 经度在前（lng,lat）。
        """
        if not CAIYUN_API_TOKEN:
            raise ValueError(
                "彩云天气 API Token 未设置！请设置环境变量 CAIYUN_API_TOKEN，"
                "或从 https://dashboard.caiyunapp.com 申请。"
            )

        # 彩云坐标: 经度,纬度
        url = f"{CAIYUN_BASE_URL}/{CAIYUN_API_TOKEN}/{location.lon:.4f},{location.lat:.4f}/weather.json"
        # 请求参数: 可指定需要的预报类型，alert=1 返回预警信息
        url += "?alert=false&minutely=60&hourly=48&daily=15"

        headers = {"Accept": "application/json"}
        logger.debug("[彩云] 请求: %s", url.replace(CAIYUN_API_TOKEN, "***"))
        data = await self._http_get(url, headers=headers)
        data["_request_url"] = url
        return data

    def _parse(self, raw: dict, location: Location) -> WeatherForecast:
        """将彩云天气原始 JSON 解析为标准化 WeatherForecast。"""
        # 彩云顶层: { status, api_version, result: {...} }
        if raw.get("status") != "ok":
            raise ValueError(f"彩云 API 返回错误状态: {raw.get('status')}, msg={raw.get('error')}")

        result = raw.get("result", {})

        # 解析各时间维度预报
        periods = self._parse_hourly(result, location)
        daily_periods = self._parse_daily(result, location)

        # 合并 hourly + daily（daily 附加在后面，含日最高/最低温）
        all_periods = periods + daily_periods

        # 分钟级降水强度（彩云独有）
        minutely_intensity = self._parse_minutely(result)

        # 预报发布时间
        server_time = raw.get("server_time")
        issue_time = None
        if server_time:
            try:
                issue_time = datetime.fromtimestamp(
                    int(server_time), tz=timezone.utc
                ).strftime("%Y-%m-%dT%H:%M:%SZ")
            except (ValueError, TypeError):
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
                raw_endpoint=raw.get("_request_url", "").replace(CAIYUN_API_TOKEN, "***"),
            ),
            periods=all_periods,
            minutely_intensity_mmh=minutely_intensity,
            minutely_description=result.get("minutely", {}).get("description"),
        )

    # ------------------------------------------------------------------
    # hourly 预报解析
    # ------------------------------------------------------------------

    def _parse_hourly(self, result: dict, location: Location) -> list[ForecastPeriod]:
        """
        解析彩云 hourly 预报（未来 48 小时）。

        彩云 hourly 各字段为等长数组，每个元素形如 {"value": x, "datetime": "..."}。
        """
        hourly = result.get("hourly", {})
        if not hourly:
            return []

        # 提取各参数序列
        temp_list = hourly.get("temperature", [])
        apparent_list = hourly.get("apparent_temperature", [])
        precip_list = hourly.get("precipitation", [])
        precip_prob_list = hourly.get("precipitation_probability", [])
        wind_list = hourly.get("wind", [])
        humidity_list = hourly.get("humidity", [])
        cloud_list = hourly.get("cloudrate", [])
        pressure_list = hourly.get("pressure", [])
        visibility_list = hourly.get("visibility", [])

        # 以温度序列的时间为基准对齐
        if not temp_list:
            # 如果没有温度，用降水时间
            base = precip_list or wind_list or humidity_list
        else:
            base = temp_list

        periods: list[ForecastPeriod] = []
        for i, item in enumerate(base):
            dt_str = item.get("datetime", "")
            valid_time = _caiyun_to_utc_iso(dt_str)

            temp_val = self._idx_get(temp_list, i, "value")
            apparent_val = self._idx_get(apparent_list, i, "value")
            precip_val = self._idx_get(precip_list, i, "value")
            precip_prob_val = self._idx_get(precip_prob_list, i, "value")
            wind_item = self._idx_get_raw(wind_list, i)
            humidity_val = self._idx_get(humidity_list, i, "value")
            cloud_val = self._idx_get(cloud_list, i, "value")
            pressure_val = self._idx_get(pressure_list, i, "value")
            visibility_val = self._idx_get(visibility_list, i, "value")

            # wind_item 形如 {"speed": 12.3, "direction": 180}
            wind_speed = None
            wind_dir = None
            if isinstance(wind_item, dict):
                wind_speed = _safe_float(wind_item.get("speed"))
                wind_dir = _safe_float(wind_item.get("direction"))

            # 气压: 彩云返回 Pa，转 hPa
            pressure_hpa = None
            if pressure_val is not None:
                pressure_hpa = round(pressure_val / 100.0, 1) if pressure_val > 2000 else round(pressure_val, 1)

            # 能见度: 彩云返回米，转 km
            visibility_km = None
            if visibility_val is not None:
                visibility_km = round(visibility_val / 1000.0, 2) if visibility_val > 1000 else round(visibility_val, 2)

            period = ForecastPeriod(
                valid_time=valid_time,
                temperature=Temperature(
                    value_c=_safe_float(temp_val),
                    apparent_c=_safe_float(apparent_val),
                ),
                precipitation=Precipitation(
                    probability=_safe_float(precip_prob_val),
                    amount_mm=_safe_float(precip_val),
                    type=self._infer_precip_type(temp_val, precip_val),
                ),
                wind=Wind(
                    speed_kph=_safe_float(wind_speed),
                    direction_deg=round(wind_dir, 1) if wind_dir is not None else None,
                ),
                humidity=round(humidity_val, 4) if humidity_val is not None else None,
                cloud_cover=round(cloud_val, 4) if cloud_val is not None else None,
                pressure_hpa=pressure_hpa,
                visibility_km=visibility_km,
            )
            periods.append(period)

        return periods

    # ------------------------------------------------------------------
    # daily 预报解析
    # ------------------------------------------------------------------

    def _parse_daily(self, result: dict, location: Location) -> list[ForecastPeriod]:
        """
        解析彩云 daily 预报（未来 15 天）。

        彩云 daily 中 temperature 形如:
            {"max": [{"value": 30, "date": "2024-01-01T00:00+08:00"}],
             "min": [{"value": 20, "date": "..."}]}
        precipitation 形如:
            {"max": [...], "min": [...], "avg": [...]}
        """
        daily = result.get("daily", {})
        if not daily:
            return []

        temp_obj = daily.get("temperature", {})
        precip_obj = daily.get("precipitation", {})
        wind_obj = daily.get("wind", [])

        temp_max_list = temp_obj.get("max", []) if isinstance(temp_obj, dict) else []
        temp_min_list = temp_obj.get("min", []) if isinstance(temp_obj, dict) else []

        # 日级降水（取 avg 或 max）
        precip_avg_list = []
        if isinstance(precip_obj, dict):
            precip_avg_list = precip_obj.get("avg", precip_obj.get("max", []))

        # 以温度 max 的时间为基准
        base = temp_max_list or temp_min_list or precip_avg_list

        periods: list[ForecastPeriod] = []
        for i, item in enumerate(base):
            date_str = item.get("date", item.get("datetime", ""))
            valid_time = _caiyun_to_utc_iso(date_str)

            temp_high = self._idx_get(temp_max_list, i, "value")
            temp_low = self._idx_get(temp_min_list, i, "value")
            precip_avg = self._idx_get(precip_avg_list, i, "value")

            period = ForecastPeriod(
                valid_time=valid_time,
                temperature=Temperature(
                    high_c=_safe_float(temp_high),
                    low_c=_safe_float(temp_low),
                ),
                precipitation=Precipitation(
                    amount_mm=_safe_float(precip_avg),
                    type=self._infer_precip_type(
                        _safe_float(temp_high), _safe_float(precip_avg)
                    ),
                ),
            )
            periods.append(period)

        return periods

    # ------------------------------------------------------------------
    # minutely 降水解析（彩云独有）
    # ------------------------------------------------------------------

    def _parse_minutely(self, result: dict) -> list[float]:
        """
        解析分钟级降水强度序列（未来 2 小时，共 120 个值）。

        这是彩云天气的核心差异化数据，可用于临近修正策略——
        当雷达回波显示降水系统偏离预报路径时，分钟级强度能提前反映。
        """
        minutely = result.get("minutely", {})
        precip_2h = minutely.get("precipitation_2h", [])
        if not precip_2h:
            # 有些版本字段名不同
            precip_2h = minutely.get("precipitation", [])
        return [round(float(v), 4) for v in precip_2h if v is not None]

    # ------------------------------------------------------------------
    # 工具
    # ------------------------------------------------------------------

    @staticmethod
    def _idx_get(lst: list, idx: int, key: str = "value"):
        """安全索引取值。"""
        if idx < len(lst) and isinstance(lst[idx], dict):
            return lst[idx].get(key)
        return None

    @staticmethod
    def _idx_get_raw(lst: list, idx: int):
        """安全索引取原始元素。"""
        if idx < len(lst):
            return lst[idx]
        return None

    @staticmethod
    def _infer_precip_type(temp_c, precip_mm) -> Optional[str]:
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
