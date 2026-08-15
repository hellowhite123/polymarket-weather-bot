"""市场发现模块 -- 数据模型。

定义 Polymarket 天气盘的统一数据模型 MarketInfo。
"""

from __future__ import annotations

from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from typing import Dict, List, Optional


def _utc_now() -> datetime:
    return datetime.now(timezone.utc)


def _iso_to_ts(iso_str: Optional[str]) -> Optional[float]:
    """ISO 8601 字符串 -> Unix 时间戳（秒），解析失败返回 None。"""
    if not iso_str:
        return None
    try:
        # 兼容带 Z 与带偏移量两种格式
        s = iso_str.replace("Z", "+00:00")
        return datetime.fromisoformat(s).timestamp()
    except (ValueError, TypeError):
        return None


def _parse_prices(raw) -> List[float]:
    """解析 outcomePrices（可能是 ["0.55","0.45"] 或 ["55","45"] 百分比）。"""
    if not raw:
        return []
    out = []
    for p in raw:
        try:
            v = float(p)
            if v > 1.0:  # 百分比格式
                v /= 100.0
            out.append(max(0.0, min(1.0, v)))
        except (ValueError, TypeError):
            out.append(0.5)
    return out


# 天气事件类型（与 strategy_engine.models.EventType 对齐）
WEATHER_EVENT_TYPES = ("rain", "temperature", "snow", "wind", "humidity")


def infer_event_type(question: str) -> str:
    """从市场问题文本推断天气事件类型，默认 rain。"""
    q = question.lower()
    if "snow" in q:
        return "snow"
    if "temperature" in q or "high" in q or "low" in q or "temp" in q:
        return "temperature"
    if "wind" in q or "mph" in q:
        return "wind"
    if "humidity" in q or "humid" in q:
        return "humidity"
    return "rain"


@dataclass
class MarketInfo:
    """一个 Polymarket 天气盘。

    字段对齐 Gamma API 的市场对象，同时兼容 mock 数据。
    """

    condition_id: str            # 唯一标识（Gamma API 的 conditionId / mock 生成）
    yes_token_id: str            # YES outcome token id（下单用）
    no_token_id: str             # NO outcome token id
    question: str                # 市场问题，如 "Will it rain in New York on 2026-08-11?"
    outcomes: List[str] = field(default_factory=lambda: ["Yes", "No"])
    outcome_prices: List[float] = field(default_factory=list)  # 与 outcomes 对应（gamma outcomePrices）
    end_date_utc: Optional[float] = None  # 截止时间戳（秒），未结束盘必须有
    start_date_utc: Optional[float] = None
    active: bool = True          # 是否可交易
    closed: bool = False         # 是否已结算
    category: str = "Weather"
    tags: List[str] = field(default_factory=list)
    volume: float = 0.0
    liquidity: float = 0.0
    # 派生字段
    location: str = ""           # 城市名（从 question 提取，尽力而为）
    event_type: str = "rain"     # rain/temperature/snow/wind/humidity

    def __post_init__(self):
        if not self.location:
            self.location = extract_location(self.question)
        self.event_type = infer_event_type(self.question)
        if not self.end_date_utc:
            raise ValueError(
                f"market {self.condition_id} 缺少 end_date_utc (未结束盘必须提供截止时间)"
            )

    @property
    def token_id(self) -> str:
        """兼容旧代码：默认返回 YES token。

        新交易代码禁止依赖此属性作为「整个市场」的唯一 outcome 标识，
        必须显式使用 yes_token_id / no_token_id。
        """
        return self.yes_token_id

    @property
    def is_expired(self) -> bool:
        """是否已过截止时间。"""
        return self.end_date_utc is not None and self.end_date_utc < _utc_now().timestamp()

    @property
    def yes_price(self) -> float:
        """YES token 当前价格（outcomePrices[0]，缺省 0.5）。"""
        if self.outcome_prices:
            try:
                return max(0.0, min(1.0, float(self.outcome_prices[0])))
            except (ValueError, TypeError):
                pass
        return 0.5

    def to_market_snapshot(self) -> dict:
        """转换为 strategy_engine.MarketSnapshot 字典。

        返回 dict 而非直接依赖 strategy_engine 类型，避免循环依赖；
        调用方用 MarketSnapshot(**d) 构造。

        R2-P0-01：同时输出 condition_id + yes_token_id/no_token_id，
        让策略层能按方向选择正确的 outcome token。
        """
        yes = self.yes_price
        return {
            "condition_id": self.condition_id,
            "token_id": self.token_id,  # 兼容旧代码（= yes_token_id）
            "yes_token_id": self.yes_token_id,
            "no_token_id": self.no_token_id,
            "yes_price": yes,
            "no_price": round(1.0 - yes, 6),
            "market_volume": self.volume,
            "spread": 0.0,
        }

    def to_dict(self) -> Dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: Dict) -> "MarketInfo":
        """从 to_dict() 生成的字典还原（兼容 registry 持久化记录）。"""
        fields = (
            "condition_id", "yes_token_id", "no_token_id", "question", "outcomes",
            "outcome_prices", "end_date_utc", "start_date_utc", "active",
            "closed", "category", "tags", "volume", "liquidity",
            "location", "event_type",
        )
        kwargs = {k: data[k] for k in fields if k in data}
        # 兼容旧数据：只有 token_id（无 yes/no 拆分）时映射到 yes_token_id
        if "yes_token_id" not in kwargs and "token_id" in data:
            kwargs["yes_token_id"] = data["token_id"]
            kwargs.setdefault("no_token_id", "")
        # R2-P0-02：生产交易路径必须 fail closed，
        # 缺完整 YES/NO token 映射时直接报错，不允许空 token 继续运行。
        if not kwargs.get("yes_token_id") or not kwargs.get("no_token_id"):
            raise ValueError(
                f"market {data.get('condition_id')} 缺少完整 YES/NO token mapping"
            )
        return cls(**kwargs)

    @classmethod
    def from_gamma(cls, raw: Dict) -> "MarketInfo":
        """从 Gamma API 市场对象构造。

        Gamma market 对象关键字段:
          id / question / conditionId / clobTokenIds / outcomes /
          endDate / startDate / active / closed / category / tags /
          volume / liquidity
        """
        token_ids = raw.get("clobTokenIds") or raw.get("tokenIds") or []
        if len(token_ids) < 2:
            raise ValueError(
                f"二元市场缺少 YES/NO token: "
                f"condition_id={raw.get('conditionId') or raw.get('id')}"
            )
        outcomes = [str(o) for o in (raw.get("outcomes") or ["Yes", "No"])]
        if len(outcomes) != 2:
            raise ValueError(f"目前只支持二元市场 outcomes={outcomes}")
        tags = [
            t.get("label") if isinstance(t, dict) else str(t)
            for t in (raw.get("tags") or [])
        ]
        return cls(
            condition_id=str(raw.get("conditionId") or raw.get("id") or ""),
            yes_token_id=str(token_ids[0]),
            no_token_id=str(token_ids[1]),
            question=str(raw.get("question") or ""),
            outcomes=outcomes,
            outcome_prices=_parse_prices(raw.get("outcomePrices")),
            end_date_utc=_iso_to_ts(raw.get("endDate")),
            start_date_utc=_iso_to_ts(raw.get("startDate")),
            active=bool(raw.get("active", True)),
            closed=bool(raw.get("closed", False)),
            category=str(raw.get("category") or "Weather"),
            tags=tags,
            volume=float(raw.get("volume") or 0.0),
            liquidity=float(raw.get("liquidity") or 0.0),
        )

    def __hash__(self):
        return hash(self.condition_id)

    def __eq__(self, other):
        return isinstance(other, MarketInfo) and self.condition_id == other.condition_id


def extract_location(question: str) -> str:
    """从问题文本粗略提取城市名（英文大写词 / 已知城市库匹配）。"""
    # 常见气象城市库（Polymarket 天气盘覆盖的主要城市）
    known = [
        "New York", "Chicago", "Miami", "Houston", "Denver", "Seattle",
        "Boston", "Phoenix", "Minneapolis", "Atlanta", "Dallas", "San Francisco",
        "Los Angeles", "Las Vegas", "Washington", "Philadelphia", "Portland",
        "Detroit", "Kansas City", "Nashville", "Memphis", "New Orleans",
        "Salt Lake City", "San Diego", "Austin",
    ]
    for city in known:
        if city.lower() in question.lower():
            return city
    # 兜底: 取第一个大写单词
    for word in question.split():
        if word and word[0].isupper() and len(word) > 2:
            return word.rstrip("?,.")
    return "Unknown"


def normalize_location(location: str) -> str:
    """归一化城市名用于跨模块匹配。

    去掉空格/逗号/短横并转小写: "New York" -> "newyork"。
    与 weather_data_ingestion 的文件命名（NewYork / New_York）对齐。
    """
    return "".join(c for c in location.lower() if c.isalnum())
