"""市场发现模块 -- 新盘到策略信号的桥接器。

闭环链路:
  扫描器发现新盘 → 匹配本地气象预报 (data/raw) → 策略引擎单盘精确评估 → 信号

配合 scanner.start(on_new_markets=bridge.on_new_markets) 使用，
或直接调用 evaluate_market() 手动评估。
"""

from __future__ import annotations

import glob
import json
import logging
import os
from datetime import datetime, timezone
from typing import List, Optional

from .models import MarketInfo, normalize_location

logger = logging.getLogger(__name__)

DEFAULT_FORECAST_DIR = "./data/raw"
DEFAULT_SIGNAL_DIR = "./data/signals"


class MarketSignalBridge:
    """把扫描发现的新盘接入策略引擎，触发信号评估。

    Parameters:
        config: StrategyConfig（None 则用默认/环境变量）。
        forecast_dir: 气象预报 JSON 目录（weather_data_ingestion 产物）。
        signal_dir: 信号输出目录。
        registry_path: market_discovery 注册表路径（记录评估过的盘）。
        target_hours: 策略目标时间窗口（小时）。
    """

    def __init__(
        self,
        config=None,
        forecast_dir: str = DEFAULT_FORECAST_DIR,
        signal_dir: str = DEFAULT_SIGNAL_DIR,
        registry_path: str = "./data/market_registry.json",
        target_hours: int = 24,
    ):
        self.forecast_dir = forecast_dir
        self.signal_dir = signal_dir
        self.registry_path = registry_path
        self.target_hours = target_hours

        from strategy_engine.config import StrategyConfig
        from strategy_engine.pipeline import StrategyPipeline

        self.config = config or StrategyConfig.from_env()
        self.pipeline = StrategyPipeline(self.config)

        self.evaluated_count = 0
        self.signal_count = 0

    # ----------------------------------------------------------------
    # 预报匹配
    # ----------------------------------------------------------------

    def find_forecast_files(self, market: MarketInfo) -> List[str]:
        """在 forecast_dir 中查找匹配该盘城市的预报文件（按时间倒序）。

        文件名形如 "<source>_<Location>_<ts>.json"，城市名与市场 location
        归一化后匹配（New York == newyork == NewYork）。
        """
        if not os.path.isdir(self.forecast_dir):
            logger.warning("预报目录不存在: %s", self.forecast_dir)
            return []

        target = normalize_location(market.location)
        matched = []
        for f in os.listdir(self.forecast_dir):
            if not f.endswith(".json"):
                continue
            if target and target in normalize_location(os.path.basename(f)):
                matched.append(os.path.join(self.forecast_dir, f))

        matched.sort(key=os.path.getmtime, reverse=True)
        return matched

    # ----------------------------------------------------------------
    # 单盘评估
    # ----------------------------------------------------------------

    async def evaluate_market(self, market: MarketInfo) -> list:
        """对单个新盘执行策略评估（预报偏差策略），返回 TradingSignal 列表。

        精确匹配: 直接以该盘的 token_id / yes_price 构造 MarketSnapshot，
        不走 pipeline.run 的注册表重加载，避免同城同类型多盘时匹配错位。
        """
        from strategy_engine.models import EventType, MarketSnapshot

        forecast_files = self.find_forecast_files(market)
        if not forecast_files:
            logger.info(
                "跳过 [%s/%s]: 无本地气象预报数据 (%s)",
                market.location, market.event_type, market.condition_id,
            )
            return []

        forecasts = self.pipeline._load_forecasts(forecast_files)
        if not forecasts:
            logger.warning("预报文件加载失败: %s", forecast_files)
            return []

        snapshot = MarketSnapshot(**market.to_market_snapshot())
        signals = await self.pipeline.deviation_strategy.evaluate_batch(
            forecasts=[forecasts[0]],          # 取该城市最新一份预报
            markets=[snapshot],
            event_types=[EventType(market.event_type)],
            target_hours_ahead=self.target_hours,
            record_deviations=True,
        )

        # 打上市场来源标记
        for s in signals:
            s.metadata["condition_id"] = market.condition_id
            s.metadata["question"] = market.question
            s.metadata["end_date_utc"] = market.end_date_utc

        if signals:
            self.pipeline._save_signals(signals)
            self.signal_count += len(signals)
            for s in signals:
                logger.info(
                    "  [SIGNAL] %s %s 目标=%.4f 市场=%.4f σ=%.1f conf=%.3f",
                    s.direction.value, market.location,
                    s.target_price, s.market_price, s.sigma_level, s.confidence,
                )
        else:
            logger.info("  [%s/%s] 无信号（偏差在正常范围）", market.location, market.event_type)

        self.evaluated_count += 1
        return signals

    # ----------------------------------------------------------------
    # 扫描器回调入口
    # ----------------------------------------------------------------

    async def on_new_markets(self, new_markets: List[MarketInfo]) -> list:
        """MarketScanner 的新盘回调：逐个评估，聚合返回所有信号。

        用法:
            scanner.start(on_new_markets=bridge.on_new_markets)
        """
        all_signals = []
        logger.info("=== 新盘评估开始: %d 个新盘 ===", len(new_markets))
        for market in new_markets:
            try:
                signals = await self.evaluate_market(market)
                all_signals.extend(signals)
            except Exception as e:
                logger.error("评估新盘异常 [%s]: %s", market.condition_id, e)
        logger.info("=== 新盘评估结束: %d 信号 ===", len(all_signals))
        return all_signals

    # ----------------------------------------------------------------
    # 统计
    # ----------------------------------------------------------------

    def summary(self) -> dict:
        return {
            "evaluated_markets": self.evaluated_count,
            "signals_generated": self.signal_count,
            "forecast_dir": self.forecast_dir,
        }
