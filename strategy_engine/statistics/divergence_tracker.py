"""源间分歧统计追踪器。

维护多数据源预报分歧的滚动窗口, 实时计算滑动均值 mu 和标准差 sigma,
用于判断当前源间分歧是否异常。

与 DeviationTracker 的区别:
  - DeviationTracker 追踪: model_prob - market_prob (模型 vs 市场偏差)
  - DivergenceTracker 追踪: source_A_prob - source_B_prob (源间分歧)

设计要点:
  - 复用 Welford 在线算法 (RunningStats)
  - 按 (event_type, location, source_pair) 分区
  - 支持持久化 (JSON 序列化/反序列化)
"""

import json
import logging
import math
import os
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Dict, List, Optional, Tuple

from ..models import DivergenceRecord, EventType
from .running_stats import RunningStats

logger = logging.getLogger(__name__)


class DivergenceTracker:
    """源间分歧统计追踪器。

    对每个 (event_type, location, source_pair) 三元组维护独立的
    RunningStats, 追踪源间分歧度 (divergence) 的历史分布。

    用法:
        tracker = DivergenceTracker(window_size=100)
        tracker.record(record)            # 录入分歧记录
        mu, sigma, n = tracker.stats(key)  # 查询统计量
        is_extreme, div, mu, sig_level = tracker.is_extreme(record, sigma=2.0)
    """

    def __init__(self, window_size: int = 100):
        self.window_size = window_size

        # 每个 key -> RunningStats (在线统计)
        self._stats: Dict[str, RunningStats] = defaultdict(RunningStats)

        # 每个 key -> 原始分歧值滑动窗口
        self._windows: Dict[str, List[float]] = defaultdict(list)

        # 每个 key -> 源权重 (基于历史准确率动态更新)
        # {"noaa": 0.6, "caiyun": 0.4}
        self._source_weights: Dict[str, Dict[str, float]] = defaultdict(dict)

        # 元数据
        self._record_count: Dict[str, int] = defaultdict(int)

    # ================================================================
    # 公共 API
    # ================================================================

    def record(self, record: DivergenceRecord):
        """录入一条源间分歧记录。"""
        key = self._make_key(
            record.event_type, record.location, record.source_pair
        )
        value = record.divergence

        self._stats[key].update(value)
        self._windows[key].append(value)
        self._record_count[key] += 1

        # 更新源权重 (基于本次融合概率的逆误差)
        self._update_weights(key, record)

        # 滑动窗口淘汰
        if len(self._windows[key]) > self.window_size:
            removed = self._windows[key].pop(0)
            self._stats[key] = self._recalc(key)

    def stats(
        self,
        event_type: EventType,
        location: str,
        source_pair: str,
    ) -> Tuple[float, float, int]:
        """获取指定 key 的统计量: (mu, sigma, n)。"""
        key = self._make_key(event_type, location, source_pair)
        s = self._stats[key]
        return s.mean, s.std, s.count

    def is_extreme(
        self,
        record: DivergenceRecord,
        sigma: float = 2.0,
    ) -> Tuple[bool, float, float, float]:
        """判断源间分歧是否异常。

        Returns:
            (is_extreme, divergence, mu, sigma_level)
            is_extreme: 是否超过 mu + sigma*std
            divergence: 实际分歧值
            mu: 历史均值
            sigma_level: 分歧是几倍 sigma
        """
        key = self._make_key(
            record.event_type, record.location, record.source_pair
        )
        s = self._stats[key]

        if not s.is_significant:
            return False, record.divergence, s.mean, 0.0

        if s.std < 1e-10:
            return record.divergence > 0.01, record.divergence, s.mean, 0.0

        sigma_level = abs(record.divergence - s.mean) / s.std
        is_extreme = sigma_level > sigma

        return is_extreme, record.divergence, s.mean, sigma_level

    def get_source_weights(
        self,
        event_type: EventType,
        location: str,
        source_pair: str,
    ) -> Dict[str, float]:
        """获取指定 key 的源权重 (归一化, 和为 1.0)。"""
        key = self._make_key(event_type, location, source_pair)
        weights = self._source_weights.get(key, {})

        if not weights:
            # 默认等权
            sources = source_pair.split("|")
            n = len(sources)
            return {s: 1.0 / n for s in sources}

        # 归一化
        total = sum(weights.values())
        if total <= 0:
            sources = source_pair.split("|")
            n = len(sources)
            return {s: 1.0 / n for s in sources}

        return {k: v / total for k, v in weights.items()}

    def set_source_weights(
        self,
        event_type: EventType,
        location: str,
        source_pair: str,
        weights: Dict[str, float],
    ):
        """手动设置源权重 (覆盖自动学习的权重)。"""
        key = self._make_key(event_type, location, source_pair)
        total = sum(weights.values())
        if total > 0:
            self._source_weights[key] = {k: v / total for k, v in weights.items()}

    def key_count(self) -> int:
        """追踪中的独立 key 数量。"""
        return len(self._record_count)

    def total_records(self) -> int:
        """总录入记录数。"""
        return sum(self._record_count.values())

    def list_keys(self) -> List[dict]:
        """列出所有追踪 key 及其统计摘要。"""
        result = []
        for key, s in self._stats.items():
            parts = key.split("|")
            # key 格式: event_type|location|source_a|source_b
            event_type = parts[0] if len(parts) > 0 else ""
            location = parts[1] if len(parts) > 1 else ""
            source_pair = "|".join(parts[2:]) if len(parts) > 2 else ""
            weights = self._source_weights.get(key, {})
            result.append({
                "event_type": event_type,
                "location": location,
                "source_pair": source_pair,
                "mu_divergence": round(s.mean, 6),
                "sigma_divergence": round(s.std, 6),
                "count": s.count,
                "source_weights": {k: round(v, 4) for k, v in weights.items()},
            })
        return result

    # ================================================================
    # 持久化
    # ================================================================

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
            "source_weights": dict(self._source_weights),
            "record_count": dict(self._record_count),
        }
        with open(path, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2, ensure_ascii=False)
        logger.info("分歧追踪器已保存: %s (%d keys)", path, len(self._stats))

    @classmethod
    def load(cls, path: str) -> Optional["DivergenceTracker"]:
        """从 JSON 文件加载追踪器。"""
        if not os.path.exists(path):
            logger.warning("分歧追踪器文件不存在: %s", path)
            return None

        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)

        tracker = cls(window_size=data.get("window_size", 100))

        for key, s_data in data.get("stats", {}).items():
            tracker._stats[key] = RunningStats.from_dict(s_data)

        for key, vals in data.get("windows", {}).items():
            tracker._windows[key] = vals

        for key, weights in data.get("source_weights", {}).items():
            tracker._source_weights[key] = weights

        for key, count in data.get("record_count", {}).items():
            tracker._record_count[key] = count

        logger.info("分歧追踪器已加载: %s (%d keys)", path, len(tracker._stats))
        return tracker

    # ================================================================
    # 内部
    # ================================================================

    @staticmethod
    def _make_key(
        event_type: EventType, location: str, source_pair: str
    ) -> str:
        """构造 key。"""
        return f"{event_type.value}|{location}|{source_pair}"

    @staticmethod
    def make_source_pair(sources: List[str]) -> str:
        """构造排序后的源对标识。"""
        return "|".join(sorted(sources))

    def _update_weights(self, key: str, record: DivergenceRecord):
        """基于融合概率的逆误差动态更新源权重。

        简化策略: 如果某源的预报概率更接近 0.5 (更保守),
        在分歧大的情况下给予更低权重; 更极端的预报给予更高权重。
        这是一个占位实现, 后续可接入历史准确率回测。
        """
        if not record.source_probs:
            return

        weights = self._source_weights[key]
        if not weights:
            n = len(record.source_probs)
            weights = {s: 1.0 / n for s in record.source_probs}

        # 简单加权: 源预报概率的绝对值越大 (越自信), 权重微增
        # 这是临时策略, 生产环境应接入历史准确率
        for source, prob in record.source_probs.items():
            confidence_signal = abs(prob - 0.5) * 2  # 0~1
            # 微调, 避免权重剧烈波动
            adjustment = 0.01 * confidence_signal
            weights[source] = weights.get(source, 0.5) * (1 + adjustment)

        # 归一化
        total = sum(weights.values())
        if total > 0:
            self._source_weights[key] = {
                k: v / total for k, v in weights.items()
            }

    def _recalc(self, key: str) -> RunningStats:
        """从原始窗口全量重算 RunningStats (淘汰旧值后使用)。"""
        new_stats = RunningStats()
        for val in self._windows[key]:
            new_stats.update(val)
        return new_stats
