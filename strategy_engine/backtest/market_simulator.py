"""市场模拟器。

模拟 Polymarket 二分市场的行为:
  - 订单成交 (含滑点和手续费)
  - 事件结算 (YES 赢 -> YES token 价值 $1, NO token 价值 $0)
  - 市场价格演化 (从市场快照序列中插值)
"""

import logging
from datetime import datetime, timezone
from typing import Dict, List, Optional, Tuple

from .models import HistoricalEvent, EventOutcome, TradeRecord, TradeAction

logger = logging.getLogger(__name__)


class MarketSimulator:
    """市场模拟器。

    职责:
      1. 根据当前时间从事件的市场快照序列中获取当前价格
      2. 模拟订单成交 (加滑点 + 手续费)
      3. 结算事件 (根据真实结果计算 P&L)
    """

    def __init__(
        self,
        slippage_bps: float = 50.0,
        fee_bps: float = 20.0,
    ):
        """
        Args:
            slippage_bps: 滑点 (基点, 1bp=0.01%)
            fee_bps: 手续费 (基点)
        """
        self.slippage_bps = slippage_bps
        self.fee_bps = fee_bps

    def get_market_price(
        self,
        event: HistoricalEvent,
        current_time: datetime,
    ) -> Optional[Tuple[float, float, float]]:
        """获取事件在当前时间的市场价格。

        从市场快照时间序列中找到最接近的快照,
        返回 (yes_price, no_price, spread)。

        Args:
            event: 历史事件
            current_time: 当前回测时间

        Returns:
            (yes_price, no_price, spread) 或 None (无市场数据)
        """
        if not event.market_history:
            return None

        # 找到时间戳 <= current_time 的最后一个快照（禁止未来数据，P0-05）
        best_snapshot = None
        for snap in event.market_history:
            snap_time = datetime.fromisoformat(snap["timestamp"])
            if snap_time.tzinfo is None:
                snap_time = snap_time.replace(tzinfo=timezone.utc)
            if current_time.tzinfo is None:
                current_time = current_time.replace(tzinfo=timezone.utc)

            # 跳过未来快照（look-ahead 防护）
            if snap_time > current_time:
                continue

            if best_snapshot is None:
                best_snapshot = snap
                continue

            best_time = datetime.fromisoformat(best_snapshot["timestamp"])
            if best_time.tzinfo is None:
                best_time = best_time.replace(tzinfo=timezone.utc)
            if snap_time > best_time:
                best_snapshot = snap

        if best_snapshot is None:
            return None

        return (
            best_snapshot["yes_price"],
            best_snapshot["no_price"],
            best_snapshot["spread"],
        )

    def simulate_order(
        self,
        event: HistoricalEvent,
        direction: str,
        quantity: float,
        current_time: datetime,
        strategy: str = "",
        confidence: float = 0.0,
    ) -> Optional[TradeRecord]:
        """模拟订单成交。

        Args:
            event: 交易的事件
            direction: BUY_YES / BUY_NO
            quantity: 下单数量
            current_time: 当前回测时间
            strategy: 来源策略
            confidence: 信号置信度

        Returns:
            TradeRecord 或 None (无法成交)
        """
        prices = self.get_market_price(event, current_time)
        if prices is None:
            logger.debug("无市场数据: %s", event.token_id)
            return None

        yes_price, no_price, spread = prices

        # 基础价格
        if direction == "BUY_YES":
            base_price = yes_price
        else:
            base_price = no_price

        if base_price <= 0 or base_price >= 1:
            return None

        # 滑点: 买入时价格上移
        slippage = base_price * self.slippage_bps / 10000.0
        fill_price = min(0.99, base_price + slippage)

        # 手续费
        notional = quantity * fill_price
        fee = notional * self.fee_bps / 10000.0

        # 总成本
        cost = notional + fee

        return TradeRecord(
            timestamp=current_time,
            action=TradeAction.OPEN,
            token_id=event.token_id,
            direction=direction,
            quantity=quantity,
            price=fill_price,
            cost=cost,
            fee=fee,
            slippage=slippage * quantity,
            strategy=strategy,
            confidence=confidence,
            metadata={
                "base_price": base_price,
                "yes_price": yes_price,
                "no_price": no_price,
                "spread": spread,
                "location": event.location,
                "event_type": event.event_type,
            },
        )

    def settle_position(
        self,
        token_id: str,
        direction: str,
        quantity: float,
        entry_price: float,
        event: HistoricalEvent,
        settle_time: datetime,
        strategy: str = "",
    ) -> TradeRecord:
        """结算持仓。

        Polymarket 二分市场结算规则:
          - 事件 YES: YES token = $1, NO token = $0
          - 事件 NO:  YES token = $0, NO token = $1

        P&L 计算:
          BUY_YES + outcome=YES: pnl = quantity * (1 - entry_price) - fee
          BUY_YES + outcome=NO:  pnl = -quantity * entry_price - fee
          BUY_NO  + outcome=NO:  pnl = quantity * (1 - entry_price) - fee
          BUY_NO  + outcome=YES: pnl = -quantity * entry_price - fee
        """
        outcome = event.outcome

        # 判断持仓方向是否赢
        if direction == "BUY_YES":
            won = (outcome == EventOutcome.YES)
        else:
            won = (outcome == EventOutcome.NO)

        if won:
            # 赢: 收回 quantity * $1, 成本 = quantity * entry_price
            gross_pnl = quantity * (1.0 - entry_price)
        else:
            # 输: 损失全部成本
            gross_pnl = -quantity * entry_price

        # 结算手续费 (卖出时的手续费)
        settle_fee = quantity * self.fee_bps / 10000.0
        pnl = gross_pnl - settle_fee

        # 确保 settle_time 有时区
        if settle_time.tzinfo is None:
            settle_time = settle_time.replace(tzinfo=timezone.utc)

        return TradeRecord(
            timestamp=settle_time,
            action=TradeAction.SETTLE,
            token_id=token_id,
            direction=direction,
            quantity=quantity,
            price=1.0 if won else 0.0,
            cost=0.0,
            fee=settle_fee,
            slippage=0.0,
            pnl=pnl,
            strategy=strategy,
            metadata={
                "outcome": outcome.value,
                "won": won,
                "entry_price": entry_price,
                "gross_pnl": gross_pnl,
            },
        )

    def get_position_value(
        self,
        direction: str,
        quantity: float,
        entry_price: float,
        event: HistoricalEvent,
        current_time: datetime,
    ) -> float:
        """估算持仓的当前市值。

        用市场隐含概率作为估值基准:
          BUY_YES: value = quantity * yes_price
          BUY_NO:  value = quantity * no_price
        """
        prices = self.get_market_price(event, current_time)
        if prices is None:
            # 无市场数据, 用 entry_price 估值
            return quantity * entry_price

        yes_price, no_price, _ = prices
        if direction == "BUY_YES":
            return quantity * yes_price
        else:
            return quantity * no_price
