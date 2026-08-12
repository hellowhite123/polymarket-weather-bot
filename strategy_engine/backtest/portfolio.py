"""组合管理器。

跟踪回测过程中的:
  - 现金账户
  - 持仓列表
  - 已实现/未实现盈亏
  - 权益曲线
"""

import logging
from datetime import datetime, timezone
from typing import Dict, List, Optional

from .models import Position, TradeRecord, EquityPoint, TradeAction

logger = logging.getLogger(__name__)


class Portfolio:
    """回测组合管理器。"""

    def __init__(
        self,
        initial_capital: float = 10000.0,
        max_open_positions: int = 20,
        max_position_size: float = 1000.0,
    ):
        self.cash: float = initial_capital
        self.initial_capital: float = initial_capital
        self.max_open_positions = max_open_positions
        self.max_position_size = max_position_size

        # 持仓: {token_id: Position}
        self.positions: Dict[str, Position] = {}

        # 交易记录
        self.trades: List[TradeRecord] = []

        # 权益曲线
        self.equity_curve: List[EquityPoint] = []

        # 累计统计
        self.realized_pnl: float = 0.0
        self.total_fees: float = 0.0
        self.total_slippage: float = 0.0

    # ================================================================
    # 开仓
    # ================================================================

    def open_position(self, trade: TradeRecord, deadline: Optional[datetime] = None) -> bool:
        """开仓。

        Returns:
            True 如果开仓成功
        """
        # 检查资金
        if trade.cost > self.cash:
            logger.debug(
                "资金不足: 需要 %.2f, 可用 %.2f (%s)",
                trade.cost, self.cash, trade.token_id,
            )
            return False

        # 检查持仓数量上限
        if trade.token_id not in self.positions:
            if len(self.positions) >= self.max_open_positions:
                logger.debug("持仓数量上限: %d", self.max_open_positions)
                return False

        # 检查单笔仓位上限
        if trade.cost > self.max_position_size:
            logger.debug(
                "单笔仓位超限: %.2f > %.2f (%s)",
                trade.cost, self.max_position_size, trade.token_id,
            )
            return False

        # 扣除资金
        self.cash -= trade.cost
        self.total_fees += trade.fee
        self.total_slippage += trade.slippage

        # 更新持仓
        if trade.token_id in self.positions:
            # 加仓: 计算新均价
            pos = self.positions[trade.token_id]
            total_qty = pos.quantity + trade.quantity
            if total_qty > 0:
                pos.entry_price = (
                    (pos.entry_price * pos.quantity + trade.price * trade.quantity)
                    / total_qty
                )
            pos.quantity = total_qty
            pos.cost += trade.cost
            logger.debug("加仓 %s: qty=%.2f avg_price=%.4f", trade.token_id, pos.quantity, pos.entry_price)
        else:
            self.positions[trade.token_id] = Position(
                token_id=trade.token_id,
                direction=trade.direction,
                quantity=trade.quantity,
                entry_price=trade.price,
                cost=trade.cost,
                open_time=trade.timestamp,
                strategy=trade.strategy,
                confidence=trade.confidence,
                deadline=deadline,
                metadata=trade.metadata,
            )
            logger.debug(
                "开仓 %s: %s qty=%.2f @ %.4f cost=%.2f",
                trade.token_id, trade.direction, trade.quantity, trade.price, trade.cost,
            )

        self.trades.append(trade)
        return True

    # ================================================================
    # 结算
    # ================================================================

    def settle_position(self, trade: TradeRecord) -> None:
        """结算持仓 (事件结果已知)。"""
        token_id = trade.token_id

        if token_id not in self.positions:
            logger.warning("结算时未找到持仓: %s", token_id)
            return

        pos = self.positions[token_id]

        # 结算回报
        if trade.pnl > 0:
            # 赢: 收回 quantity * $1
            self.cash += pos.quantity
        # 输: 已经在开仓时扣除了成本, 不需要额外操作

        self.realized_pnl += trade.pnl
        self.total_fees += trade.fee
        self.cash -= trade.fee

        # 记录交易
        self.trades.append(trade)

        # 移除持仓
        del self.positions[token_id]

        logger.debug(
            "结算 %s: pnl=%+.2f (won=%s) cash=%.2f",
            token_id, trade.pnl, trade.pnl > 0, self.cash,
        )

    # ================================================================
    # 权益快照
    # ================================================================

    def snapshot(
        self,
        current_time: datetime,
        positions_value: float = 0.0,
    ) -> EquityPoint:
        """记录当前权益快照。"""
        equity = self.cash + positions_value
        unrealized = positions_value - sum(p.cost for p in self.positions.values())

        point = EquityPoint(
            timestamp=current_time,
            cash=self.cash,
            positions_value=positions_value,
            equity=equity,
            open_positions=len(self.positions),
            realized_pnl=self.realized_pnl,
            unrealized_pnl=unrealized,
        )
        self.equity_curve.append(point)
        return point

    # ================================================================
    # 查询
    # ================================================================

    def get_positions_value(self, market_values: Dict[str, float]) -> float:
        """计算所有持仓的市值。

        Args:
            market_values: {token_id: current_value}
        """
        total = 0.0
        for token_id, pos in self.positions.items():
            total += market_values.get(token_id, pos.cost)
        return total

    @property
    def total_equity(self) -> float:
        """当前总权益 (不含未实现盈亏的精确估值)。"""
        return self.cash + sum(p.cost for p in self.positions.values())

    def has_position(self, token_id: str) -> bool:
        return token_id in self.positions

    def get_open_position_count(self) -> int:
        return len(self.positions)

    def get_positions_list(self) -> List[Position]:
        return list(self.positions.values())
