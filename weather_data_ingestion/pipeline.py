"""
pipeline.py - 数据接入管线编排

将多数据源并发拉取 → 清洗 → 格式化 整合为一条完整管线。

用法:
    import asyncio
    from pipeline import IngestionPipeline
    from config import load_config

    async def main():
        pipeline = IngestionPipeline(load_config())
        results = await pipeline.run()  # List[WeatherForecast]
        for r in results:
            print(pipeline.format_summary(r))

    asyncio.run(main())
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
from datetime import datetime, timezone
from typing import List, Optional

import aiohttp

from .cache import TTLCache
from .cleaning import clean_forecast
from .config import PipelineConfig, Location
from .formatter import (
    generate_filename,
    summary as format_summary,
    to_json_file,
    to_csv,
    to_dataframe,
)
from .models import WeatherForecast
from .sources import BaseWeatherSource, CaiyunDataSource, ECMWFDataSource, JMADataSource, NOAADataSource

logger = logging.getLogger(__name__)


class IngestionPipeline:
    """
    气象数据接入管线。

    负责:
      1. 创建 HTTP 会话与缓存
      2. 按配置初始化各数据源客户端
      3. 并发拉取所有 城市×数据源 的预报
      4. 对每个预报执行清洗
      5. 可选: 落盘 JSON / CSV
    """

    def __init__(self, config: PipelineConfig):
        self.config = config
        self._session: Optional[aiohttp.ClientSession] = None
        self._cache = TTLCache(ttl_seconds=config.cache_ttl)
        self._sources: List[BaseWeatherSource] = []

    # ------------------------------------------------------------------
    # 生命周期
    # ------------------------------------------------------------------

    async def __aenter__(self):
        await self._init()
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        await self.close()

    async def _init(self) -> None:
        """初始化 HTTP 会话与数据源。"""
        if self._session is None or self._session.closed:
            self._session = aiohttp.ClientSession(
                headers={"Accept": "application/json"},
            )
        self._sources = self._build_sources()

    async def close(self) -> None:
        """关闭 HTTP 会话。"""
        if self._session and not self._session.closed:
            await self._session.close()

    def _build_sources(self) -> List[BaseWeatherSource]:
        """根据配置构建数据源客户端列表。"""
        sources: List[BaseWeatherSource] = []
        common_kwargs = dict(
            session=self._session,
            cache=self._cache,
            timeout=self.config.timeout,
            retry_count=self.config.retry_count,
            retry_backoff=self.config.retry_backoff,
        )
        if self.config.enable_noaa:
            sources.append(NOAADataSource(**common_kwargs))
        if self.config.enable_caiyun:
            sources.append(CaiyunDataSource(**common_kwargs))
        if self.config.enable_ecmwf:
            sources.append(ECMWFDataSource(**common_kwargs))
        if self.config.enable_jma:
            sources.append(JMADataSource(**common_kwargs))
        return sources

    # ------------------------------------------------------------------
    # 核心运行
    # ------------------------------------------------------------------

    async def run(
        self,
        locations: Optional[List[Location]] = None,
        save_to_disk: bool = True,
    ) -> List[WeatherForecast]:
        """
        执行完整管线: 并发拉取 → 清洗 → 格式化。

        Args:
            locations: 目标城市列表，默认用 config 中的
            save_to_disk: 是否将结果落盘 JSON

        Returns:
            清洗后的 WeatherForecast 列表（每个 城市×数据源 一条）
        """
        await self._init()
        locations = locations or self.config.locations
        logger.info(
            "管线启动: %d 个城市 × %d 个数据源 = %d 个采集任务",
            len(locations), len(self._sources), len(locations) * len(self._sources),
        )

        # 构建所有 城市×数据源 任务
        tasks = []
        for source in self._sources:
            for loc in locations:
                tasks.append(self._fetch_and_clean(source, loc))

        # 并发执行
        results = await asyncio.gather(*tasks, return_exceptions=True)

        # 过滤成功的，记录失败的
        forecasts: List[WeatherForecast] = []
        for i, result in enumerate(results):
            if isinstance(result, Exception):
                logger.error("采集任务失败: %s", result)
            elif result is not None:
                forecasts.append(result)

        logger.info("管线完成: %d/%d 个任务成功", len(forecasts), len(tasks))

        if save_to_disk and forecasts:
            self._save_all(forecasts)

        return forecasts

    async def _fetch_and_clean(
        self, source: BaseWeatherSource, location: Location
    ) -> Optional[WeatherForecast]:
        """拉取单个 城市×数据源 的预报并清洗。"""
        try:
            raw_forecast = await source.fetch(location)
            cleaned = clean_forecast(raw_forecast)
            return cleaned
        except Exception as e:
            logger.error(
                "[%s] %s 采集+清洗失败: %s",
                source.SOURCE_NAME, location.name, e,
            )
            return None

    # ------------------------------------------------------------------
    # 落盘
    # ------------------------------------------------------------------

    def _save_all(self, forecasts: List[WeatherForecast]) -> None:
        """将所有预报保存为 JSON 文件。"""
        output_dir = self.config.output_dir
        os.makedirs(output_dir, exist_ok=True)
        for fc in forecasts:
            filename = generate_filename(fc, "json")
            filepath = os.path.join(output_dir, filename)
            to_json_file(fc, filepath)
            logger.debug("已保存: %s", filepath)

    # ------------------------------------------------------------------
    # 工具方法
    # ------------------------------------------------------------------

    @staticmethod
    def format_summary(forecast: WeatherForecast) -> str:
        """生成人类可读摘要。"""
        return format_summary(forecast)

    def to_dataframe(self, forecasts: List[WeatherForecast]):
        """将多个预报合并为一个 DataFrame。"""
        frames = []
        for fc in forecasts:
            frames.append(to_dataframe(fc))
        if not frames:
            import pandas as pd
            return pd.DataFrame()
        import pandas as pd
        return pd.concat(frames, ignore_index=True)
