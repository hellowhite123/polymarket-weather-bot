"""风控闸门。

在下单前后做两阶段检查:

  预检查 (仓位计算前, 不需 quantity):
    - global_halt:         全局熔断 (手动 + 每日亏损触发)
    - settlement_window:   距结算 < settlement_lock_hours 禁新开仓
    - daily_loss:           当日亏损触及上限

  后检查 (仓位计算后, 需 quantity):
    - position_limit:      单市场持仓 + 新增 > 上限
    - global_exposure:     全局暴露 + 新增 > 上限

任何一条不通过, 指令标记 BLOCKED 并记录原因。
仓位为 0 时, 后检查自动通过 (不增加暴露)。
"""

import logging
from datetime import datetime, timezone
from typing import List

from .models import AggregatedOrder, RiskCheckResult, RiskState

logger = logging.getLogger(__name__)


class RiskGate:
    """风控闸门。"""

    def __init__(
        self,
        max_position_per_market: float = 1000.0,
        max_global_exposure: float = 5000.0,
        settlement_lock_hours: float = 2.0,
        daily_loss_limit: float = 100.0,
    ):
        self.max_position_per_market = max_position_per_market
        self.max_global_exposure = max_global_exposure
        self.settlement_lock_hours = settlement_lock_hours
        self.daily_loss_limit = daily_loss_limit

    def check_all(
        self, order: AggregatedOrder, state: RiskState
    ) -> List[RiskCheckResult]:
        """执行全部风控检查, 返回结果列表。"""
        return [
            self._check_halt(order, state),
            self._check_settlement_window(order, state),
            self._check_daily_loss(order, state),
            self._check_position_limit(order, state),
            self._check_global_exposure(order, state),
        ]

    def any_failed(self, results: List[RiskCheckResult]) -> bool:
        """是否任一检查未通过。"""
        return any(not r.passed for r in results)

    def first_failure(self, results: List[RiskCheckResult]) -> RiskCheckResult:
        """返回第一个未通过的检查 (无则 None)。"""
        for r in results:
            if not r.passed:
                return r
        return None

    # ================================================================
    # 预检查 (不需仓位)
    # ================================================================

    def _check_halt(
        self, order: AggregatedOrder, state: RiskState
    ) -> RiskCheckResult:
        if state.is_trading_halted():
            reason = "手动熔断" if state.manual_halt else "每日亏损熔断"
            logger.warning("token=%s 全局熔断: %s", order.token_id, reason)
            return RiskCheckResult(
                rule="global_halt", passed=False, reason=reason
            )
        return RiskCheckResult(rule="global_halt", passed=True)

    def _check_settlement_window(
        self, order: AggregatedOrder, state: RiskState
    ) -> RiskCheckResult:
        settlement = state.settlement_times.get(order.token_id)
        if settlement is None:
            return RiskCheckResult(rule="settlement_window", passed=True)

        now = datetime.now(timezone.utc)
        if settlement.tzinfo is None:
            settlement = settlement.replace(tzinfo=timezone.utc)

        delta_hours = (settlement - now).total_seconds() / 3600.0
        if delta_hours < self.settlement_lock_hours:
            logger.info(
                "token=%s 结算锁定: 距结算 %.2fh < 锁定期 %.2fh",
                order.token_id, delta_hours, self.settlement_lock_hours,
            )
            return RiskCheckResult(
                rule="settlement_window",
                passed=False,
                reason=(
                    f"距结算仅 {delta_hours:.2f}h < "
                    f"锁定期 {self.settlement_lock_hours}h"
                ),
            )
        return RiskCheckResult(rule="settlement_window", passed=True)

    def _check_daily_loss(
        self, order: AggregatedOrder, state: RiskState
    ) -> RiskCheckResult:
        if state.daily_pnl < -self.daily_loss_limit:
            return RiskCheckResult(
                rule="daily_loss",
                passed=False,
                reason=(
                    f"当日亏损 {state.daily_pnl:.2f} < "
                    f"-{self.daily_loss_limit:.2f}"
                ),
            )
        return RiskCheckResult(rule="daily_loss", passed=True)

    # ================================================================
    # 后检查 (需仓位)
    # ================================================================

    def _check_position_limit(
        self, order: AggregatedOrder, state: RiskState
    ) -> RiskCheckResult:
        if order.quantity <= 0:
            return RiskCheckResult(rule="position_limit", passed=True)

        existing = abs(state.get_position(order.token_id))
        new_total = existing + order.quantity
        if new_total > self.max_position_per_market:
            return RiskCheckResult(
                rule="position_limit",
                passed=False,
                reason=(
                    f"持仓 {existing:.2f} + 新增 {order.quantity:.2f} "
                    f"= {new_total:.2f} > 上限 "
                    f"{self.max_position_per_market:.2f}"
                ),
            )
        return RiskCheckResult(rule="position_limit", passed=True)

    def _check_global_exposure(
        self, order: AggregatedOrder, state: RiskState
    ) -> RiskCheckResult:
        if order.quantity <= 0:
            return RiskCheckResult(rule="global_exposure", passed=True)

        current = state.get_global_exposure()
        new_total = current + order.quantity
        if new_total > self.max_global_exposure:
            return RiskCheckResult(
                rule="global_exposure",
                passed=False,
                reason=(
                    f"全局暴露 {current:.2f} + 新增 {order.quantity:.2f} "
                    f"= {new_total:.2f} > 上限 "
                    f"{self.max_global_exposure:.2f}"
                ),
            )
        return RiskCheckResult(rule="global_exposure", passed=True)
