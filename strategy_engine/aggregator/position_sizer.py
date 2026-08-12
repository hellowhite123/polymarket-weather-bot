"""仓位计算器。

两种模式:

  Kelly 分数:
    预测市场二元结构 -- 买 YES token 成本 = price, 赢得 1 (净利 1-price)
    赔率 b = (1 - price) / price
    Kelly 最优比例 f* = (b*p - (1-p)) / b
    实际仓位 = kelly_fraction * f* * base_capital / price
    (kelly_fraction 通常取 0.25 ~ 0.5, 防止过拟合和方差爆炸)

  固定比例:
    数量 = base_capital * fixed_fraction * confidence / price
    (更保守, 与置信度线性挂钩)
"""

import logging
from typing import Tuple

from ..models import SignalDirection

logger = logging.getLogger(__name__)


class PositionSizer:
    """下单数量计算器。"""

    def __init__(
        self,
        method: str = "kelly",
        kelly_fraction: float = 0.25,
        fixed_fraction: float = 0.05,
        base_capital: float = 1000.0,
    ):
        """
        Args:
            method: "kelly" 或 "fixed_fraction"
            kelly_fraction: Kelly 缩小倍数 (0.25 = 四分之一 Kelly)
            fixed_fraction: 固定比例模式下的资金比例
            base_capital: 计算仓位的基础资金 (USDC)
        """
        if method not in ("kelly", "fixed_fraction"):
            raise ValueError(f"未知仓位模式: {method}")
        self.method = method
        self.kelly_fraction = kelly_fraction
        self.fixed_fraction = fixed_fraction
        self.base_capital = base_capital

    def size(
        self,
        confidence: float,
        market_price: float,
        direction: SignalDirection,
    ) -> Tuple[float, dict]:
        """根据置信度和市场价格计算下单数量。

        Returns:
            (quantity, details) -- details 含计算过程, 供审计
        """
        if confidence <= 0:
            return 0.0, {"reason": "confidence <= 0"}

        if market_price <= 0 or market_price >= 1:
            return 0.0, {"reason": f"无效市场价格 {market_price}"}

        if self.method == "kelly":
            qty, detail = self._kelly(confidence, market_price, direction)
        else:
            qty, detail = self._fixed(confidence, market_price)

        qty = max(0.0, qty)
        detail["method"] = self.method
        detail["confidence"] = confidence
        detail["market_price"] = market_price
        detail["quantity"] = qty
        return qty, detail

    def _kelly(
        self, p: float, price: float, direction: SignalDirection
    ) -> Tuple[float, dict]:
        """Kelly 公式仓位计算。

        买 YES: 成本 price, 赔率 b = (1-price)/price
        买 NO:  成本 (1-price), 赔率 b = price/(1-price)
        """
        if direction == SignalDirection.BUY_YES:
            b = (1.0 - price) / price
        else:
            # 买 NO: 成本 = no_price = 1 - yes_price
            # 赢得 1, 净利 = 1 - (1-price) = price
            # 赔率 b = price / (1 - price)
            b = price / (1.0 - price)

        if b <= 0:
            return 0.0, {"reason": f"赔率 b={b} <= 0"}

        q = 1.0 - p
        f_star = (b * p - q) / b

        if f_star <= 0:
            return 0.0, {
                "reason": f"Kelly f*={f_star:.4f} <= 0 (预期负收益)",
                "b": b, "p": p, "f_star": f_star,
            }

        f = self.kelly_fraction * f_star
        # 数量 = 资金 * f / 单位成本
        cost = price if direction == SignalDirection.BUY_YES else (1.0 - price)
        quantity = self.base_capital * f / cost

        return quantity, {
            "b": b, "p": p, "f_star": f_star,
            "kelly_fraction": self.kelly_fraction,
            "f_applied": f, "cost": cost,
        }

    def _fixed(self, confidence: float, price: float) -> Tuple[float, dict]:
        """固定比例仓位计算。"""
        quantity = (
            self.base_capital
            * self.fixed_fraction
            * confidence
            / price
        )
        return quantity, {
            "fixed_fraction": self.fixed_fraction,
            "base_capital": self.base_capital,
        }
