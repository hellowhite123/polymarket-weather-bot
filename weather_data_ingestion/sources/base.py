"""
base.py - 数据源抽象基类

定义所有气象数据源客户端必须实现的接口。
统一接口使得下游 pipeline 可以透明地并发拉取多源数据。
"""

from __future__ import annotations

import asyncio
import logging
from abc import ABC, abstractmethod
from typing import Optional

import aiohttp

from ..cache import TTLCache
from ..config import Location
from ..models import WeatherForecast

logger = logging.getLogger(__name__)


class BaseWeatherSource(ABC):
    """
    气象数据源抽象基类。

    子类需实现:
        - _fetch_raw():  调用 API 获取原始 JSON
        - _parse():      将原始 JSON 转换为标准化 WeatherForecast

    本基类提供:
        - HTTP 会话管理（aiohttp.ClientSession）
        - 缓存（TTLCache）
        - 重试（指数退避）
        - 统一异常处理与日志
    """

    SOURCE_NAME: str = "base"

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

    # ------------------------------------------------------------------
    # 公开 API
    # ------------------------------------------------------------------

    async def fetch(self, location: Location) -> WeatherForecast:
        """
        获取指定城市的天气预报（带缓存 + 重试）。

        返回标准化 WeatherForecast；如果所有重试均失败则抛出异常。
        """
        cache_key = f"{self.SOURCE_NAME}:{location.lat:.4f},{location.lon:.4f}"
        cached = self._cache.get(cache_key)
        if cached is not None:
            logger.debug("[%s] 缓存命中: %s", self.SOURCE_NAME, location.name)
            return cached

        raw = await self._fetch_with_retry(location)
        forecast = self._parse(raw, location)

        self._cache.set(cache_key, forecast)
        logger.info(
            "[%s] 拉取成功: %s, 共 %d 个预报周期",
            self.SOURCE_NAME,
            location.name,
            forecast.period_count,
        )
        return forecast

    # ------------------------------------------------------------------
    # 子类实现
    # ------------------------------------------------------------------

    @abstractmethod
    async def _fetch_raw(self, location: Location) -> dict:
        """调用 API 获取原始 JSON 响应。"""
        ...

    @abstractmethod
    def _parse(self, raw: dict, location: Location) -> WeatherForecast:
        """将原始 JSON 解析为标准化 WeatherForecast。"""
        ...

    # ------------------------------------------------------------------
    # 内部工具
    # ------------------------------------------------------------------

    async def _fetch_with_retry(self, location: Location) -> dict:
        """带指数退避重试的 API 请求。"""
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
                        "[%s] %s 请求彻底失败，已达最大重试次数 %d",
                        self.SOURCE_NAME,
                        location.name,
                        self._retry_count,
                    )
        raise last_exc  # type: ignore[misc]

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
