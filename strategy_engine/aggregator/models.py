"""信号聚合器数据模型。

定义聚合器与下游 Rust 执行层之间的接口契约:

  - AggregatedOrder:   聚合后的最终交易指令 (Rust 直接消费)
  - RiskState:         风控状态 (由外部 Rust/链上层更新, 聚合器只读)
  - RiskCheckResult:   单项风控检查结果
  - ConflictResolution: 冲突仲裁中间结果 (内部使用)

设计原则:
  - 聚合器是无状态函数式的: 输入信号 + 风控状态 -> 输出指令
  - RiskState 由调用方维护, 聚合器不修改它, 只读取判断
  - blocked=True 的指令仍会输出 (带 block_reason), 方便日志审计
"""

from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from enum import Enum
from typing import Dict, List, Optional

from ..models import EventType, SignalDirection


class OrderStatus(str, Enum):
    """聚合指令状态。"""

    OPEN = "open"            # 通过风控, 可下单
    BLOCKED = "blocked"      # 被风控拦截
    CONFLICT_VOID = "void"  # 方向冲突相消, 不交易
    NO_SIGNAL = "no_signal"  # 无有效信号输入


@dataclass
class RiskCheckResult:
    """单项风控检查结果。"""

    rule: str           # 规则名: position_limit / settlement_window / daily_loss / global_exposure
    passed: bool
    reason: str = ""    # 不通过时的原因

    def to_dict(self) -> dict:
        return asdict(self)


@dataclass
class ConflictResolution:
    """冲突仲裁中间结果 (内部使用)。

    对同一 token_id 的多策略信号做方向仲裁后,
    确定最终方向和参与贡献的信号子集。
    """

    token_id: str
    direction: Optional[SignalDirection]    # None = 相消不交易
    contributing_signals: List = field(default_factory=list)  # List[TradingSignal]
    void_reason: str = ""                   # direction=None 时的原因

    @property
    def is_void(self) -> bool:
        return self.direction is None


@dataclass
class AggregatedOrder:
    """聚合后的最终交易指令。

    这是 Python 策略大脑交给 Rust 执行手脚的唯一数据载体,
    包含 Rust 下单所需的全部信息:
      - 哪个市场 (token_id)
      - 什么方向 (direction)
      - 目标价 (target_price)
      - 下多少 (quantity)
      - 置信度 (confidence)
      - 哪些策略贡献了信号 (contributing_strategies)

    Rust 侧 EIP-712 签名 + CLOB 下单只需要 token_id / direction /
    target_price / quantity 四个字段即可执行。
    """

    order_id: str = field(default_factory=lambda: f"ord_{datetime.now(timezone.utc).strftime('%Y%m%d%H%M%S%f')}")
    token_id: str = ""
    direction: SignalDirection = SignalDirection.BUY_YES
    event_type: EventType = EventType.RAIN
    location: str = ""

    # 定价与数量
    target_price: float = 0.0       # 聚合加权后的目标价
    market_price: float = 0.0       # 当前市场价 (来自 MarketSnapshot)
    quantity: float = 0.0           # 仓位计算后的下单数量

    # 置信度
    confidence: float = 0.0         # 综合置信度 (0.0~1.0)

    # 来源
    contributing_strategies: List[str] = field(default_factory=list)
    input_signal_count: int = 0      # 输入信号总数 (含被冲突相消的)

    # 状态
    status: OrderStatus = OrderStatus.OPEN
    block_reason: str = ""           # status=BLOCKED 时的拦截原因
    risk_checks: List[RiskCheckResult] = field(default_factory=list)

    # 生命周期
    timestamp: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    metadata: dict = field(default_factory=dict)

    @property
    def is_actionable(self) -> bool:
        """是否可实际下单 (通过风控且有数量)。"""
        return (
            self.status == OrderStatus.OPEN
            and self.quantity > 0
            and self.confidence > 0
        )

    def to_dict(self) -> dict:
        d = asdict(self)
        d["direction"] = self.direction.value
        d["event_type"] = self.event_type.value
        d["status"] = self.status.value
        d["timestamp"] = self.timestamp.isoformat()
        d["risk_checks"] = [r.to_dict() for r in self.risk_checks]
        return d

    @classmethod
    def from_dict(cls, data: dict) -> "AggregatedOrder":
        return cls(
            order_id=data.get("order_id", ""),
            token_id=data.get("token_id", ""),
            direction=SignalDirection(data["direction"]),
            event_type=EventType(data.get("event_type", "rain")),
            location=data.get("location", ""),
            target_price=data.get("target_price", 0.0),
            market_price=data.get("market_price", 0.0),
            quantity=data.get("quantity", 0.0),
            confidence=data.get("confidence", 0.0),
            contributing_strategies=data.get("contributing_strategies", []),
            input_signal_count=data.get("input_signal_count", 0),
            status=OrderStatus(data.get("status", "open")),
            block_reason=data.get("block_reason", ""),
            risk_checks=[
                RiskCheckResult(**r) for r in data.get("risk_checks", [])
            ],
            timestamp=(
                datetime.fromisoformat(data["timestamp"])
                if data.get("timestamp")
                else datetime.now(timezone.utc)
            ),
            metadata=data.get("metadata", {}),
        )


@dataclass
class RiskState:
    """风控状态快照。

    由调用方 (Rust 执行层 / 链上状态同步模块) 维护并定期更新,
    聚合器只读取不修改。

    职责边界:
      - 聚合器: 根据 RiskState 判断 "现在能不能交易"
      - Rust:   根据 AggregatedOrder 实际下单, 并回写 RiskState
    """

    # 当前持仓 {token_id: 数量} (正数=多头, 负数=空头)
    positions: Dict[str, float] = field(default_factory=dict)

    # 持仓名义价值 {token_id: USDC}（R2-P0-09：数量 ≠ 暴露）
    # notional = quantity * avg_price，用于正确的 USDC 暴露计算
    position_notionals: Dict[str, float] = field(default_factory=dict)

    # 各市场结算时间 {token_id: datetime}
    settlement_times: Dict[str, datetime] = field(default_factory=dict)

    # 当日盈亏 (USDC)
    daily_pnl: float = 0.0

    # 熔断标志 -- 触及每日亏损限制后置 True
    daily_circuit_breaker: bool = False

    # 手动熔断 (人工干预)
    manual_halt: bool = False

    # 上次重置日期 (用于判断是否需要重置 daily_pnl)
    last_reset_date: str = ""

    def has_position(self, token_id: str) -> bool:
        return abs(self.positions.get(token_id, 0.0)) > 1e-9

    def get_position(self, token_id: str) -> float:
        return self.positions.get(token_id, 0.0)

    def get_global_exposure(self) -> float:
        """全局暴露 = 所有持仓名义 USDC 之和（R2-P0-09 修复）。

        不能把 token 数量直接当暴露：100 shares @ $0.10 = $10，
        100 shares @ $0.90 = $90，风险完全不同。
        """
        if self.position_notionals:
            return sum(abs(v) for v in self.position_notionals.values())
        # 兼容旧数据：无 notionals 时退回数量之和（不精确，但保持可用）
        return sum(abs(v) for v in self.positions.values())

    def is_trading_halted(self) -> bool:
        """交易是否被全局熔断。"""
        return self.daily_circuit_breaker or self.manual_halt

    def to_dict(self) -> dict:
        d = asdict(self)
        d["settlement_times"] = {
            k: v.isoformat() for k, v in self.settlement_times.items()
        }
        return d

    @classmethod
    def from_dict(cls, data: dict) -> "RiskState":
        return cls(
            positions=data.get("positions", {}),
            position_notionals=data.get("position_notionals", {}),
            settlement_times={
                k: datetime.fromisoformat(v)
                for k, v in data.get("settlement_times", {}).items()
            },
            daily_pnl=data.get("daily_pnl", 0.0),
            daily_circuit_breaker=data.get("daily_circuit_breaker", False),
            manual_halt=data.get("manual_halt", False),
            last_reset_date=data.get("last_reset_date", ""),
        )
