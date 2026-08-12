"""信号聚合器子包。

四步流水线:
  1. ConflictResolver  -- 方向冲突仲裁
  2. ConfidenceFuser    -- 置信度加权融合
  3. RiskGate           -- 风控闸门 (预检查 + 后检查)
  4. PositionSizer      -- 仓位计算 (Kelly / 固定比例)
     OrderSizeLimits    -- 下单金额上限 (总资产分档)
"""

from .aggregator import AggregatorParams, SignalAggregator
from .confidence_fuser import ConfidenceFuser
from .conflict_resolver import ConflictResolver
from .models import (
    AggregatedOrder,
    ConflictResolution,
    OrderStatus,
    RiskCheckResult,
    RiskState,
)
from .order_size_limits import OrderSizeLimits
from .position_sizer import PositionSizer
from .risk_gate import RiskGate

__all__ = [
    "AggregatorParams",
    "SignalAggregator",
    "ConflictResolver",
    "ConfidenceFuser",
    "RiskGate",
    "PositionSizer",
    "OrderSizeLimits",
    "AggregatedOrder",
    "ConflictResolution",
    "OrderStatus",
    "RiskCheckResult",
    "RiskState",
]
