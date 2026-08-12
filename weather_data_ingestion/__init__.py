"""
weather_data_ingestion - 气象数据接入与融合引擎

对接 NOAA NWS API（美国气象局）与彩云天气 API（中国），
实现多源气象数据的异步采集、清洗与标准化格式化输出。

标准化输出 WeatherForecast 可直接供下游策略引擎消费。
"""

from .config import Location, PipelineConfig, load_config
from .models import (
    ForecastPeriod,
    WeatherForecast,
    Precipitation,
    Temperature,
    Wind,
    SourceMeta,
)
from .pipeline import IngestionPipeline

__version__ = "0.1.0"

__all__ = [
    "Location",
    "PipelineConfig",
    "load_config",
    "ForecastPeriod",
    "WeatherForecast",
    "Precipitation",
    "Temperature",
    "Wind",
    "SourceMeta",
    "IngestionPipeline",
    "__version__",
]
