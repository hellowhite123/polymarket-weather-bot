"""历史统计策略。

核心逻辑:
  1. 查询 (location, event_type, month) 的历史气候学概率基线
  2. 将当前预报概率与气候学概率混合（lead time 越长，气候学权重越大）
  3. 混合后概率 vs 市场隐含概率 -> 偏差 -> 信号

与其他策略的区别:
  - 预报偏差策略：气象模型概率 vs 市场隐含概率（统计偏差）
  - 多源验证策略：多源融合概率 vs 市场隐含概率（源间分歧）
  - 临近修正策略：实况观测修正后的概率 vs 市场隐含概率（预报准确性）
  - 历史统计策略：气候学混合概率 vs 市场隐含概率（季节性基线）

设计思想:
  市场常对短期预报过度反应。当预报说"明天 80% 降雨"但历史同期
  只有 30% 降雨概率时，真实概率更可能介于两者之间（如 50%）。
  如果市场跟随预报定价在 80%，则存在 BUY_NO 的机会。

  反之，预报说"明天 20% 降雨"但历史同期 70% 降雨概率，
  市场可能定价在 20%（跟随预报），但气候学暗示概率被低估 -> BUY_YES。

混合权重设计:
  - forecast_lead_hours = 1 (临近):  blend = 0.85 (预报占主导)
  - forecast_lead_hours = 24:       blend = 0.60
  - forecast_lead_hours = 72:       blend = 0.35 (气候学占主导)
  - forecast_lead_hours = 168 (7天): blend = 0.20

  公式: forecast_weight = max(0.15, 1.0 - lead_hours / 200)
        climatology_weight = 1.0 - forecast_weight
"""

import logging
import math
from datetime import datetime, timedelta, timezone
from typing import List, Optional

from ..models import (
    EventType,
    MarketSnapshot,
    SignalDirection,
    TradingSignal,
)
from ..statistics.climate_tracker import ClimateTracker
from .base import BaseStrategy
from .forecast_utils import extract_probability, get_location

logger = logging.getLogger(__name__)


class HistoricalStatisticalStrategy(BaseStrategy):
    """历史统计策略。

    用长期气候学基线修正短期预报概率，修正后概率与市场隐含概率
    的偏差超过阈值时生成信号。

    用法:
        tracker = ClimateTracker()
        # ... 导入历史数据 ...
        strategy = HistoricalStatisticalStrategy(tracker, params)
        signals = await strategy.evaluate_batch(
            forecasts=[...],
            markets=[...],
            event_types=[...],
            target_hours_ahead=24,
        )
    """

    def __init__(
        self,
        tracker: ClimateTracker,
        params=None,
    ):
        super().__init__(name="historical_statistical")
        if params is None:
            from ..config import HistoricalParams
            params = HistoricalParams()
        self.tracker = tracker
        self.params = params

    # ================================================================
    # 公共 API
    # ================================================================

    async def evaluate(self) -> List[TradingSignal]:
        """抽象方法实现（单次调用请用 evaluate_batch）。"""
        return []

    async def evaluate_batch(
        self,
        forecasts: list,
        markets: list[MarketSnapshot],
        event_types: list[EventType],
        target_hours_ahead: int = 24,
        record_data: bool = True,
    ) -> List[TradingSignal]:
        """批量评估多个 (预报, 市场) 对。

        Args:
            forecasts: WeatherForecast 列表
            markets: MarketSnapshot 列表
            event_types: EventType 列表
            target_hours_ahead: 预报目标时间窗口（小时）
            record_data: 是否将本次数据录入追踪器

        Returns:
            TradingSignal 列表
        """
        if not (len(forecasts) == len(markets) == len(event_types)):
            raise ValueError(
                f"forecasts({len(forecasts)}), markets({len(markets)}), "
                f"event_types({len(event_types)}) 长度必须一致"
            )

        signals: List[TradingSignal] = []

        for forecast, market, event_type in zip(forecasts, markets, event_types):
            signal = self._evaluate_one(
                forecast, market, event_type, target_hours_ahead, record_data
            )
            if signal is not None:
                signals.append(signal)
                self._increment_counter()

        logger.info(
            "历史统计策略评估完成: %d 个输入, %d 个信号",
            len(forecasts), len(signals),
        )
        return signals

    def evaluate_sync(
        self,
        forecast,
        market: MarketSnapshot,
        event_type: EventType,
        target_hours_ahead: int = 24,
        record_data: bool = True,
    ) -> Optional[TradingSignal]:
        """同步评估单个 (预报, 市场) 对。"""
        return self._evaluate_one(
            forecast, market, event_type, target_hours_ahead, record_data
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
        record_data: bool,
    ) -> Optional[TradingSignal]:
        """评估单个预报-市场对。"""

        now = datetime.now(timezone.utc)

        # Step 1: 提取当前预报概率
        forecast_prob = extract_probability(
            forecast, event_type.value, target_hours_ahead
        )
        if forecast_prob is None:
            logger.debug(
                "无法从预报提取概率: %s, event=%s",
                getattr(forecast, "location_name", "unknown"),
                event_type.value,
            )
            return None

        location = get_location(forecast)

        # Step 2: 查询历史气候学概率
        month = now.month
        clim_result = self.tracker.get_smoothed_climatology(
            location, event_type, month,
            window=self.params.smoothing_window,
        )

        if clim_result is None:
            logger.debug(
                "无气候学数据: %s, event=%s, month=%d",
                location, event_type.value, month,
            )
            return None

        clim_mean, clim_std, clim_count = clim_result

        if clim_count < self.params.min_climatology_samples:
            logger.debug(
                "气候学样本不足: %d < %d (%s, %s, month=%d)",
                clim_count, self.params.min_climatology_samples,
                location, event_type.value, month,
            )
            return None

        # Step 3: 计算混合权重
        forecast_weight = self._compute_blend_weight(target_hours_ahead)
        climatology_weight = 1.0 - forecast_weight

        # Step 4: 混合概率
        blended_prob = (
            forecast_prob * forecast_weight
            + clim_mean * climatology_weight
        )
        blended_prob = max(0.0, min(1.0, blended_prob))

        # Step 5: 与市场比较
        market_prob = market.implied_probability
        deviation = blended_prob - market_prob

        if abs(deviation) < self.params.min_deviation_abs:
            logger.debug(
                "历史统计偏差 %.4f < 最小阈值 %.4f，跳过",
                deviation, self.params.min_deviation_abs,
            )
            return None

        # Step 6: 季节性偏差 = 预报 vs 气候学
        seasonal_deviation = forecast_prob - clim_mean

        # Step 7: 录入追踪器（可选）
        if record_data:
            self.tracker.add_observation(
                location, event_type, month, forecast_prob,
            )

        # Step 8: 生成信号
        direction = self._determine_direction(deviation)
        confidence = self._calculate_confidence(
            deviation, seasonal_deviation, clim_std, clim_count,
            forecast_weight,
        )
        quantity = self._calculate_quantity(deviation, confidence)

        if confidence < self.params.min_confidence:
            logger.debug(
                "置信度 %.3f < 最小阈值 %.3f，丢弃",
                confidence, self.params.min_confidence,
            )
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
            target_price=round(blended_prob, 4),
            market_price=round(market.price_for(direction), 4),
            quantity=quantity,
            confidence=confidence,
            deviation=round(deviation, 4),
            sigma_level=round(
                abs(seasonal_deviation) / clim_std if clim_std > 1e-10 else 0.0, 2
            ),
            generated_at=now,
            expires_at=now + ttl,
            metadata={
                "forecast_prob": round(forecast_prob, 4),
                "climatology_prob": round(clim_mean, 4),
                "climatology_std": round(clim_std, 4),
                "climatology_count": clim_count,
                "blended_prob": round(blended_prob, 4),
                "forecast_weight": round(forecast_weight, 4),
                "climatology_weight": round(climatology_weight, 4),
                "seasonal_deviation": round(seasonal_deviation, 4),
                "target_hours": target_hours_ahead,
                "month": month,
            },
        )

        logger.info(
            "历史统计信号: %s %s token=%s dev=%.4f seasonal=%.4f conf=%.3f",
            direction.value,
            event_type.value,
            market.token_id[:12],
            deviation,
            seasonal_deviation,
            confidence,
        )

        return signal

    # ================================================================
    # 混合权重计算
    # ================================================================

    def _compute_blend_weight(self, lead_hours: float) -> float:
        """计算预报在混合概率中的权重。

        lead time 越长，预报越不确定，气候学权重越大。

        公式: forecast_weight = max(min_weight, 1.0 - lead_hours / decay_factor)

        默认参数下:
          - 1h:   0.995 -> 截断为 0.85 (上限)
          - 24h:  0.88  -> 截断为 0.85 (上限)
          - 72h:  0.64
          - 168h: 0.16
          - 200h+: 0.15 (下限)

        Args:
            lead_hours: 预报提前时间（小时）

        Returns:
            预报权重 (0.0~1.0)
        """
        raw = 1.0 - lead_hours / self.params.lead_time_decay_hours
        return max(
            self.params.min_forecast_weight,
            min(self.params.max_forecast_weight, raw),
        )

    # ================================================================
    # 信号生成辅助
    # ================================================================

    @staticmethod
    def _determine_direction(deviation: float) -> SignalDirection:
        """根据偏差方向决定交易方向。

        deviation > 0: 混合概率 > 市场概率 -> 市场低估 -> BUY_YES
        deviation < 0: 混合概率 < 市场概率 -> 市场高估 -> BUY_NO
        """
        return SignalDirection.BUY_YES if deviation > 0 else SignalDirection.BUY_NO

    def _calculate_confidence(
        self,
        deviation: float,
        seasonal_deviation: float,
        clim_std: float,
        clim_count: int,
        forecast_weight: float,
    ) -> float:
        """计算信号置信度。

        置信度因子:
          1. 偏差幅度: |deviation| 越大越自信
          2. 季节性异常度: |seasonal_deviation| / clim_std 越大越自信
          3. 样本充足度: clim_count 越多越自信
          4. 气候学权重: 气候学占混合比例越高越自信

        公式:
          base = sigmoid(|deviation| * scale)
          seasonal_factor = 1 + min(|seasonal_deviation| / max(clim_std, eps), 3) * boost
          sample_factor = min(clim_count / 30, 1.0)
          clim_weight_factor = 1.0 - forecast_weight  (气候学权重)

          confidence = base * (0.5 + 0.3 * sample_factor + 0.2 * clim_weight_factor)
                       * min(seasonal_factor, 2.0)
        """
        # 基础置信度：偏差幅度的 sigmoid
        dev_abs = abs(deviation)
        base = 1.0 / (1.0 + math.exp(-dev_abs * 10.0 + 2.0))
        # 映射到 0.3~0.95
        base = 0.3 + 0.65 * base

        # 季节性异常因子
        eps = 0.01
        seasonal_ratio = abs(seasonal_deviation) / max(clim_std, eps)
        seasonal_factor = 1.0 + min(seasonal_ratio, 3.0) * self.params.seasonal_confidence_boost

        # 样本充足度
        sample_factor = min(clim_count / 30.0, 1.0)

        # 气候学权重因子（气候学占混合比例越高，基线越可靠）
        clim_weight = 1.0 - forecast_weight
        weight_factor = 0.5 + 0.3 * sample_factor + 0.2 * clim_weight

        raw = base * weight_factor * min(seasonal_factor, 2.0)
        return round(max(0.0, min(1.0, raw)), 4)

    def _calculate_quantity(self, deviation: float, confidence: float) -> float:
        """计算建议交易数量。"""
        base = abs(deviation) * 80.0 * confidence
        return round(min(base, self.params.max_position_per_market), 2)
