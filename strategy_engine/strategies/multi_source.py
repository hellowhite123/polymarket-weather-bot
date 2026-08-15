"""多源验证策略。

核心逻辑:
  1. 输入多个数据源 (NOAA/ECMWF/彩云) 的 WeatherForecast
  2. 对齐时间窗口, 提取每个源的概率 P_1, P_2, ..., P_n
  3. 计算源间分歧度: divergence = max(|P_i - P_j|)
  4. 与历史分歧基线比较, 判断当前分歧是否异常
  5. 加权融合概率: P_fused = sum(w_i * P_i), 权重基于历史准确率
  6. 计算融合偏差: market_dev = P_fused - P_market
  7. 与历史融合偏差基线比较, 判断市场是否定价错误

信号方向:
  - 分歧异常 + 融合偏差 > mu + k*sigma -> BUY_YES
  - 分歧异常 + 融合偏差 < mu - k*sigma -> BUY_NO
  - 分歧正常 + 融合偏差极端 -> 信号 (但置信度降低, 因源间一致)

与预报偏差策略的区别:
  - 预报偏差: 单一源 vs 市场, 检测 model-market 偏差
  - 多源验证: 多源融合 vs 市场, 检测 source-source 分歧 + 融合-market 偏差
  - 多源策略在分歧大时有独特优势: 市场往往折中定价, 信息不对称更严重
"""

import logging
import math
from datetime import datetime, timedelta, timezone
from itertools import combinations
from typing import Dict, List, Optional, Tuple

from ..config import MultiSourceStrategyParams
from ..models import (
    DeviationRecord,
    DivergenceRecord,
    EventType,
    MarketSnapshot,
    SignalDirection,
    TradingSignal,
)
from ..statistics.deviation_tracker import DeviationTracker
from ..statistics.divergence_tracker import DivergenceTracker
from .base import BaseStrategy
from .forecast_utils import extract_probability, get_location, get_source_name

logger = logging.getLogger(__name__)


class MultiSourceStrategy(BaseStrategy):
    """多源验证策略。

    当多个气象数据源之间出现异常分歧, 且融合概率与市场隐含概率
    之间的偏差也超过阈值时, 生成交易信号。

    分歧越大, 信息不对称越严重, 置信度乘数越高。

    用法:
        div_tracker = DivergenceTracker(window_size=100)
        dev_tracker = DeviationTracker(window_size=100)
        strategy = MultiSourceStrategy(div_tracker, dev_tracker, params)
        signals = await strategy.evaluate_batch(
            multi_forecasts=[[fc_noaa, fc_caiyun], ...],
            markets=[...],
        )
    """

    def __init__(
        self,
        divergence_tracker: DivergenceTracker,
        deviation_tracker: DeviationTracker,
        params: Optional[MultiSourceStrategyParams] = None,
    ):
        super().__init__(name="multi_source")
        self.div_tracker = divergence_tracker
        self.dev_tracker = deviation_tracker
        self.params = params or MultiSourceStrategyParams()

    # ================================================================
    # 公共 API
    # ================================================================

    async def evaluate(self) -> List[TradingSignal]:
        """抽象方法实现（单次调用请用 evaluate_batch）。"""
        return []

    async def evaluate_batch(
        self,
        multi_forecasts: List[list],        # list[list[WeatherForecast]]
        markets: List[MarketSnapshot],
        event_types: List[EventType],
        target_hours_ahead: int = 24,
        record_data: bool = True,
    ) -> List[TradingSignal]:
        """批量评估多组多源预报, 生成交易信号。

        Args:
            multi_forecasts: 每个元素是一个源的 WeatherForecast 列表,
                             外层与 markets 一一对应
            markets: MarketSnapshot 列表
            event_types: EventType 列表
            target_hours_ahead: 目标时间窗口
            record_data: 是否将本次数据录入追踪器

        Returns:
            TradingSignal 列表
        """
        if not (len(multi_forecasts) == len(markets) == len(event_types)):
            raise ValueError(
                f"multi_forecasts({len(multi_forecasts)}), "
                f"markets({len(markets)}), "
                f"event_types({len(event_types)}) 长度必须一致"
            )

        signals: List[TradingSignal] = []

        for forecasts, market, event_type in zip(
            multi_forecasts, markets, event_types
        ):
            signal = self._evaluate_one(
                forecasts, market, event_type,
                target_hours_ahead, record_data,
            )
            if signal is not None:
                signals.append(signal)
                self._increment_counter()

        logger.info(
            "多源验证策略评估完成: %d 组输入, %d 个信号",
            len(multi_forecasts), len(signals),
        )
        return signals

    def evaluate_sync(
        self,
        forecasts: list,           # list[WeatherForecast]
        market: MarketSnapshot,
        event_type: EventType,
        target_hours_ahead: int = 24,
        record_data: bool = True,
    ) -> Optional[TradingSignal]:
        """同步评估单组多源预报。"""
        return self._evaluate_one(
            forecasts, market, event_type,
            target_hours_ahead, record_data,
        )

    # ================================================================
    # 内部逻辑
    # ================================================================

    def _evaluate_one(
        self,
        forecasts: list,           # list[WeatherForecast] (多个源)
        market: MarketSnapshot,
        event_type: EventType,
        target_hours_ahead: int,
        record_data: bool,
    ) -> Optional[TradingSignal]:
        """评估单组多源预报。"""

        if len(forecasts) < 2:
            logger.debug("多源策略至少需要 2 个源的预报, 当前 %d", len(forecasts))
            return None

        # Step 1: 提取每个源的概率
        source_probs: Dict[str, float] = {}
        for fc in forecasts:
            prob = extract_probability(
                fc, event_type.value, target_hours_ahead
            )
            if prob is None:
                logger.debug(
                    "源 %s 无法提取概率, 跳过",
                    get_source_name(fc),
                )
                continue
            source_name = get_source_name(fc)
            source_probs[source_name] = prob

        if len(source_probs) < 2:
            logger.debug("有效源不足 2 个, 跳过多源验证")
            return None

        # Step 2: 计算源间分歧度
        divergence = self._calc_divergence(source_probs)

        if divergence < self.params.min_divergence_abs:
            logger.debug(
                "源间分歧 %.4f < 最小阈值 %.4f, 源间一致",
                divergence, self.params.min_divergence_abs,
            )
            # 源间一致时, 可以退化为预报偏差策略逻辑
            # 但这里我们让多源策略专注分歧场景, 一致的情况交给预报偏差策略
            return None

        # Step 3: 构造源对标识
        source_pair = DivergenceTracker.make_source_pair(
            list(source_probs.keys())
        )

        # Step 4: 获取融合权重
        location = get_location(forecasts[0])
        weights = self.div_tracker.get_source_weights(
            event_type, location, source_pair
        )

        # Step 5: 计算融合概率
        fused_prob = self._calc_fused_prob(source_probs, weights)

        # Step 6: 创建分歧记录并判断是否异常
        div_record = DivergenceRecord(
            timestamp=datetime.now(timezone.utc),
            source_probs=dict(source_probs),
            fused_prob=fused_prob,
            divergence=divergence,
            event_type=event_type,
            location=location,
            token_id=market.token_id,
            source_pair=source_pair,
        )

        div_is_extreme, _, div_mu, div_sigma = self.div_tracker.is_extreme(
            div_record, sigma=self.params.divergence_threshold_sigma
        )

        # Step 7: 创建融合偏差记录并判断是否异常
        market_prob = market.implied_probability
        market_dev = fused_prob - market_prob

        if abs(market_dev) < self.params.min_market_div_abs:
            logger.debug(
                "融合偏差 %.4f < 最小阈值 %.4f, 市场定价合理",
                market_dev, self.params.min_market_div_abs,
            )
            if record_data:
                self._record_data(div_record, market_dev, event_type,
                                  location, market.token_id, source_probs,
                                  fused_prob, divergence, source_pair)
            return None

        dev_record = DeviationRecord(
            timestamp=datetime.now(timezone.utc),
            model_prob=fused_prob,
            market_prob=market_prob,
            deviation=market_dev,
            event_type=event_type,
            location=location,
            token_id=market.token_id,
            model_source="fused",
        )

        dev_is_extreme, _, dev_mu, dev_sigma = self.dev_tracker.is_extreme(
            dev_record, sigma=self.params.market_div_threshold_sigma
        )

        # Step 8: 录入数据
        if record_data:
            self._record_data(div_record, market_dev, event_type,
                              location, market.token_id, source_probs,
                              fused_prob, divergence, source_pair)

        # Step 9: 判断是否生成信号
        # 条件: 融合偏差异常 (必须), 分歧异常 (放大置信度)
        if not dev_is_extreme:
            logger.debug(
                "融合偏差 %.4f 未超过 %.1f sigma 阈值 (sigma_level=%.2f)",
                market_dev, self.params.market_div_threshold_sigma, dev_sigma,
            )
            return None

        # Step 10: 生成信号
        direction = self._determine_direction(market_dev, dev_mu)
        confidence = self._calculate_confidence(
            dev_sigma, div_sigma, div_is_extreme
        )
        quantity = self._calculate_quantity(market_dev, dev_sigma, divergence)

        if confidence < self.params.min_confidence:
            logger.debug(
                "置信度 %.3f < 最小阈值 %.3f, 丢弃信号",
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
            target_price=round(fused_prob, 4),
            market_price=round(market.price_for(direction), 4),
            quantity=quantity,
            confidence=confidence,
            deviation=round(market_dev, 4),
            sigma_level=round(dev_sigma, 2),
            generated_at=datetime.now(timezone.utc),
            expires_at=datetime.now(timezone.utc) + ttl,
            metadata={
                "fused_prob": round(fused_prob, 4),
                "divergence": round(divergence, 4),
                "divergence_sigma": round(div_sigma, 2),
                "divergence_is_extreme": div_is_extreme,
                "source_probs": {k: round(v, 4) for k, v in source_probs.items()},
                "source_weights": {k: round(v, 4) for k, v in weights.items()},
                "dev_mu": round(dev_mu, 6),
                "dev_sigma": round(dev_sigma, 2),
                "target_hours": target_hours_ahead,
            },
        )

        logger.info(
            "多源信号: %s %s token=%s fused=%.3f market=%.3f dev=%.4f "
            "div=%.4f div_sigma=%.2f conf=%.3f",
            direction.value, event_type.value, market.token_id[:12],
            fused_prob, market_prob, market_dev,
            divergence, div_sigma, confidence,
        )

        return signal

    # ================================================================
    # 辅助方法
    # ================================================================

    @staticmethod
    def _calc_divergence(source_probs: Dict[str, float]) -> float:
        """计算源间分歧度: 所有两两源概率差的最大值。

        示例: {noaa: 0.8, caiyun: 0.2} -> max(|0.8-0.2|) = 0.6
        """
        if len(source_probs) < 2:
            return 0.0

        max_diff = 0.0
        for (s1, p1), (s2, p2) in combinations(source_probs.items(), 2):
            max_diff = max(max_diff, abs(p1 - p2))

        return max_diff

    @staticmethod
    def _calc_fused_prob(
        source_probs: Dict[str, float],
        weights: Dict[str, float],
    ) -> float:
        """计算加权融合概率。

        P_fused = sum(w_i * P_i), 权重归一化后和为 1.0。
        """
        total_weight = 0.0
        weighted_sum = 0.0

        for source, prob in source_probs.items():
            w = weights.get(source, 1.0 / len(source_probs))
            weighted_sum += w * prob
            total_weight += w

        if total_weight <= 0:
            # 退化为简单平均
            return sum(source_probs.values()) / len(source_probs)

        fused = weighted_sum / total_weight
        return max(0.0, min(1.0, fused))

    @staticmethod
    def _determine_direction(
        market_dev: float, mu: float
    ) -> SignalDirection:
        """根据融合偏差方向决定交易方向。

        market_dev > mu -> 融合概率高于历史 -> 市场低估 -> BUY_YES
        market_dev < mu -> 融合概率低于历史 -> 市场高估 -> BUY_NO
        """
        if market_dev > mu:
            return SignalDirection.BUY_YES
        else:
            return SignalDirection.BUY_NO

    def _calculate_confidence(
        self,
        dev_sigma: float,
        div_sigma: float,
        div_is_extreme: bool,
    ) -> float:
        """计算置信度。

        基础置信度来自融合偏差的 sigma 倍数,
        源间分歧异常时额外放大置信度。

        Args:
            dev_sigma: 融合偏差的 sigma 倍数
            div_sigma: 源间分歧的 sigma 倍数
            div_is_extreme: 源间分歧是否异常
        """
        # 基础置信度 (与预报偏差策略一致)
        if dev_sigma <= 1.0:
            return 0.0
        if dev_sigma >= 10:
            base = 0.99
        else:
            base = 1.0 - math.exp(2.0 - dev_sigma)

        # 分歧放大: 分歧异常时置信度提升
        if div_is_extreme and div_sigma > 0:
            # 归一化分歧 sigma 到 0~1
            div_boost_norm = min(div_sigma / 5.0, 1.0)
            boost = 1.0 + self.params.divergence_confidence_boost * div_boost_norm
            base = min(0.99, base * boost)

        return round(max(0.0, min(1.0, base)), 4)

    def _calculate_quantity(
        self,
        market_dev: float,
        dev_sigma: float,
        divergence: float,
    ) -> float:
        """计算建议交易数量。

        基于偏差幅度、置信度和分歧度。
        分歧越大, 说明信息不对称越严重, 可以适当加仓。
        """
        base = abs(market_dev) * 100.0 * min(dev_sigma / 2.0, 2.0)
        # 分歧加成: 分歧度 0~0.5 映射到 1.0~1.5 倍
        div_multiplier = 1.0 + min(divergence, 0.5)
        base *= div_multiplier
        return round(min(base, self.params.max_position_per_market), 2)

    def _record_data(
        self,
        div_record: DivergenceRecord,
        market_dev: float,
        event_type: EventType,
        location: str,
        token_id: str,
        source_probs: Dict[str, float],
        fused_prob: float,
        divergence: float,
        source_pair: str,
    ):
        """录入追踪器数据。"""
        # 录入分歧追踪器
        self.div_tracker.record(div_record)

        # 录入偏差追踪器 (用融合概率作为 model_prob)
        dev_record = DeviationRecord(
            timestamp=datetime.now(timezone.utc),
            model_prob=fused_prob,
            market_prob=fused_prob - market_dev,
            deviation=market_dev,
            event_type=event_type,
            location=location,
            token_id=token_id,
            model_source="fused",
        )
        self.dev_tracker.record(dev_record)
