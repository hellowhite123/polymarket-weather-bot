"""市场发现模块 -- 定时扫描器。

MarketScanner 负责按固定间隔（默认 10 分钟）拉取天气盘，
通过 MarketRegistry 增量去重，发现新盘后触发回调。

用法（库方式）:
    scanner = MarketScanner(interval_seconds=600)
    await scanner.start(on_new_markets=my_callback)   # 常驻
    或
    new = await scanner.scan_once()                    # 单次
"""

from __future__ import annotations

import asyncio
import logging
from typing import Awaitable, Callable, List, Optional

from .gamma_client import GammaClient
from .models import MarketInfo
from .registry import MarketRegistry, DEFAULT_REGISTRY_PATH

logger = logging.getLogger(__name__)

DEFAULT_INTERVAL_SECONDS = 600  # 10 分钟

# 新盘回调签名: Callable[[List[MarketInfo]], Awaitable[None]]
NewMarketsHandler = Callable[[List[MarketInfo]], Awaitable[None]]


class MarketScanner:
    """定时扫描 Polymarket 天气盘，发现新开的盘。

    Parameters:
        interval_seconds: 扫描间隔（秒），默认 600 (10 分钟)。
        registry_path: 注册表存储路径。
        client: 自定义 GammaClient（注入 mock/真实模式）。
        graceful_delay: 扫描失败后的退避等待（秒），避免失败时忙轮询。
    """

    def __init__(
        self,
        interval_seconds: int = DEFAULT_INTERVAL_SECONDS,
        registry_path: str = DEFAULT_REGISTRY_PATH,
        client: Optional[GammaClient] = None,
        graceful_delay: float = 5.0,
    ):
        self.interval = interval_seconds
        self.registry = MarketRegistry(registry_path)
        self.client = client or GammaClient()
        self.graceful_delay = graceful_delay
        self._running = False
        self._stop_event: Optional[asyncio.Event] = None
        self.scan_count = 0
        self.total_new_markets = 0

    # ----------------------------------------------------------------
    # 单次扫描
    # ----------------------------------------------------------------

    async def scan_once(self) -> List[MarketInfo]:
        """执行一次完整扫描：拉取 -> 去重 -> 返回新盘列表。"""
        scanned = await self.client.fetch_weather_markets()
        new_markets = self.registry.diff(scanned)
        self.scan_count += 1
        self.total_new_markets += len(new_markets)

        logger.info(
            "扫描 #%d: 拉到 %d 个天气盘, 新开 %d 个, 注册表累计 %d 个",
            self.scan_count, len(scanned), len(new_markets),
            self.registry.count(),
        )
        if new_markets:
            for m in new_markets:
                logger.info(
                    "  [NEW] %s | %s | 截止 %s | token=%s...",
                    m.location or m.question[:40], m.event_type,
                    _fmt_ts(m.end_date_utc), m.token_id[:12],
                )
        return new_markets

    # ----------------------------------------------------------------
    # 常驻循环
    # ----------------------------------------------------------------

    async def start(self, on_new_markets: Optional[NewMarketsHandler] = None):
        """启动常驻扫描循环，直到 stop() 被调用。

        第一次扫描立即执行，之后每 interval 秒执行一次。
        """
        if self._running:
            raise RuntimeError("scanner 已在运行")
        self._running = True
        self._stop_event = asyncio.Event()

        logger.info(
            "市场扫描器启动: 间隔 %d 秒 (%.1f 分钟), 注册表 %s",
            self.interval, self.interval / 60, self.registry.path,
        )

        try:
            while not self._stop_event.is_set():
                try:
                    new_markets = await self.scan_once()
                    if new_markets and on_new_markets:
                        await on_new_markets(new_markets)
                except asyncio.CancelledError:
                    raise
                except Exception as e:
                    logger.error("扫描异常: %s", e, exc_info=True)
                    # 失败退避，避免错误风暴
                    await self._wait(self.graceful_delay)
                    continue

                await self._wait(self.interval)
        finally:
            self._running = False
            self._stop_event = None

    def stop(self):
        """请求停止扫描循环（线程安全，需在事件循环内执行）。"""
        if self._stop_event is not None:
            self._stop_event.set()

    async def _wait(self, seconds: float):
        try:
            await asyncio.wait_for(self._stop_event.wait(), timeout=seconds)
        except asyncio.TimeoutError:
            pass

    @property
    def is_running(self) -> bool:
        return self._running


def _fmt_ts(ts: Optional[float]) -> str:
    """Unix 时间戳 -> 本地可读时间。"""
    if not ts:
        return "?"
    from datetime import datetime
    return datetime.fromtimestamp(ts).strftime("%m-%d %H:%M")
