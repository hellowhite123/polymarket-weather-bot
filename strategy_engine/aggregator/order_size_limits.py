"""下单金额限制 — 根据总资产动态调整单次下单上限。

分档规则：
  总资产 < $100:     每次下单 ≤ $1,  单市场 ≤ $3
  $100 ≤ 总资产 < $500:  每次下单 ≤ $1,  单市场 ≤ $5
  $500 ≤ 总资产 < $2,000: 单市场 ≤ $10
  $2,000 ≤ 总资产 < $10,000: 单市场 ≤ $50
  $10,000 ≤ 总资产:        单市场 ≤ $100

设计意图：
  - 小资金以保守试错为主，每次只下 $1
  - 随着资产增长逐步放宽，控制单一市场集中度
  - 是 Kelly 仓位计算的上限（cap），而非替代它
"""

from dataclasses import dataclass


@dataclass(frozen=True)
class OrderSizeLimits:
    """动态下单金额上限。"""

    max_order_cost: float          # 单次下单最大金额 (USD)
    max_market_exposure: float     # 单个市场最大敞口 (USD)

    @classmethod
    def from_portfolio(cls, total_value_usd: float) -> "OrderSizeLimits":
        """根据总资产 (USDC + 持仓市值) 返回对应的限制。

        Args:
            total_value_usd: 总资产金额 (美元)，含 USDC 余额 + 所有持仓市值
        """
        if total_value_usd < 100.0:
            return cls(max_order_cost=1.0, max_market_exposure=3.0)
        elif total_value_usd < 500.0:
            return cls(max_order_cost=1.0, max_market_exposure=5.0)
        elif total_value_usd < 2000.0:
            return cls(max_order_cost=10.0, max_market_exposure=10.0)
        elif total_value_usd < 10000.0:
            return cls(max_order_cost=50.0, max_market_exposure=50.0)
        else:
            return cls(max_order_cost=100.0, max_market_exposure=100.0)

    @classmethod
    def unlimited(cls) -> "OrderSizeLimits":
        """无限制 (回测/测试用)。"""
        return cls(
            max_order_cost=float("inf"),
            max_market_exposure=float("inf"),
        )
