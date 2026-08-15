"""预报偏差策略。

核心逻辑:
  1. 从 WeatherForecast 提取目标时间窗口的天气概率
  2. 与市场 YES 价格（隐含概率）比较
  3. 计算偏差: deviation = P_model - P_market
  4. 与历史偏差分布比较: 若 |deviation - mu| > k*sigma，生成交易信号

信号方向:
  - deviation > mu + k*sigma: 市场低估事件概率 -> BUY_YES
  - deviation < mu - k*sigma: 市场高估事件概率 -> BUY_NO
"""

import logging
import math
from datetime import datetime, timedelta, timezone
from typing import List, Optional

from ..config import DeviationStrategyParams
from ..models import (
    DeviationRecord,
    EventType,
    MarketSnapshot,
    SignalDirection,
    TradingSignal,
)
from ..statistics.deviation_tracker import DeviationTracker
from .base import BaseStrategy
from .forecast_utils import extract_probability, get_location

logger = logging.getLogger(__name__)


class ForecastDeviationStrategy(BaseStrategy):
    """预报偏差策略。

    当气象模型概率与市场隐含概率之间的偏差超过
    历史分布的 k*sigma 阈值时，生成交易信号。

    用法:
        tracker = DeviationTracker(window_size=100)
        strategy = ForecastDeviationStrategy(tracker, params)
        signals = await strategy.evaluate_batch(
            forecasts=[...],
            markets=[...],
        )
    """

    def __init__(
        self,
        tracker: DeviationTracker,
        params: Optional[DeviationStrategyParams] = None,
    ):
        super().__init__(name="forecast_deviation")
        self.tracker = tracker
        self.params = params or DeviationStrategyParams()

    # ================================================================
    # 公共 API
    # ================================================================

    async def evaluate(self) -> List[TradingSignal]:
        """抽象方法实现（单次调用请用 evaluate_batch）。"""
        return []

    async def evaluate_batch(
        self,
        forecasts: list,               # list[WeatherForecast]
        markets: list[MarketSnapshot],
        event_types: list[EventType],
        target_hours_ahead: int = 24,
        record_deviations: bool = True,
    ) -> List[TradingSignal]:
        """批量评估多个 (预报, 市场) 对，生成交易信号。

        Args:
            forecasts: WeatherForecast 列表
            markets: MarketSnapshot 列表（与 forecasts 一一对应）
            event_types: EventType 列表（与 forecasts 一一对应）
            target_hours_ahead: 目标时间窗口（小时），从当前时间起算
            record_deviations: 是否将本次偏差录入 tracker

        Returns:
            TradingSignal 列表（仅包含触发阈值的信号）
        """
        if not (len(forecasts) == len(markets) == len(event_types)):
            raise ValueError(
                f"forecasts({len(forecasts)}), markets({len(markets)}), "
                f"event_types({len(event_types)}) 长度必须一致"
            )

        signals: List[TradingSignal] = []

        for forecast, market, event_type in zip(forecasts, markets, event_types):
            signal = self._evaluate_one(
                forecast, market, event_type, target_hours_ahead, record_deviations
            )
            if signal is not None:
                signals.append(signal)
                self._increment_counter()

        logger.info(
            "预报偏差策略评估完成: %d 个输入, %d 个信号",
            len(forecasts), len(signals),
        )
        return signals

    def evaluate_sync(
        self,
        forecast,           # WeatherForecast
        market: MarketSnapshot,
        event_type: EventType,
        target_hours_ahead: int = 24,
        record_deviation: bool = True,
    ) -> Optional[TradingSignal]:
        """同步评估单个 (预报, 市场) 对（非异步版本，方便测试）。"""
        return self._evaluate_one(
            forecast, market, event_type, target_hours_ahead, record_deviation
        )

    # ================================================================
    # 内部逻辑
    # ================================================================

    def _evaluate_one(
        self,
        forecast,
        market: MarketSnapshot,
        event_type: EventType,
        target_hours_ahead: int,
        record_deviation: bool,
    ) -> Optional[TradingSignal]:
        """评估单个预报-市场对。"""

        # Step 1: 提取气象概率
        model_prob = self._extract_probability(forecast, event_type, target_hours_ahead)
        if model_prob is None:
            logger.debug(
                "无法从预报提取概率: %s, event=%s", forecast.location_name, event_type.value
            )
            return None

        # Step 2: 获取市场隐含概率
        market_prob = market.implied_probability

        # Step 3: 计算偏差
        deviation = model_prob - market_prob

        # 绝对偏差太小，过滤噪声
        if abs(deviation) < self.params.min_deviation_abs:
            logger.debug(
                "偏差 %.4f < 最小阈值 %.4f，跳过",
                deviation, self.params.min_deviation_abs,
            )
            return None

        # Step 4: 创建偏差记录
        location = self._get_location(forecast)
        record = DeviationRecord(
            timestamp=datetime.now(timezone.utc),
            model_prob=model_prob,
            market_prob=market_prob,
            deviation=deviation,
            event_type=event_type,
            location=location,
            token_id=market.token_id,
            model_source=forecast.source.source if hasattr(forecast, 'source') else "",
        )

        # Step 5: 判断是否极端
        is_extreme, _, mu, sigma_level = self.tracker.is_extreme(
            record, sigma=self.params.deviation_threshold_sigma
        )

        # Step 6: 录入偏差追踪器
        if record_deviation:
            self.tracker.record(record)

        if not is_extreme:
            return None

        # Step 7: 生成信号
        direction = self._determine_direction(deviation, mu)
        confidence = self._calculate_confidence(sigma_level)
        quantity = self._calculate_quantity(deviation, sigma_level)

        if confidence < self.params.min_confidence:
            logger.debug("置信度 %.3f < 最小阈值 %.3f，丢弃信号", confidence, self.params.min_confidence)
            return None

        ttl = timedelta(seconds=self.params.signal_ttl_seconds)

        signal = TradingSignal(
            strategy=self.name,
            direction=direction,
            token_id=market.token_for(direction),
            event_type=event_type,
            location=location,
            condition_id=market.condition_id,
            yes_token_id=market.yes_token_id,
            no_token_id=market.no_token_id,
            target_price=round(model_prob, 4),
            market_price=round(market.price_for(direction), 4),
            quantity=quantity,
            confidence=confidence,
            deviation=round(deviation, 4),
            sigma_level=round(sigma_level, 2),
            generated_at=datetime.now(timezone.utc),
            expires_at=datetime.now(timezone.utc) + ttl,
            metadata={
                "mu": round(mu, 6),
                "sigma": round(self.tracker.stats(event_type, location, market.token_id)[1], 6),
                "model_source": record.model_source,
                "target_hours": target_hours_ahead,
            },
        )

        logger.info(
            "信号生成: %s %s token=%s dev=%.4f sigma=%.2f conf=%.3f",
            direction.value, event_type.value, market.token_id[:12],
            deviation, sigma_level, confidence,
        )

        return signal

    # ----- 概率提取 (委托给公共工具) -----

    def _extract_probability(
        self,
        forecast,
        event_type: EventType,
        target_hours_ahead: int,
    ) -> Optional[float]:
        """从 WeatherForecast 中提取目标时间窗口的气象概率。

        委托给 forecast_utils.extract_probability, 保持向后兼容。
        """
        return extract_probability(
            forecast, event_type.value, target_hours_ahead
        )

    # ----- 信号生成辅助 -----

    @staticmethod
    def _determine_direction(deviation: float, mu: float) -> SignalDirection:
        """根据偏差方向决定交易方向。

        deviation > mu -> 气象概率高于历史均值 -> 市场低估 -> BUY_YES
        deviation < mu -> 气象概率低于历史均值 -> 市场高估 -> BUY_NO
        """
        if deviation > mu:
            return SignalDirection.BUY_YES
        else:
            return SignalDirection.BUY_NO

    @staticmethod
    def _calculate_confidence(sigma_level: float) -> float:
        """根据 sigma 倍数计算置信度。

        2sigma -> 0.50
        3sigma -> 0.75
        4sigma -> 0.90
        5sigma+ -> 0.95+

        使用 sigmoid-like 函数平滑映射。
        """
        if sigma_level <= 1.0:
            return 0.0
        if sigma_level >= 10:
            return 0.99
        raw = 1.0 - math.exp(2.0 - sigma_level)
        return round(max(0.0, min(1.0, raw)), 4)

    def _calculate_quantity(self, deviation: float, sigma_level: float) -> float:
        """计算建议交易数量。

        基于偏差幅度和置信度，输出基础仓位。
        实际仓位由 Rust 端风控模块做最终限制。
        """
        # 基础仓位 = 偏差幅度 * 100（偏差 0.1 = 10 shares）
        base = abs(deviation) * 100.0 * min(sigma_level / 2.0, 2.0)
        return round(min(base, self.params.max_position_per_market), 2)

    @staticmethod
    def _get_location(forecast) -> str:
        """从 WeatherForecast 提取位置标识。"""
        return get_location(forecast)
