"""
config.py - 配置管理

集中管理 API 端点、Token、超时、缓存、目标城市等配置项。
支持从环境变量读取敏感信息（API Token），也可从 .env 文件加载。
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from typing import Dict, List

try:
    from dotenv import load_dotenv
    load_dotenv()
except ImportError:
    # python-dotenv 非必须，仅在存在时加载 .env
    pass


# ---------------------------------------------------------------------------
# 彩云天气 API
# ---------------------------------------------------------------------------
CAIYUN_BASE_URL = "https://api.caiyunapp.com/v2.6"
CAIYUN_API_TOKEN = os.getenv("CAIYUN_API_TOKEN", "")  # 从环境变量读取

# ---------------------------------------------------------------------------
# NOAA NWS API（免费，无需 Token，但建议设置 User-Agent）
# ---------------------------------------------------------------------------
NOAA_BASE_URL = "https://api.weather.gov"
NOAA_USER_AGENT = os.getenv(
    "NOAA_USER_AGENT",
    "WeatherTradingBot/0.1 (contact@example.com)",  # NOAA 建议填写联系方式
)

# ---------------------------------------------------------------------------
# ECMWF CDS API（需注册获取 API Key: https://cds.climate.copernicus.eu）
# ---------------------------------------------------------------------------
ECMWF_BASE_URL = os.getenv("CDSAPI_URL", "https://cds.climate.copernicus.eu/api")
ECMWF_API_KEY = os.getenv("CDSAPI_KEY", "")  # 格式: "uid:api-key"
ECMWF_OPEN_DATA_URL = "https://api.open-meteo.com/v1/forecast"

# ---------------------------------------------------------------------------
# 网络配置
# ---------------------------------------------------------------------------
DEFAULT_TIMEOUT_SECONDS = 30
DEFAULT_RETRY_COUNT = 3
DEFAULT_RETRY_BACKOFF = 1.5  # 指数退避基数（秒）

# ---------------------------------------------------------------------------
# 缓存配置
# ---------------------------------------------------------------------------
CACHE_TTL_SECONDS = 300  # 默认缓存 5 分钟，避免频繁请求 API


@dataclass
class Location:
    """目标地理位置——一个天气预测市场通常绑定一个城市坐标。"""

    name: str
    lat: float
    lon: float
    timezone: str = "UTC"

    def __str__(self) -> str:
        return f"{self.name}({self.lat:.4f},{self.lon:.4f})"


@dataclass
class PipelineConfig:
    """数据接入管线的运行配置。"""

    # 目标城市列表（一个城市对应一个 Polymarket 天气事件）
    locations: List[Location] = field(default_factory=list)

    # 启用的数据源
    enable_noaa: bool = True
    enable_caiyun: bool = True
    enable_ecmwf: bool = False  # 默认关闭，需注册 CDS 后开启
    enable_jma: bool = False    # 默认关闭，需 JMA API Key 或 Open-Meteo 回退

    # 网络参数
    timeout: float = DEFAULT_TIMEOUT_SECONDS
    retry_count: int = DEFAULT_RETRY_COUNT
    retry_backoff: float = DEFAULT_RETRY_BACKOFF

    # 缓存
    cache_ttl: int = CACHE_TTL_SECONDS

    # 输出
    output_dir: str = "./data/raw"  # 原始数据落盘目录

    def __post_init__(self) -> None:
        if not self.locations:
            # 默认目标城市：纽约（Polymarket 天气市场最活跃城市）+ 北京
            self.locations = [
                Location(name="New York", lat=40.7128, lon=-74.0060, timezone="America/New_York"),
                Location(name="Beijing", lat=39.9042, lon=116.4074, timezone="Asia/Shanghai"),
            ]

    @property
    def active_sources(self) -> List[str]:
        sources = []
        if self.enable_noaa:
            sources.append("noaa")
        if self.enable_caiyun:
            sources.append("caiyun")
        if self.enable_ecmwf:
            sources.append("ecmwf")
        if self.enable_jma:
            sources.append("jma")
        return sources


def load_config(**overrides) -> PipelineConfig:
    """
    加载管线配置，允许运行时覆盖默认值。

    用法:
        cfg = load_config(timeout=60, enable_caiyun=False)
    """
    config = PipelineConfig()
    for key, value in overrides.items():
        if hasattr(config, key):
            setattr(config, key, value)
    return config
