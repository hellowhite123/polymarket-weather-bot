"""dataclass <-> proto 消息转换器。

所有转换函数都是纯函数, 无副作用, 便于测试。
"""

from datetime import datetime, timezone
from typing import Dict, List, Optional

from strategy_engine.aggregator.models import (
    AggregatedOrder,
    RiskState,
    RiskCheckResult,
    OrderStatus,
)
from strategy_engine.models import EventType, SignalDirection

from strategy_engine.grpc_bridge import polymarket_pb2 as pb


# ============================================================
# 枚举映射
# ============================================================

_DIRECTION_MAP = {
    SignalDirection.BUY_YES: pb.Direction.BUY_YES,
    SignalDirection.BUY_NO: pb.Direction.BUY_NO,
}

_DIRECTION_REV_MAP = {v: k for k, v in _DIRECTION_MAP.items()}

_EVENT_TYPE_MAP = {
    EventType.RAIN: pb.EventType.RAIN,
    EventType.SNOW: pb.EventType.SNOW,
    EventType.TEMPERATURE: pb.EventType.TEMPERATURE,
    EventType.WIND: pb.EventType.WIND,
}

_EVENT_TYPE_REV_MAP = {v: k for k, v in _EVENT_TYPE_MAP.items()}

_ORDER_STATUS_MAP = {
    OrderStatus.OPEN: pb.OrderStatus.OPEN,
    OrderStatus.BLOCKED: pb.OrderStatus.BLOCKED,
    OrderStatus.CONFLICT_VOID: pb.OrderStatus.CONFLICT_VOID,
    OrderStatus.NO_SIGNAL: pb.OrderStatus.NO_SIGNAL,
}

_ORDER_STATUS_REV_MAP = {v: k for k, v in _ORDER_STATUS_MAP.items()}


# ============================================================
# AggregatedOrder: dataclass -> proto
# ============================================================

def order_to_proto(order: AggregatedOrder, paper_trading: bool = False) -> pb.AggregatedOrder:
    """将 Python AggregatedOrder 转换为 proto 消息。"""
    risk_checks = [
        pb.RiskCheckResult(rule=rc.rule, passed=rc.passed, reason=rc.reason)
        for rc in order.risk_checks
    ]

    ts = order.timestamp
    if isinstance(ts, datetime):
        ts_str = ts.isoformat()
    else:
        ts_str = str(ts) if ts else ""

    return pb.AggregatedOrder(
        order_id=order.order_id,
        token_id=order.token_id,
        direction=_DIRECTION_MAP.get(order.direction, pb.Direction.DIRECTION_UNSPECIFIED),
        event_type=_EVENT_TYPE_MAP.get(order.event_type, pb.EventType.EVENT_TYPE_UNSPECIFIED),
        location=order.location or "",
        target_price=order.target_price,
        market_price=order.market_price,
        quantity=order.quantity,
        confidence=order.confidence,
        contributing_strategies=order.contributing_strategies,
        input_signal_count=order.input_signal_count,
        status=_ORDER_STATUS_MAP.get(order.status, pb.OrderStatus.ORDER_STATUS_UNSPECIFIED),
        block_reason=order.block_reason or "",
        risk_checks=risk_checks,
        timestamp=ts_str,
        metadata={k: str(v) for k, v in order.metadata.items()},
        paper_trading=paper_trading,
    )


# ============================================================
# RiskState: dataclass -> proto
# ============================================================

def risk_state_to_proto(state: RiskState) -> pb.RiskState:
    """将 Python RiskState 转换为 proto 消息。"""
    positions = [
        pb.PositionEntry(token_id=tid, quantity=qty)
        for tid, qty in state.positions.items()
    ]

    settlement_times = []
    for tid, dt in state.settlement_times.items():
        ts_str = dt.isoformat() if isinstance(dt, datetime) else str(dt)
        settlement_times.append(pb.SettlementTimeEntry(token_id=tid, settlement_time=ts_str))

    return pb.RiskState(
        positions=positions,
        settlement_times=settlement_times,
        daily_pnl=state.daily_pnl,
        daily_circuit_breaker=state.daily_circuit_breaker,
        manual_halt=state.manual_halt,
        last_reset_date=state.last_reset_date or "",
    )


# ============================================================
# proto -> dataclass
# ============================================================

def order_from_proto(msg: pb.AggregatedOrder) -> AggregatedOrder:
    """将 proto AggregatedOrder 转换回 Python dataclass。"""
    risk_checks = [
        RiskCheckResult(rule=rc.rule, passed=rc.passed, reason=rc.reason)
        for rc in msg.risk_checks
    ]

    ts = datetime.now(timezone.utc)
    if msg.timestamp:
        try:
            ts = datetime.fromisoformat(msg.timestamp)
        except (ValueError, TypeError):
            pass

    return AggregatedOrder(
        order_id=msg.order_id,
        token_id=msg.token_id,
        direction=_DIRECTION_REV_MAP.get(msg.direction, SignalDirection.BUY_YES),
        event_type=_EVENT_TYPE_REV_MAP.get(msg.event_type, EventType.RAIN),
        location=msg.location,
        target_price=msg.target_price,
        market_price=msg.market_price,
        quantity=msg.quantity,
        confidence=msg.confidence,
        contributing_strategies=list(msg.contributing_strategies),
        input_signal_count=msg.input_signal_count,
        status=_ORDER_STATUS_REV_MAP.get(msg.status, OrderStatus.OPEN),
        block_reason=msg.block_reason,
        risk_checks=risk_checks,
        timestamp=ts,
        metadata=dict(msg.metadata) if msg.metadata else {},
    )


def risk_state_from_proto(msg: pb.RiskState) -> RiskState:
    """将 proto RiskState 转换回 Python dataclass。"""
    positions = {p.token_id: p.quantity for p in msg.positions}

    settlement_times = {}
    for st in msg.settlement_times:
        try:
            settlement_times[st.token_id] = datetime.fromisoformat(st.settlement_time)
        except (ValueError, TypeError):
            pass

    return RiskState(
        positions=positions,
        settlement_times=settlement_times,
        daily_pnl=msg.daily_pnl,
        daily_circuit_breaker=msg.daily_circuit_breaker,
        manual_halt=msg.manual_halt,
        last_reset_date=msg.last_reset_date,
    )


def order_result_from_proto(msg) -> dict:
    """将 proto OrderResult 转换为 Python dict (不对应 dataclass, 直接用 dict)。"""
    return {
        "order_id": msg.order_id,
        "success": msg.success,
        "exchange_order_id": msg.exchange_order_id,
        "error_message": msg.error_message,
        "timestamp": msg.timestamp,
        "filled_price": msg.filled_price,
        "filled_size": msg.filled_size,
        "status": msg.status,  # int enum value
        "status_name": pb.ExecutionStatus.Name(msg.status),
        # 成交记录完整字段
        "filled_at": msg.filled_at,
        "settled_at": msg.settled_at,
        "slippage": msg.slippage,
        "slippage_pct": msg.slippage_pct,
        "fee": msg.fee,
        "pnl": msg.pnl,
    }


def fill_record_from_proto(msg) -> dict:
    """将 proto FillRecord 转换为 Python dict。"""
    return {
        "order_id": msg.order_id,
        "token_id": msg.token_id,
        "side": msg.side,
        "placed_at": msg.placed_at,
        "filled_at": msg.filled_at,
        "settled_at": msg.settled_at,
        "limit_price": msg.limit_price,
        "filled_price": msg.filled_price,
        "size": msg.size,
        "fee": msg.fee,
        "slippage": msg.slippage,
        "slippage_pct": msg.slippage_pct,
        "pnl": msg.pnl,
        "status": msg.status,
    }
