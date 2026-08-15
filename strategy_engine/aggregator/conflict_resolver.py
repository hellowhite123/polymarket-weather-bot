"""冲突仲裁器。

按 token_id 分组信号, 对每组做方向仲裁:

  - 同方向信号: 直接合并为该方向
  - 反方向信号 (BUY_YES vs BUY_NO): 计算双向加权置信度,
    差值 < conflict_threshold  -> 旗鼓相当, 相消不交易 (void)
    差值 >= conflict_threshold  -> 取置信度高的一侧

加权置信度 = sum(strategy_weight_i * confidence_i),
体现 "可靠策略 + 高置信度" 的信号应有更大话语权。
"""

import logging
from typing import Dict, List

from ..models import SignalDirection, TradingSignal
from .models import ConflictResolution

logger = logging.getLogger(__name__)


class ConflictResolver:
    """方向冲突仲裁器。"""

    def __init__(
        self,
        strategy_weights: Dict[str, float],
        conflict_threshold: float = 0.15,
    ):
        """
        Args:
            strategy_weights: 策略名 -> 权重, 未知策略默认权重 1.0
            conflict_threshold: 双向加权置信度差值低于此值则相消不交易
        """
        self.strategy_weights = strategy_weights
        self.conflict_threshold = conflict_threshold

    def _weight(self, strategy_name: str) -> float:
        return self.strategy_weights.get(strategy_name, 1.0)

    def resolve(
        self, signals: List[TradingSignal]
    ) -> Dict[str, ConflictResolution]:
        """对一批信号按 condition_id 分组并做方向仲裁（R2-P1-13）。

        Returns:
            {condition_id: ConflictResolution} -- 每个 condition 的仲裁结果
        """
        # 1. 按 condition_id 分组（fallback 到 token_id 兼容旧信号）
        groups: Dict[str, List[TradingSignal]] = {}
        for s in signals:
            key = s.condition_id or s.token_id
            groups.setdefault(key, []).append(s)

        results: Dict[str, ConflictResolution] = {}

        for key, group_signals in groups.items():
            resolution = self._resolve_group(key, group_signals)
            if resolution is not None:
                results[key] = resolution

        return results

    def _resolve_group(
        self, key: str, signals: List[TradingSignal]
    ) -> ConflictResolution:
        """对单个 condition 的信号做方向仲裁（YES vs NO 才是真冲突）。"""

        yes_signals = [
            s for s in signals if s.direction == SignalDirection.BUY_YES
        ]
        no_signals = [
            s for s in signals if s.direction == SignalDirection.BUY_NO
        ]

        if not yes_signals and not no_signals:
            return None

        # 只有一个方向 -- 无冲突
        if not no_signals:
            return ConflictResolution(
                token_id=yes_signals[0].token_id,
                direction=SignalDirection.BUY_YES,
                contributing_signals=yes_signals,
            )
        if not yes_signals:
            return ConflictResolution(
                token_id=no_signals[0].token_id,
                direction=SignalDirection.BUY_NO,
                contributing_signals=no_signals,
            )

        # 双向冲突 -- 加权置信度仲裁
        yes_score = sum(
            self._weight(s.strategy) * s.confidence for s in yes_signals
        )
        no_score = sum(
            self._weight(s.strategy) * s.confidence for s in no_signals
        )
        diff = abs(yes_score - no_score)

        if diff < self.conflict_threshold:
            logger.info(
                "condition=%s 双向冲突相消: YES_score=%.4f NO_score=%.4f "
                "diff=%.4f < 阈值 %.4f",
                key, yes_score, no_score, diff, self.conflict_threshold,
            )
            return ConflictResolution(
                token_id=key,
                direction=None,
                contributing_signals=signals,
                void_reason=(
                    f"双向加权置信度差 {diff:.4f} < 阈值 "
                    f"{self.conflict_threshold:.4f}, 相消不交易"
                ),
            )

        if yes_score > no_score:
            winner = SignalDirection.BUY_YES
            contributing = yes_signals
            loser_score = no_score
        else:
            winner = SignalDirection.BUY_NO
            contributing = no_signals
            loser_score = yes_score

        logger.info(
            "condition=%s 方向冲突仲裁: %s 胜 (score=%.4f vs %.4f, diff=%.4f)",
            key, winner.value,
            max(yes_score, no_score), loser_score, diff,
        )
        return ConflictResolution(
            token_id=contributing[0].token_id,
            direction=winner,
            contributing_signals=contributing,
        )
