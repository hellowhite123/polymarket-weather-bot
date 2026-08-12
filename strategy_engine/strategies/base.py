"""策略抽象基类。

所有策略必须实现 evaluate() 方法，返回 TradingSignal 列表。
"""

from abc import ABC, abstractmethod
from typing import List

from ..models import TradingSignal


class BaseStrategy(ABC):
    """策略基类。"""

    def __init__(self, name: str):
        self.name = name
        self._signal_count: int = 0

    @abstractmethod
    async def evaluate(self) -> List[TradingSignal]:
        """执行策略评估，返回生成的交易信号。"""
        ...

    @property
    def signal_count(self) -> int:
        """已生成信号总数。"""
        return self._signal_count

    def _increment_counter(self):
        self._signal_count += 1
