"""历史数据加载器。

支持两种数据源:
  1. 从 JSON 文件加载真实历史数据
  2. 生成模拟历史数据 (用于无真实数据时的框架验证)

数据格式:
  一个 JSON 文件包含一个 HistoricalEvent 的序列化形式,
  或者一个包含多个事件的列表。
"""

import json
import logging
import os
import random
from datetime import datetime, timezone, timedelta
from pathlib import Path
from typing import Dict, List, Optional, Tuple

from .models import HistoricalEvent, EventOutcome

logger = logging.getLogger(__name__)


class DataLoader:
    """历史数据加载器。"""

    def __init__(self, seed: int = 42):
        self._rng = random.Random(seed)

    # ================================================================
    # 从文件加载
    # ================================================================

    def load_events(self, filepaths: List[str]) -> List[HistoricalEvent]:
        """从 JSON 文件列表加载历史事件。

        每个文件可以是单个事件或事件列表。
        """
        events = []
        for fpath in filepaths:
            if not os.path.exists(fpath):
                logger.warning("文件不存在: %s", fpath)
                continue
            try:
                with open(fpath, "r", encoding="utf-8") as f:
                    data = json.load(f)
                if isinstance(data, list):
                    for item in data:
                        events.append(self._parse_event(item))
                else:
                    events.append(self._parse_event(data))
            except Exception as e:
                logger.error("加载事件失败 %s: %s", fpath, e)

        logger.info("从 %d 个文件加载了 %d 个历史事件", len(filepaths), len(events))
        return events

    def _parse_event(self, data: dict) -> HistoricalEvent:
        """解析单个事件 JSON。"""
        return HistoricalEvent(
            token_id=data["token_id"],
            location=data["location"],
            event_type=data["event_type"],
            deadline=datetime.fromisoformat(data["deadline"]),
            outcome=EventOutcome(data.get("outcome", "pending")),
            forecast_files=data.get("forecast_files", []),
            market_history=data.get("market_history", []),
            actual_data=data.get("actual_data", {}),
        )

    # ================================================================
    # 生成模拟历史数据
    # ================================================================

    def generate_synthetic_events(
        self,
        n_events: int = 50,
        locations: Optional[List[str]] = None,
        event_types: Optional[List[str]] = None,
        start_date: Optional[datetime] = None,
        days_span: int = 90,
        base_prob: float = 0.4,
    ) -> List[HistoricalEvent]:
        """生成模拟历史天气事件。

        模拟逻辑:
          - 在 days_span 天内生成 n_events 个事件
          - 每个事件有随机位置、事件类型、截止时间
          - 为每个事件生成市场快照时间序列
          - 根据概率随机决定真实结果

        Args:
            n_events: 事件数量
            locations: 位置列表 (默认 NYC/LA/Chicago)
            event_types: 事件类型列表 (默认 rain/temperature)
            start_date: 起始日期 (默认 90 天前)
            days_span: 时间跨度 (天)
            base_prob: 基础事件发生概率
        """
        if locations is None:
            locations = ["NewYork", "LosAngeles", "Chicago", "Houston", "Miami"]
        if event_types is None:
            event_types = ["rain", "temperature"]
        if start_date is None:
            start_date = datetime.now(timezone.utc) - timedelta(days=days_span)

        events = []
        for i in range(n_events):
            loc = self._rng.choice(locations)
            etype = self._rng.choice(event_types)

            # 随机截止时间 (在 start_date + days_span 内)
            day_offset = self._rng.uniform(1, days_span)
            deadline = start_date + timedelta(days=day_offset)

            # 事件发生概率 (加入位置/季节性扰动)
            prob = max(0.1, min(0.9, base_prob + self._rng.gauss(0, 0.15)))

            # 真实结果
            outcome = EventOutcome.YES if self._rng.random() < prob else EventOutcome.NO

            token_id = f"bt_{loc}_{etype}_{deadline.strftime('%Y%m%d')}"

            # 生成市场快照时间序列 (截止前 48h~1h, 每 6h 一个快照)
            market_history = self._generate_market_series(
                deadline, prob, base_bias=0.02, noise=0.05
            )

            event = HistoricalEvent(
                token_id=token_id,
                location=loc,
                event_type=etype,
                deadline=deadline,
                outcome=outcome,
                forecast_files=[],  # 回测时用 Mock 预报
                market_history=market_history,
                actual_data={
                    "prob": prob,
                    "temperature_c": round(self._rng.gauss(20, 8), 1),
                    "precip_mm": round(max(0, self._rng.gauss(prob * 10, 3)), 1)
                    if outcome == EventOutcome.YES else 0.0,
                },
            )
            events.append(event)

        # 按截止时间排序
        events.sort(key=lambda e: e.deadline)
        logger.info(
            "生成了 %d 个模拟事件 (YES=%d, NO=%d, 跨度=%d天)",
            len(events),
            sum(1 for e in events if e.outcome == EventOutcome.YES),
            sum(1 for e in events if e.outcome == EventOutcome.NO),
            days_span,
        )
        return events

    def _generate_market_series(
        self,
        deadline: datetime,
        true_prob: float,
        base_bias: float = 0.02,
        noise: float = 0.05,
        n_points: int = 8,
        step_hours: float = 6.0,
    ) -> List[dict]:
        """为单个事件生成市场快照时间序列。

        模拟市场价格向真实概率收敛的过程:
          - 截止前 48h: 市场价格 = true_prob + bias + 大噪声
          - 临近截止: 价格逐渐收敛到 true_prob
        """
        series = []
        for i in range(n_points):
            # 从截止前 48h 开始, 每 step_hours 一个快照
            ts = deadline - timedelta(hours=48 - i * step_hours)
            if ts >= deadline:
                break

            hours_to_deadline = (deadline - ts).total_seconds() / 3600

            # 收敛因子: 临近截止时偏差和噪声减小
            convergence = max(0.3, min(1.0, hours_to_deadline / 48.0))

            # 市场隐含概率 = 真实概率 + 偏差*收敛 + 噪声*收敛
            implied = true_prob + base_bias * convergence + self._rng.gauss(0, noise * convergence)
            implied = max(0.01, min(0.99, implied))

            spread = self._rng.uniform(0.002, 0.01)
            volume = self._rng.uniform(2000, 10000)

            series.append({
                "timestamp": ts.isoformat(),
                "yes_price": round(implied, 4),
                "no_price": round(1.0 - implied - spread, 4),
                "volume": round(volume, 2),
                "spread": round(spread, 4),
                "hours_to_deadline": round(hours_to_deadline, 1),
            })

        return series

    # ================================================================
    # 保存/加载事件到 JSON
    # ================================================================

    def save_events(
        self,
        events: List[HistoricalEvent],
        filepath: str,
    ) -> None:
        """保存事件列表到 JSON 文件。"""
        os.makedirs(os.path.dirname(filepath), exist_ok=True)
        data = [e.to_dict() for e in events]
        with open(filepath, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2, ensure_ascii=False)
        logger.info("保存了 %d 个事件到 %s", len(events), filepath)

    def load_events_from_file(self, filepath: str) -> List[HistoricalEvent]:
        """从单个 JSON 文件加载事件。"""
        if not os.path.exists(filepath):
            logger.warning("文件不存在: %s", filepath)
            return []
        with open(filepath, "r", encoding="utf-8") as f:
            data = json.load(f)
        if isinstance(data, list):
            return [self._parse_event(item) for item in data]
        return [self._parse_event(data)]
