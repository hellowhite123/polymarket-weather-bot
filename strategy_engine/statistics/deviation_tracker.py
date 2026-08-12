"""偏差统计追踪器。

维护历史偏差数据的滚动窗口，实时计算滑动均值 μ 和标准差 σ，
用于判断当前偏差是否异常（是否超过 μ±k*σ）。

设计要点:
  - 滚动窗口: 保持最近 N 条记录，自动淘汰过期数据
  - 在线计算: 使用 Welford 算法实现 O(1) 增量更新 μ/σ
  - 按 key 分区: 不同 (event_type, location, token_id) 独立追踪
  - 持久化: 支持 JSON 序列化，断线恢复后继续追踪
"""

import json
import logging
import math
import os
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

from ..models import DeviationRecord, EventType

logger = logging.getLogger(__name__)


class RunningStats:
    """Welford 在线统计算法 — O(1) 增量计算均值和方差。

    每新增一个数据点只需常数时间即可更新 μ 和 σ，
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
        """需要至少 10 个样本点才算有统计意义。"""
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


class DeviationTracker:
    """偏差统计追踪器。

    对每个 (event_type, location, token_id) 三元组维护独立的
    RunningStats + 滑动窗口原始数据。

    用法:
        tracker = DeviationTracker(window_size=100)
        tracker.record(record)           # 录入偏差记录
        mu, sigma = tracker.stats(key)   # 查询统计量
        is_extreme = tracker.is_extreme(record, sigma=2.0)  # 判断是否异常
    """

    def __init__(self, window_size: int = 100):
        self.window_size = window_size

        # 每个 key → RunningStats (在线统计)
        self._stats: dict[str, RunningStats] = defaultdict(RunningStats)

        # 每个 key → 原始偏差值滑动窗口 (用于判断 old data 的淘汰影响)
        self._windows: dict[str, list[float]] = defaultdict(list)

        # 元数据
        self._record_count: dict[str, int] = defaultdict(int)

    # ----- 公共 API -----

    def record(self, record: DeviationRecord):
        """录入一条偏差记录。"""
        key = self._make_key(record.event_type, record.location, record.token_id)
        value = record.deviation

        self._stats[key].update(value)
        self._windows[key].append(value)
        self._record_count[key] += 1

        # 滑动窗口淘汰
        if len(self._windows[key]) > self.window_size:
            removed = self._windows[key].pop(0)
            self._stats[key] = self._recalc(key)  # 全量重算（Welford 不支持删除）

    def stats(
        self, event_type: EventType, location: str, token_id: str
    ) -> tuple[float, float, int]:
        """获取指定 key 的统计量: (μ, σ, n)。"""
        key = self._make_key(event_type, location, token_id)
        s = self._stats[key]
        return s.mean, s.std, s.count

    def is_extreme(
        self,
        record: DeviationRecord,
        sigma: float = 2.0,
    ) -> tuple[bool, float, float, float]:
        """判断偏差是否异常。

        Returns:
            (is_extreme, deviation, mu, sigma_level)
            is_extreme: 是否超过 mu ± sigma*σ
            deviation: 实际偏差值
            mu: 历史均值
            sigma_level: 偏差是几倍 σ
        """
        key = self._make_key(record.event_type, record.location, record.token_id)
        s = self._stats[key]

        if not s.is_significant:
            # 样本不足，不判断
            return False, record.deviation, s.mean, 0.0

        deviation = record.deviation
        if s.std < 1e-10:
            # 标准差极小，数据几乎无波动
            return abs(deviation - s.mean) > 0.01, deviation, s.mean, 0.0

        sigma_level = abs(deviation - s.mean) / s.std
        is_extreme = sigma_level > sigma

        return is_extreme, deviation, s.mean, sigma_level

    def key_count(self) -> int:
        """追踪中的独立 key 数量。"""
        return len(self._record_count)

    def total_records(self) -> int:
        """总录入记录数。"""
        return sum(self._record_count.values())

    def list_keys(self) -> list[dict]:
        """列出所有追踪 key 及其统计摘要。"""
        result = []
        for key, s in self._stats.items():
            parts = key.split("|")
            result.append(
                {
                    "event_type": parts[0] if len(parts) > 0 else "",
                    "location": parts[1] if len(parts) > 1 else "",
                    "token_id": parts[2] if len(parts) > 2 else "",
                    "mu": round(s.mean, 6),
                    "sigma": round(s.std, 6),
                    "count": s.count,
                }
            )
        return result

    # ----- 持久化 -----

    def save(self, path: str):
        """保存追踪器状态到 JSON 文件。"""
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        data = {
            "window_size": self.window_size,
            "stats": {
                key: s.to_dict() for key, s in self._stats.items()
            },
            "windows": {
                key: vals for key, vals in self._windows.items()
            },
            "record_count": dict(self._record_count),
        }
        with open(path, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2, ensure_ascii=False)
        logger.info("追踪器已保存: %s (%d keys)", path, len(self._stats))

    @classmethod
    def load(cls, path: str) -> Optional["DeviationTracker"]:
        """从 JSON 文件加载追踪器。"""
        if not os.path.exists(path):
            logger.warning("追踪器文件不存在: %s", path)
            return None

        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)

        tracker = cls(window_size=data.get("window_size", 100))

        for key, s_data in data.get("stats", {}).items():
            tracker._stats[key] = RunningStats.from_dict(s_data)

        for key, vals in data.get("windows", {}).items():
            tracker._windows[key] = vals

        for key, count in data.get("record_count", {}).items():
            tracker._record_count[key] = count

        logger.info("追踪器已加载: %s (%d keys)", path, len(tracker._stats))
        return tracker

    # ----- 内部 -----

    @staticmethod
    def _make_key(event_type: EventType, location: str, token_id: str) -> str:
        """构造 key。"""
        return f"{event_type.value}|{location}|{token_id}"

    def _recalc(self, key: str) -> RunningStats:
        """从原始窗口全量重算 RunningStats（淘汰旧值后使用）。"""
        new_stats = RunningStats()
        for val in self._windows[key]:
            new_stats.update(val)
        return new_stats
