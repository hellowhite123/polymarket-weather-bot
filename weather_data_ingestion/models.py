"""
models.py - 标准化气象数据模型

所有数据源（NOAA、彩云天气）的原始数据经过清洗后，
统一映射为以下数据结构，供下游策略引擎消费。

设计原则：
  - 单位一律使用国际标准（摄氏度、毫米、公里/时）
  - 时间一律使用 UTC ISO 8601 字符串
  - 概率值一律使用 0.0~1.0 浮点数
  - 缺失值统一用 None 表示，由下游决定如何处理
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import List, Optional


# ---------------------------------------------------------------------------
# 子结构
# ---------------------------------------------------------------------------

@dataclass
class Temperature:
    """温度（摄氏度）。"""

    value_c: Optional[float] = None      # 当前/瞬时温度
    high_c: Optional[float] = None       # 日最高温
    low_c: Optional[float] = None        # 日最低温
    apparent_c: Optional[float] = None   # 体感温度

    @classmethod
    def from_dict(cls, d: dict) -> "Temperature":
        return cls(
            value_c=d.get("temp_c"),
            high_c=d.get("temp_high_c"),
            low_c=d.get("temp_low_c"),
            apparent_c=d.get("temp_apparent_c"),
        )


@dataclass
class Precipitation:
    """降水信息——天气预测市场的核心标的。"""

    probability: Optional[float] = None   # 降水概率 0.0~1.0
    amount_mm: Optional[float] = None     # 降水量（毫米）
    intensity_mmh: Optional[float] = None # 降水强度（毫米/小时），彩云 minutely 提供
    type: Optional[str] = None            # 降水类型: rain / snow / sleet / none

    @classmethod
    def from_dict(cls, d: dict) -> "Precipitation":
        return cls(
            probability=d.get("precip_prob"),
            amount_mm=d.get("precip_amount_mm"),
            intensity_mmh=d.get("precip_intensity_mmh"),
            type=d.get("precip_type"),
        )


@dataclass
class Wind:
    """风。"""

    speed_kph: Optional[float] = None    # 风速 km/h
    direction_deg: Optional[float] = None  # 风向（度，0=北）

    @classmethod
    def from_dict(cls, d: dict) -> "Wind":
        return cls(
            speed_kph=d.get("wind_speed_kph"),
            direction_deg=d.get("wind_dir_deg"),
        )


@dataclass
class SourceMeta:
    """数据来源元信息，用于追溯和融合加权。"""

    source: str = ""                # "noaa" | "caiyun"
    issue_time: Optional[str] = None  # 预报发布时间（UTC ISO 8601）
    fetch_time: Optional[str] = None  # 本地拉取时间（UTC ISO 8601）
    raw_endpoint: str = ""           # 原始 API 端点 URL

    @classmethod
    def from_dict(cls, d: dict) -> "SourceMeta":
        return cls(
            source=d.get("source", ""),
            issue_time=d.get("issue_time"),
            fetch_time=d.get("fetch_time"),
            raw_endpoint=d.get("raw_endpoint", ""),
        )


# ---------------------------------------------------------------------------
# 预报周期——一个时间段（小时级或日级）的预报
# ---------------------------------------------------------------------------

@dataclass
class ForecastPeriod:
    """
    单个预报周期。

    对 NOAA: 通常为 1 小时或 6 小时一段。
    对彩云天气: hourly 数组每小时一条，daily 数组每天一条。
    """

    valid_time: str = ""              # 该预报生效时间（UTC ISO 8601）
    valid_time_end: Optional[str] = None  # 生效结束时间（UTC ISO 8601）

    temperature: Temperature = field(default_factory=Temperature)
    precipitation: Precipitation = field(default_factory=Precipitation)
    wind: Wind = field(default_factory=Wind)

    # 辅助字段
    humidity: Optional[float] = None    # 相对湿度 0.0~1.0
    cloud_cover: Optional[float] = None  # 云量 0.0~1.0
    pressure_hpa: Optional[float] = None  # 气压 hPa
    visibility_km: Optional[float] = None  # 能见度 km

    def to_dict(self) -> dict:
        """序列化为扁平字典，方便写入 DataFrame / JSON。"""
        return {
            "valid_time": self.valid_time,
            "valid_time_end": self.valid_time_end,
            "temp_c": self.temperature.value_c,
            "temp_high_c": self.temperature.high_c,
            "temp_low_c": self.temperature.low_c,
            "temp_apparent_c": self.temperature.apparent_c,
            "precip_prob": self.precipitation.probability,
            "precip_amount_mm": self.precipitation.amount_mm,
            "precip_intensity_mmh": self.precipitation.intensity_mmh,
            "precip_type": self.precipitation.type,
            "wind_speed_kph": self.wind.speed_kph,
            "wind_dir_deg": self.wind.direction_deg,
            "humidity": self.humidity,
            "cloud_cover": self.cloud_cover,
            "pressure_hpa": self.pressure_hpa,
            "visibility_km": self.visibility_km,
        }

    @classmethod
    def from_dict(cls, d: dict) -> "ForecastPeriod":
        return cls(
            valid_time=d.get("valid_time", ""),
            valid_time_end=d.get("valid_time_end"),
            temperature=Temperature.from_dict(d),
            precipitation=Precipitation.from_dict(d),
            wind=Wind.from_dict(d),
            humidity=d.get("humidity"),
            cloud_cover=d.get("cloud_cover"),
            pressure_hpa=d.get("pressure_hpa"),
            visibility_km=d.get("visibility_km"),
        )


# ---------------------------------------------------------------------------
# 顶层预报快照——一个城市 × 一个数据源 的完整预报
# ---------------------------------------------------------------------------

@dataclass
class WeatherForecast:
    """
    标准化天气预报快照。

    一次 API 调用的完整结果，包含若干 ForecastPeriod。
    这是数据接入层的最终输出，也是策略引擎的输入。
    """

    location_name: str = ""
    lat: float = 0.0
    lon: float = 0.0
    timezone: str = "UTC"

    source: SourceMeta = field(default_factory=SourceMeta)
    periods: List[ForecastPeriod] = field(default_factory=list)

    # 临近预报（彩云 minutely 特有，分钟级降水强度）
    minutely_intensity_mmh: List[float] = field(default_factory=list)
    minutely_description: Optional[str] = None

    def to_dict(self) -> dict:
        return {
            "location_name": self.location_name,
            "lat": self.lat,
            "lon": self.lon,
            "timezone": self.timezone,
            "source": {
                "source": self.source.source,
                "issue_time": self.source.issue_time,
                "fetch_time": self.source.fetch_time,
            },
            "periods": [p.to_dict() for p in self.periods],
            "minutely_intensity_mmh": self.minutely_intensity_mmh,
            "minutely_description": self.minutely_description,
        }

    @property
    def period_count(self) -> int:
        return len(self.periods)

    @classmethod
    def from_dict(cls, d: dict) -> "WeatherForecast":
        return cls(
            location_name=d.get("location_name", ""),
            lat=d.get("lat", 0.0),
            lon=d.get("lon", 0.0),
            timezone=d.get("timezone", "UTC"),
            source=SourceMeta.from_dict(d.get("source", {})),
            periods=[ForecastPeriod.from_dict(p) for p in d.get("periods", [])],
            minutely_intensity_mmh=d.get("minutely_intensity_mmh", []),
            minutely_description=d.get("minutely_description"),
        )


# ---------------------------------------------------------------------------
# 实况观测数据（用于临近修正策略）
# ---------------------------------------------------------------------------

@dataclass
class RadarObservation:
    """
    实时地面观测数据（ASOS/METAR 站点）。

    与 WeatherForecast 不同，这是已经发生的实况，
    不是预测。临近修正策略用实况修正预报概率。
    """

    station_id: str = ""
    station_name: str = ""
    lat: float = 0.0
    lon: float = 0.0
    timestamp: str = ""  # ISO 8601 UTC

    # 温度
    temperature_c: Optional[float] = None
    dewpoint_c: Optional[float] = None
    apparent_temp_c: Optional[float] = None

    # 降水
    precip_last_hour_mm: Optional[float] = None
    precipitation_type: Optional[str] = None  # rain / snow / sleet / none

    # 风
    wind_speed_kph: Optional[float] = None
    wind_dir_deg: Optional[float] = None
    wind_gust_kph: Optional[float] = None

    # 其他
    visibility_km: Optional[float] = None
    humidity: Optional[float] = None      # 0.0~1.0
    pressure_hpa: Optional[float] = None
    cloud_cover: Optional[float] = None   # 0.0~1.0

    # 雷达回波（如果有）
    radar_reflectivity_dbz: Optional[float] = None

    def to_dict(self) -> dict:
        return {
            "station_id": self.station_id,
            "station_name": self.station_name,
            "lat": self.lat,
            "lon": self.lon,
            "timestamp": self.timestamp,
            "temperature_c": self.temperature_c,
            "dewpoint_c": self.dewpoint_c,
            "apparent_temp_c": self.apparent_temp_c,
            "precip_last_hour_mm": self.precip_last_hour_mm,
            "precipitation_type": self.precipitation_type,
            "wind_speed_kph": self.wind_speed_kph,
            "wind_dir_deg": self.wind_dir_deg,
            "wind_gust_kph": self.wind_gust_kph,
            "visibility_km": self.visibility_km,
            "humidity": self.humidity,
            "pressure_hpa": self.pressure_hpa,
            "cloud_cover": self.cloud_cover,
            "radar_reflectivity_dbz": self.radar_reflectivity_dbz,
        }

    @classmethod
    def from_dict(cls, d: dict) -> "RadarObservation":
        return cls(
            station_id=d.get("station_id", ""),
            station_name=d.get("station_name", ""),
            lat=d.get("lat", 0.0),
            lon=d.get("lon", 0.0),
            timestamp=d.get("timestamp", ""),
            temperature_c=d.get("temperature_c"),
            dewpoint_c=d.get("dewpoint_c"),
            apparent_temp_c=d.get("apparent_temp_c"),
            precip_last_hour_mm=d.get("precip_last_hour_mm"),
            precipitation_type=d.get("precipitation_type"),
            wind_speed_kph=d.get("wind_speed_kph"),
            wind_dir_deg=d.get("wind_dir_deg"),
            wind_gust_kph=d.get("wind_gust_kph"),
            visibility_km=d.get("visibility_km"),
            humidity=d.get("humidity"),
            pressure_hpa=d.get("pressure_hpa"),
            cloud_cover=d.get("cloud_cover"),
            radar_reflectivity_dbz=d.get("radar_reflectivity_dbz"),
        )


# ---------------------------------------------------------------------------
# 工具函数
# ---------------------------------------------------------------------------

def now_utc_iso() -> str:
    """返回当前 UTC 时间的 ISO 8601 字符串。"""
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
