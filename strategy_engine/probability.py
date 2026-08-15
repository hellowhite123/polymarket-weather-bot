"""事件概率工具（P0-07 修复）。

把天气原始量（温度 / 风速 / 湿度 / 降水）转换为**事件概率** P(event)，
而不是把原始值直接混入概率策略。

核心：
  - `normal_cdf` / `probability_above` / `probability_below`：
    基于正态分布把「均值 + 标准差 + 阈值」转为概率。
  - `EventProbability`：统一的事件概率输出，携带不确定性、时间与来源，
    避免上层把原始值误当概率。

用法示例（市场问 "Will high temperature exceed 90°F?"）：
    p = probability_above(mean=32.2, std=1.5, threshold=32.2)  # P(T > 90°F)
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from math import erf, sqrt
from typing import Optional


def normal_cdf(x: float) -> float:
    """标准正态分布的累积分布函数 Φ(x)。"""
    return 0.5 * (1.0 + erf(x / sqrt(2.0)))


def probability_above(mean: float, std: float, threshold: float) -> float:
    """P(X > threshold)，X ~ N(mean, std)。

    Args:
        mean: 预测均值（如预报温度 °C）
        std: 预测标准差（不确定性）
        threshold: 事件阈值

    Returns:
        概率 0.0 ~ 1.0
    """
    if std <= 0:
        return float(mean > threshold)
    z = (threshold - mean) / std
    return 1.0 - normal_cdf(z)


def probability_below(mean: float, std: float, threshold: float) -> float:
    """P(X < threshold)，X ~ N(mean, std)。"""
    if std <= 0:
        return float(mean < threshold)
    z = (threshold - mean) / std
    return normal_cdf(z)


@dataclass
class EventProbability:
    """统一的事件概率输出。

    天气原始量必须经转换后以本结构进入策略层，禁止直接传原始值。
    """

    probability: float                     # P(event) ∈ [0, 1]
    uncertainty: float = 0.0               # 概率不确定性（std of probability）
    forecast_time: Optional[datetime] = None   # 预报发布时间
    valid_time: Optional[datetime] = None      # 预报有效时间（事件发生时刻）
    source: str = ""                       # 数据来源（noaa/ecmwf/caiyun/jma）
    calibration_version: str = ""          # 校准版本号

    def clamped(self) -> "EventProbability":
        """返回概率裁剪到 [0,1] 的副本。"""
        return EventProbability(
            probability=max(0.0, min(1.0, self.probability)),
            uncertainty=self.uncertainty,
            forecast_time=self.forecast_time,
            valid_time=self.valid_time,
            source=self.source,
            calibration_version=self.calibration_version,
        )

    def to_dict(self) -> dict:
        return {
            "probability": self.probability,
            "uncertainty": self.uncertainty,
            "forecast_time": self.forecast_time.isoformat() if self.forecast_time else None,
            "valid_time": self.valid_time.isoformat() if self.valid_time else None,
            "source": self.source,
            "calibration_version": self.calibration_version,
        }
