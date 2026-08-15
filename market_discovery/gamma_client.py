"""市场发现模块 -- Gamma API 客户端。

真实模式: 轮询 Polymarket Gamma API 按 weather 标签拉取天气盘。
Mock 模式: 生成模拟天气盘（无外网 / 手动指定时使用），每次扫描会随机新增新盘，
           用于本地验证增量发现逻辑。

设计:
  - 默认尝试真实 API，超时/异常自动降级 mock 并给出日志（与项目 mock/live 惯例一致）。
  - fetch_weather_markets() 返回 List[MarketInfo]。
"""

from __future__ import annotations

import asyncio
import logging
import random
from datetime import datetime, timedelta, timezone
from typing import List, Optional

import aiohttp

from .models import MarketInfo, WEATHER_EVENT_TYPES

logger = logging.getLogger(__name__)

GAMMA_API_BASE = "https://gamma-api.polymarket.com"
DEFAULT_TIMEOUT = 15  # 秒


class GammaClient:
    """Polymarket Gamma API 客户端。

    Parameters:
        base_url: Gamma API 根地址。
        tag: 市场标签过滤，默认 weather。
        timeout: 单次请求超时（秒）。
        force_mock: True 时跳过网络直接返回 mock 数据。
        mock_seed: mock 数据随机种子（可复现）。
    """

    def __init__(
        self,
        base_url: str = GAMMA_API_BASE,
        tag: str = "weather",
        timeout: float = DEFAULT_TIMEOUT,
        force_mock: bool = False,
        mock_seed: Optional[int] = None,
    ):
        self.base_url = base_url.rstrip("/")
        self.tag = tag
        self.timeout = timeout
        self.force_mock = force_mock
        self._mock_rng = random.Random(mock_seed)
        self._mock_round = 0  # 模拟轮次计数，驱动"新盘出现"
        self._last_mock: List[MarketInfo] = []

    # ----------------------------------------------------------------
    # 公开接口
    # ----------------------------------------------------------------

    async def fetch_weather_markets(self) -> List[MarketInfo]:
        """拉取天气盘列表。真实 API 失败时自动降级 mock。"""
        if self.force_mock:
            return self.fetch_mock_markets()

        try:
            markets = await self._fetch_from_gamma()
            if not markets:
                logger.warning("Gamma API 返回空列表，降级 mock 数据")
                return self.fetch_mock_markets()
            return markets
        except Exception as e:
            logger.warning("Gamma API 请求失败 (%s)，降级 mock 数据", e)
            return self.fetch_mock_markets()

    async def is_api_reachable(self) -> bool:
        """探测真实 API 是否可达（供启动时提示模式）。"""
        if self.force_mock:
            return False
        try:
            async with aiohttp.ClientSession(
                timeout=aiohttp.ClientTimeout(total=self.timeout)
            ) as session:
                async with session.get(f"{self.base_url}/markets?limit=1") as resp:
                    return resp.status == 200
        except Exception:
            return False

    # ----------------------------------------------------------------
    # 真实 API
    # ----------------------------------------------------------------

    async def _fetch_from_gamma(self) -> List[MarketInfo]:
        """翻页拉取 Gamma API 天气盘，转成 MarketInfo 列表。"""
        markets: List[MarketInfo] = []
        offset = 0
        page_size = 100

        # 只拉未结束、可交易的盘；end_date_min 过滤掉已过期
        params = {
            "tag": self.tag,
            "limit": page_size,
            "offset": offset,
            "active": "true",
            "closed": "false",
        }
        timeout = aiohttp.ClientTimeout(total=self.timeout)
        headers = {"User-Agent": "Mozilla/5.0 (PolymarketWeatherBot/1.0)"}

        async with aiohttp.ClientSession(timeout=timeout, headers=headers) as session:
            while True:
                async with session.get(
                    f"{self.base_url}/markets", params=params
                ) as resp:
                    resp.raise_for_status()
                    page = await resp.json()

                if not isinstance(page, list) or not page:
                    break

                for raw in page:
                    try:
                        m = MarketInfo.from_gamma(raw)
                        if not m.closed and not m.is_expired:
                            markets.append(m)
                    except (ValueError, TypeError) as e:
                        logger.debug("跳过无效市场: %s", e)

                if len(page) < page_size:
                    break
                params["offset"] += page_size
                await asyncio.sleep(0.1)  # 温和节流

        logger.info("Gamma API 返回 %d 个天气盘", len(markets))
        return markets

    # ----------------------------------------------------------------
    # Mock 数据
    # ----------------------------------------------------------------

    def fetch_mock_markets(self) -> List[MarketInfo]:
        """生成模拟天气盘列表。

        模拟"市场滚动上新"：第 0 轮生成 6 个基础盘，之后每轮以 60% 概率
        新增 1~2 个新盘（不同城市/日期），用于验证增量发现。
        """
        cities = [
            "New York", "Chicago", "Miami", "Houston", "Denver",
            "Seattle", "Boston", "Phoenix", "Minneapolis", "Atlanta",
            "Dallas", "Portland",
        ]
        base = ["New York", "Chicago", "Miami", "Houston", "Denver", "Seattle"]

        self._mock_round += 1
        markets: List[MarketInfo] = []
        now = datetime.now(timezone.utc)

        def make(city: str, day_offset: int, event: str, idx: int) -> MarketInfo:
            end = now + timedelta(days=day_offset, hours=self._mock_rng.randint(0, 20))
            if event == "temperature":
                question = (
                    f"Will the high temperature in {city} on "
                    f"{(now + timedelta(days=day_offset)).strftime('%m-%d')} exceed 30°C?"
                )
            else:
                question = (
                    f"Will it {event} in {city} on "
                    f"{(now + timedelta(days=day_offset)).strftime('%m-%d')}?"
                )
            return MarketInfo(
                condition_id=f"mock-{event}-{city.lower().replace(' ', '-')}-{idx}",
                yes_token_id=f"mock-yes-token-{idx}",
                no_token_id=f"mock-no-token-{idx}",
                question=question,
                end_date_utc=end.timestamp(),
                outcome_prices=[
                    round(self._mock_rng.uniform(0.30, 0.75), 4),
                ],
                volume=round(self._mock_rng.uniform(5000, 120000), 2),
                liquidity=round(self._mock_rng.uniform(2000, 30000), 2),
                tags=["weather", event],
            )

        # 第 0 轮: 固定基础盘
        if self._mock_round == 1:
            for i, city in enumerate(base):
                event = WEATHER_EVENT_TYPES[i % len(WEATHER_EVENT_TYPES)]
                markets.append(make(city, 1 + (i % 5), event, i + 1))
        else:
            # 后续轮次: 沿用上一轮全部 + 随机新增
            prev = self._last_mock or []
            markets.extend(prev)
            if self._mock_rng.random() < 0.6:
                new_n = self._mock_rng.randint(1, 2)
                used = {m.location for m in prev}
                fresh = [c for c in cities if c not in used] or cities
                for j in range(new_n):
                    city = self._mock_rng.choice(fresh)
                    event = self._mock_rng.choice(WEATHER_EVENT_TYPES)
                    idx = self._mock_round * 100 + j
                    markets.append(make(city, 1 + self._mock_rng.randint(0, 6), event, idx))

        self._last_mock = markets
        logger.debug("mock 生成 %d 个天气盘 (round=%d)", len(markets), self._mock_round)
        return markets
