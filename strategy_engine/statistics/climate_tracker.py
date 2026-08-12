"""历史气候统计追踪器。

按 (location, event_type, month) 三元组分区存储历史气候概率的
在线统计量（Welford 均值/方差），用于历史统计策略的季节性基线。

设计要点:
  - 按 (location, event_type, month) 分区: 不同地点、不同事件、不同月份独立追踪
  - 在线计算: Welford 算法 O(1) 增量更新
  - 批量导入: 支持从历史观测 JSON 文件批量导入
  - 持久化: JSON 序列化/反序列化
  - 优雅降级: 无历史数据时返回 None，策略层自行处理
"""

import json
import logging
import math
import os
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Dict, List, Optional, Tuple

from ..models import EventType

logger = logging.getLogger(__name__)


class ClimateStats:
    """单个 (location, event_type, month) 的气候统计。

    使用 Welford 算法在线计算均值和方差。
    """

    def __init__(self):
        self.count: int = 0
        self.mean: float = 0.0
        self.m2: float = 0.0  # 平方差累积
        # 原始值缓存（用于序列化恢复和离线重算）
        self._values: List[float] = []

    def update(self, value: float):
        """新增一个历史观测概率。"""
        self.count += 1
        delta = value - self.mean
        self.mean += delta / self.count
        delta2 = value - self.mean
        self.m2 += delta * delta2
        self._values.append(value)

    @property
    def variance(self) -> float:
        if self.count < 2:
            return 0.0
        return self.m2 / (self.count - 1)

    @property
    def std(self) -> float:
        return math.sqrt(self.variance)

    @property
    def is_significant(self) -> bool:
        """需要至少 5 个样本才算有统计意义（历史数据量通常较少）。"""
        return self.count >= 5

    def to_dict(self) -> dict:
        return {
            "count": self.count,
            "mean": self.mean,
            "m2": self.m2,
            "values": self._values,
        }

    @classmethod
    def from_dict(cls, data: dict) -> "ClimateStats":
        s = cls()
        s.count = data["count"]
        s.mean = data["mean"]
        s.m2 = data["m2"]
        s._values = data.get("values", [])
        return s


class ClimateTracker:
    """历史气候统计追踪器。

    对每个 (location, event_type, month) 三元组维护独立的 ClimateStats。

    用法:
        tracker = ClimateTracker()
        tracker.add_observation("NewYork", EventType.RAIN, 7, 0.35)
        stats = tracker.get_stats("NewYork", EventType.RAIN, 7)
        # -> ClimateStats(mean=0.35, std=0.0, count=1)
    """

    def __init__(self):
        # key: "location|event_type|month" -> ClimateStats
        self._stats: Dict[str, ClimateStats] = defaultdict(ClimateStats)

    # ----- 公共 API -----

    def add_observation(
        self,
        location: str,
        event_type: EventType,
        month: int,
        probability: float,
    ):
        """录入一条历史气候观测。

        Args:
            location: 地点标识
            event_type: 事件类型
            month: 月份 (1~12)
            probability: 该次观测的事件概率 (0.0~1.0)
        """
        key = self._make_key(location, event_type, month)
        self._stats[key].update(probability)

    def add_observations_batch(
        self,
        location: str,
        event_type: EventType,
        observations: List[Tuple[int, float]],
    ):
        """批量录入同一 (location, event_type) 的多条观测。

        Args:
            location: 地点标识
            event_type: 事件类型
            observations: [(month, probability), ...] 列表
        """
        for month, prob in observations:
            self.add_observation(location, event_type, month, prob)

    def get_stats(
        self,
        location: str,
        event_type: EventType,
        month: int,
    ) -> Optional[ClimateStats]:
        """获取指定 key 的气候统计。

        Returns:
            ClimateStats 或 None（无数据）
        """
        key = self._make_key(location, event_type, month)
        s = self._stats.get(key)
        if s is None or s.count == 0:
            return None
        return s

    def get_climatology(
        self,
        location: str,
        event_type: EventType,
        month: int,
    ) -> Optional[Tuple[float, float, int]]:
        """获取气候学概率 (mean, std, count)。

        Returns:
            (mean, std, count) 或 None（无数据）
        """
        s = self.get_stats(location, event_type, month)
        if s is None:
            return None
        return s.mean, s.std, s.count

    def get_smoothed_climatology(
        self,
        location: str,
        event_type: EventType,
        month: int,
        window: int = 1,
    ) -> Optional[Tuple[float, float, int]]:
        """获取平滑后的气候学概率（用前后 window 个月的数据加权平均）。

        月份是循环的：month=1 的前一个月是 12，month=12 的后一个月是 1。
        权重随月份距离线性衰减。

        Args:
            location: 地点
            event_type: 事件类型
            month: 目标月份 (1~12)
            window: 平滑窗口（前后各 window 个月）

        Returns:
            (weighted_mean, weighted_std, total_count) 或 None
        """
        all_values: List[float] = []
        all_weights: List[float] = []

        for offset in range(-window, window + 1):
            m = ((month - 1 + offset) % 12) + 1  # 循环月份
            s = self.get_stats(location, event_type, m)
            if s is None or s.count == 0:
                continue
            weight = 1.0 / (1.0 + abs(offset))  # 距离越远权重越低
            all_values.append(s.mean)
            all_weights.append(weight)

        if not all_values:
            return None

        total_weight = sum(all_weights)
        weighted_mean = sum(v * w for v, w in zip(all_values, all_weights)) / total_weight

        # 加权标准差
        if len(all_values) > 1:
            weighted_var = sum(
                w * (v - weighted_mean) ** 2
                for v, w in zip(all_values, all_weights)
            ) / total_weight
            weighted_std = math.sqrt(weighted_var)
        else:
            weighted_std = 0.0

        total_count = sum(
            self.get_stats(location, event_type, m).count
            for m in range(1, 13)
            if self.get_stats(location, event_type, m) is not None
        )

        return weighted_mean, weighted_std, total_count

    def key_count(self) -> int:
        """追踪中的独立 key 数量。"""
        return len(self._stats)

    def total_observations(self) -> int:
        """总观测数。"""
        return sum(s.count for s in self._stats.values())

    def list_keys(self) -> List[dict]:
        """列出所有追踪 key 及其统计摘要。"""
        result = []
        for key, s in self._stats.items():
            parts = key.split("|")
            result.append({
                "location": parts[0] if len(parts) > 0 else "",
                "event_type": parts[1] if len(parts) > 1 else "",
                "month": int(parts[2]) if len(parts) > 2 else 0,
                "mean": round(s.mean, 6),
                "std": round(s.std, 6),
                "count": s.count,
            })
        return result

    # ----- 持久化 -----

    def save(self, path: str):
        """保存到 JSON 文件。"""
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        data = {
            "stats": {
                key: s.to_dict() for key, s in self._stats.items()
            },
        }
        with open(path, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2, ensure_ascii=False)
        logger.info(
            "气候追踪器已保存: %s (%d keys, %d observations)",
            path, len(self._stats), self.total_observations(),
        )

    @classmethod
    def load(cls, path: str) -> Optional["ClimateTracker"]:
        """从 JSON 文件加载。"""
        if not os.path.exists(path):
            logger.warning("气候追踪器文件不存在: %s", path)
            return None

        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)

        tracker = cls()
        for key, s_data in data.get("stats", {}).items():
            tracker._stats[key] = ClimateStats.from_dict(s_data)

        logger.info(
            "气候追踪器已加载: %s (%d keys, %d observations)",
            path, len(tracker._stats), tracker.total_observations(),
        )
        return tracker

    # ----- 历史数据导入 -----

    @classmethod
    def from_historical_json(cls, filepath: str) -> "ClimateTracker":
        """从历史观测 JSON 文件构建追踪器。

        JSON 格式:
        [
            {
                "location": "NewYork",
                "event_type": "rain",
                "month": 7,
                "probability": 0.35
            },
            ...
        ]

        或者也可以是带 timestamp 的观测记录，自动提取月份:
        [
            {
                "location": "NewYork",
                "event_type": "rain",
                "timestamp": "2023-07-15T12:00:00Z",
                "probability": 0.35
            },
            ...
        ]
        """
        tracker = cls()

        if not os.path.exists(filepath):
            logger.warning("历史数据文件不存在: %s", filepath)
            return tracker

        with open(filepath, "r", encoding="utf-8") as f:
            data = json.load(f)

        if not isinstance(data, list):
            data = [data]

        count = 0
        for item in data:
            location = item.get("location", "unknown")
            event_str = item.get("event_type", "rain")
            try:
                event_type = EventType(event_str)
            except ValueError:
                continue

            # 优先用 month 字段，否则从 timestamp 提取
            month = item.get("month")
            if month is None:
                ts = item.get("timestamp", "")
                try:
                    dt = datetime.fromisoformat(ts.replace("Z", "+00:00"))
                    month = dt.month
                except (ValueError, AttributeError):
                    continue

            prob = item.get("probability")
            if prob is None:
                continue

            tracker.add_observation(location, event_type, month, float(prob))
            count += 1

        logger.info(
            "从 %s 导入 %d 条历史观测 (%d keys)",
            filepath, count, tracker.key_count(),
        )
        return tracker

    # ----- 内部 -----

    @staticmethod
    def _make_key(location: str, event_type: EventType, month: int) -> str:
        """构造 key。"""
        return f"{location}|{event_type.value}|{month}"
