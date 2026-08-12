"""策略引擎数据模型。"""

from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone, timedelta
from enum import Enum
from typing import Dict, Optional
import uuid


# ============================================================
# 枚举
# ============================================================


class SignalDirection(str, Enum):
    """交易信号方向。"""
    BUY_YES = "BUY_YES"   # 买入 YES token（看涨）
    BUY_NO = "BUY_NO"     # 买入 NO token（看跌）


class EventType(str, Enum):
    """天气事件类型。"""
    RAIN = "rain"
    TEMPERATURE = "temperature"
    SNOW = "snow"
    WIND = "wind"
    HUMIDITY = "humidity"


# ============================================================
# 核心模型
# ============================================================


@dataclass
class DeviationRecord:
    """单条偏差记录 — DeviationTracker 的数据点。

    记录一次气象预报概率与市场隐含概率之间的偏差，
    用于计算历史 μ 和 σ。
    """

    timestamp: datetime
    model_prob: float              # 气象模型概率 (0.0~1.0)
    market_prob: float             # 市场隐含概率 (0.0~1.0)
    deviation: float               # model_prob - market_prob
    event_type: EventType
    location: str                  # 城市名或坐标
    token_id: str                  # Polymarket token ID
    model_source: str = ""         # 数据来源（noaa/caiyun/ecmwf）

    def to_dict(self) -> dict:
        d = asdict(self)
        d["timestamp"] = self.timestamp.isoformat()
        d["event_type"] = self.event_type.value
        return d

    @classmethod
    def from_dict(cls, data: dict) -> "DeviationRecord":
        return cls(
            timestamp=datetime.fromisoformat(data["timestamp"]),
            model_prob=data["model_prob"],
            market_prob=data["market_prob"],
            deviation=data["deviation"],
            event_type=EventType(data["event_type"]),
            location=data["location"],
            token_id=data["token_id"],
            model_source=data.get("model_source", ""),
        )


@dataclass
class MarketSnapshot:
    """市场快照 — 策略输入的行情数据。"""

    token_id: str
    yes_price: float               # YES token 当前价格（二分市场：YES+NO≈1.0）
    no_price: float                # NO token 当前价格
    market_volume: float = 0.0     # 24h 成交量
    spread: float = 0.0            # 买卖价差
    timestamp: datetime = field(default_factory=lambda: datetime.now(timezone.utc))

    @property
    def implied_probability(self) -> float:
        """市场隐含概率 = YES token 价格。

        在二分市场中，YES 价格直接反映市场对该事件发生的概率估计。
        """
        return max(0.0, min(1.0, self.yes_price))

    def to_dict(self) -> dict:
        d = asdict(self)
        d["timestamp"] = self.timestamp.isoformat()
        return d


@dataclass
class TradingSignal:
    """交易信号 — 策略引擎最终输出。

    包含完整的方向、定价、风控信息，下游可直接消费。
    """

    signal_id: str = field(default_factory=lambda: uuid.uuid4().hex[:12])
    strategy: str = ""                 # 策略名称
    direction: SignalDirection = SignalDirection.BUY_YES
    token_id: str = ""
    event_type: EventType = EventType.RAIN
    location: str = ""

    # 定价
    target_price: float = 0.0          # 策略认为的合理价格
    market_price: float = 0.0          # 当前市场价格
    quantity: float = 0.0              # 建议交易数量

    # 置信度
    confidence: float = 0.0            # 0.0~1.0
    deviation: float = 0.0             # 实际偏差值
    sigma_level: float = 0.0           # 偏差是几倍 σ

    # 生命周期
    generated_at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    expires_at: Optional[datetime] = None

    # 扩展
    metadata: dict = field(default_factory=dict)

    def is_expired(self) -> bool:
        """检查信号是否过期。"""
        if self.expires_at is None:
            return False
        return datetime.now(timezone.utc) > self.expires_at

    def to_dict(self) -> dict:
        d = asdict(self)
        d["direction"] = self.direction.value
        d["event_type"] = self.event_type.value
        d["generated_at"] = self.generated_at.isoformat()
        d["expires_at"] = (
            self.expires_at.isoformat() if self.expires_at else None
        )
        return d

    @classmethod
    def from_dict(cls, data: dict) -> "TradingSignal":
        return cls(
            signal_id=data.get("signal_id", ""),
            strategy=data.get("strategy", ""),
            direction=SignalDirection(data["direction"]),
            token_id=data.get("token_id", ""),
            event_type=EventType(data.get("event_type", "rain")),
            location=data.get("location", ""),
            target_price=data.get("target_price", 0.0),
            market_price=data.get("market_price", 0.0),
            quantity=data.get("quantity", 0.0),
            confidence=data.get("confidence", 0.0),
            deviation=data.get("deviation", 0.0),
            sigma_level=data.get("sigma_level", 0.0),
            generated_at=datetime.fromisoformat(data["generated_at"]),
            expires_at=(
                datetime.fromisoformat(data["expires_at"])
                if data.get("expires_at")
                else None
            ),
            metadata=data.get("metadata", {}),
        )


# ============================================================
# 多源验证策略专用模型
# ============================================================


@dataclass
class DivergenceRecord:
    """单条源间分歧记录 -- DivergenceTracker 的数据点。

    记录多个气象数据源对同一事件的预报分歧，
    用于计算历史分歧的 mu 和 sigma。
    """

    timestamp: datetime
    source_probs: Dict[str, float]      # {"noaa": 0.8, "caiyun": 0.2}
    fused_prob: float                    # 加权融合概率
    divergence: float                    # 源间分歧度 (max pairwise |diff|)
    event_type: EventType
    location: str
    token_id: str
    source_pair: str = ""                # 排序后的源对标识 "caiyun|noaa"

    def to_dict(self) -> dict:
        d = asdict(self)
        d["timestamp"] = self.timestamp.isoformat()
        d["event_type"] = self.event_type.value
        return d

    @classmethod
    def from_dict(cls, data: dict) -> "DivergenceRecord":
        return cls(
            timestamp=datetime.fromisoformat(data["timestamp"]),
            source_probs=data["source_probs"],
            fused_prob=data["fused_prob"],
            divergence=data["divergence"],
            event_type=EventType(data["event_type"]),
            location=data["location"],
            token_id=data["token_id"],
            source_pair=data.get("source_pair", ""),
        )
