"""置信度融合器。

将仲裁后的多策略信号融合为单一综合置信度和目标价格:

  综合置信度 = sum(w_i * conf_i) / sum(w_i)
  目标价格   = sum(w_i * conf_i * price_i) / sum(w_i * conf_i)

权重 w_i 来自策略可靠度 (历史准确率), 置信度 conf_i 来自策略自身评估。
高置信度信号对目标价格的影响更大 -- 这是对策略 "有多确定" 的二次加权。
"""

import logging
from typing import List, Tuple

from ..models import TradingSignal
from .models import ConflictResolution

logger = logging.getLogger(__name__)


class ConfidenceFuser:
    """多策略信号置信度融合器。"""

    def __init__(
        self,
        strategy_weights: dict,
        min_aggregated_confidence: float = 0.5,
    ):
        """
        Args:
            strategy_weights: 策略名 -> 权重, 未知策略默认 1.0
            min_aggregated_confidence: 综合置信度低于此值不下单
        """
        self.strategy_weights = strategy_weights
        self.min_confidence = min_aggregated_confidence

    def _weight(self, strategy_name: str) -> float:
        return self.strategy_weights.get(strategy_name, 1.0)

    def fuse(
        self, resolution: ConflictResolution
    ) -> Tuple[float, float, List[str], float]:
        """融合一组信号为 (综合置信度, 目标价格, 贡献策略列表, 市场价格)。

        若 resolution 为 void 或无信号, 返回全零。
        """
        if resolution.is_void:
            return 0.0, 0.0, [], 0.0

        signals: List[TradingSignal] = resolution.contributing_signals
        if not signals:
            return 0.0, 0.0, [], 0.0

        # 综合置信度: 加权平均
        total_weight = sum(self._weight(s.strategy) for s in signals)
        if total_weight <= 0:
            return 0.0, 0.0, [], 0.0

        weighted_conf = sum(
            self._weight(s.strategy) * s.confidence for s in signals
        )
        confidence = weighted_conf / total_weight

        # 目标价格: 用 conf 二次加权 (高置信度信号对价格影响更大)
        weight_conf_sum = weighted_conf
        if weight_conf_sum <= 0:
            target_price = signals[0].target_price
        else:
            target_price = sum(
                self._weight(s.strategy) * s.confidence * s.target_price
                for s in signals
            ) / weight_conf_sum

        # 市场价: 同一 token_id 应一致, 取第一个
        market_price = signals[0].market_price

        # 贡献策略 (去重保序)
        seen = set()
        strategies = []
        for s in signals:
            if s.strategy not in seen:
                seen.add(s.strategy)
                strategies.append(s.strategy)

        logger.debug(
            "token=%s 融合: conf=%.4f target=%.4f strategies=%s",
            resolution.token_id, confidence, target_price, strategies,
        )
        return confidence, target_price, strategies, market_price

    def is_confidence_sufficient(self, confidence: float) -> bool:
        """综合置信度是否达到下限。"""
        return confidence >= self.min_confidence
