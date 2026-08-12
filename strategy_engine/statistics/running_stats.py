"""公共统计基础设施。

RunningStats: Welford 在线算法, O(1) 增量计算均值和方差。
被 DeviationTracker 和 DivergenceTracker 共同复用。
"""

import math
from typing import Optional


class RunningStats:
    """Welford 在线统计算法 -- O(1) 增量计算均值和方差。

    每新增一个数据点只需常数时间即可更新 mu 和 sigma,
    不需要存储全部历史数据。
    """

    def __init__(self):
        self.count: int = 0
        self.mean: float = 0.0
        self.m2: float = 0.0  # 平方差累积

    def update(self, value: float):
        self.count += 1
        delta = value - self.mean
        self.mean += delta / self.count
        delta2 = value - self.mean
        self.m2 += delta * delta2

    @property
    def variance(self) -> float:
        if self.count < 2:
            return 0.0
        return self.m2 / (self.count - 1)  # 样本方差

    @property
    def std(self) -> float:
        return math.sqrt(self.variance)

    @property
    def is_significant(self) -> bool:
        """需要至少 N 个样本点才算有统计意义。"""
        return self.count >= 10

    def to_dict(self) -> dict:
        return {"count": self.count, "mean": self.mean, "m2": self.m2}

    @classmethod
    def from_dict(cls, data: dict) -> "RunningStats":
        s = cls()
        s.count = data["count"]
        s.mean = data["mean"]
        s.m2 = data["m2"]
        return s
