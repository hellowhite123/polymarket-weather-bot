"""Mock 市场数据提供者。

在无真实 Polymarket API 的环境中，提供模拟的市场数据用于策略测试。
支持静态模式（固定价格）和动态模式（基于预报概率加噪声生成）。
"""

import logging
import random
from datetime import datetime, timezone
from typing import Optional

from ..models import MarketSnapshot

logger = logging.getLogger(__name__)


class MockMarketProvider:
    """模拟市场数据提供者。

    设计原理:
      对于一个天气事件（如"纽约明天会下雨吗"），市场 YES 价格
      近似反映市场对该事件的概率判断。在测试环境中，我们基于
      气象预报概率 + 可控噪声来模拟市场价格。

    用法:
        provider = MockMarketProvider(base_noise=0.05, bias=-0.03)
        snapshot = provider.generate(token_id, model_prob)
    """

    def __init__(
        self,
        base_noise: float = 0.05,
        bias: float = 0.0,
        seed: Optional[int] = None,
    ):
        """
        Args:
            base_noise: 基础噪声标准差（高斯噪声的 σ）
            bias: 系统性偏差（正数=市场高估，负数=市场低估）
            seed: 随机种子（用于可复现测试）
        """
        self.base_noise = base_noise
        self.bias = bias
        self._rng = random.Random(seed)

    def generate(
        self,
        token_id: str,
        model_prob: float,
        volume: float = 5000.0,
    ) -> MarketSnapshot:
        """根据模型概率生成模拟市场快照。

        市场隐含概率 = clamp(model_prob + bias + noise, 0.01, 0.99)

        Args:
            token_id: Polymarket token ID
            model_prob: 气象模型概率
            volume: 模拟 24h 成交量

        Returns:
            MarketSnapshot
        """
        # 噪声 = N(0, base_noise²)
        noise = self._rng.gauss(0.0, self.base_noise)

        # 市场隐含概率 = 模型概率 + 系统性偏差 + 随机噪声
        implied = max(0.01, min(0.99, model_prob + self.bias + noise))

        spread = self._rng.uniform(0.001, 0.01)

        return MarketSnapshot(
            token_id=token_id,
            yes_price=round(implied, 4),
            no_price=round(1.0 - implied - spread, 4),
            market_volume=volume,
            spread=round(spread, 4),
            timestamp=datetime.now(timezone.utc),
        )

    def generate_batch(
        self,
        token_probs: list[tuple[str, float]],
    ) -> list[MarketSnapshot]:
        """批量生成市场快照。"""
        return [
            self.generate(token_id, prob)
            for token_id, prob in token_probs
        ]
