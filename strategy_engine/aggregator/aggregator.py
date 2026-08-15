"""信号聚合器 -- 多策略信号收口为单条交易指令。

四步流水线 (每步可独立测试):

  1. ConflictResolver  按 token_id 分组, 仲裁方向冲突
  2. ConfidenceFuser   加权融合置信度 + 目标价格
  3. RiskGate (预检)    全局熔断 / 结算窗口 / 每日亏损
  4. PositionSizer      Kelly / 固定比例 算仓位
     RiskGate (后检)    仓位上限 / 全局暴露

是 Python 策略大脑与 Rust 执行手脚之间的收口层:
  - 输入: List[TradingSignal] + RiskState
  - 输出: List[AggregatedOrder] (每市场至多一条)
  - Rust 只消费 order.is_actionable 的指令做 EIP-712 签名 + CLOB 下单
"""

import logging
import os
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Dict, List, Optional

from ..models import EventType, SignalDirection, TradingSignal
from .confidence_fuser import ConfidenceFuser
from .conflict_resolver import ConflictResolver
from .models import AggregatedOrder, ConflictResolution, OrderStatus, RiskState
from .order_size_limits import OrderSizeLimits
from .position_sizer import PositionSizer
from .risk_gate import RiskGate

logger = logging.getLogger(__name__)


@dataclass
class AggregatorParams:
    """信号聚合器参数。

    可通过 from_env() 从环境变量加载, 也可在代码中直接构造。
    """

    # 策略权重 -- 历史准确率越高权重越大
    strategy_weights: Dict[str, float] = field(
        default_factory=lambda: {
            "forecast_deviation": 1.0,
            "multi_source": 1.2,
        }
    )

    # 冲突仲裁: 双向加权置信度差值低于此值则相消不交易
    conflict_threshold: float = 0.15

    # 综合置信度下限, 低于此值不下单
    min_aggregated_confidence: float = 0.5

    # 仓位计算
    position_sizing: str = "kelly"          # "kelly" | "fixed_fraction"
    kelly_fraction: float = 0.25           # 四分之一 Kelly (防方差爆炸)
    fixed_fraction: float = 0.05
    base_capital: float = 1000.0           # 计算仓位的基础资金 (USDC)

    # 下单金额上限 (根据总资产动态分档)
    total_portfolio_value: Optional[float] = None  # None = 不设限 (回测/测试)

    # 风控
    max_position_per_market: float = 1000.0
    max_global_exposure: float = 5000.0
    settlement_lock_hours: float = 2.0
    daily_loss_limit: float = 100.0

    @classmethod
    def from_env(cls) -> "AggregatorParams":
        """从环境变量加载。"""
        weights_str = os.getenv("AGG_STRATEGY_WEIGHTS", "")
        strategy_weights = {
            "forecast_deviation": 1.0,
            "multi_source": 1.2,
        }
        if weights_str:
            for pair in weights_str.split(","):
                if ":" in pair:
                    k, v = pair.split(":", 1)
                    strategy_weights[k.strip()] = float(v.strip())

        return cls(
            strategy_weights=strategy_weights,
            conflict_threshold=float(
                os.getenv("AGG_CONFLICT_THRESHOLD", "0.15")
            ),
            min_aggregated_confidence=float(
                os.getenv("AGG_MIN_CONFIDENCE", "0.5")
            ),
            position_sizing=os.getenv(
                "AGG_POSITION_SIZING", "kelly"
            ),
            kelly_fraction=float(
                os.getenv("AGG_KELLY_FRACTION", "0.25")
            ),
            fixed_fraction=float(
                os.getenv("AGG_FIXED_FRACTION", "0.05")
            ),
            base_capital=float(
                os.getenv("AGG_BASE_CAPITAL", "1000.0")
            ),
            total_portfolio_value=(
                float(os.getenv("AGG_PORTFOLIO_VALUE"))
                if os.getenv("AGG_PORTFOLIO_VALUE")
                else None
            ),
            max_position_per_market=float(
                os.getenv("AGG_MAX_POSITION", "1000.0")
            ),
            max_global_exposure=float(
                os.getenv("AGG_MAX_GLOBAL_EXPOSURE", "5000.0")
            ),
            settlement_lock_hours=float(
                os.getenv("AGG_SETTLEMENT_LOCK_HOURS", "2.0")
            ),
            daily_loss_limit=float(
                os.getenv("AGG_DAILY_LOSS_LIMIT", "100.0")
            ),
        )


class SignalAggregator:
    """信号聚合器。

    用法:
        params = AggregatorParams.from_env()
        aggregator = SignalAggregator(params)

        orders = aggregator.aggregate(
            signals=[sig1, sig2, ...],
            risk_state=risk_state,
        )

        for order in orders:
            if order.is_actionable:
                # 发给 Rust 执行层
                send_to_rust(order)
    """

    def __init__(self, params: Optional[AggregatorParams] = None):
        self.params = params or AggregatorParams()
        p = self.params

        self.conflict_resolver = ConflictResolver(
            strategy_weights=p.strategy_weights,
            conflict_threshold=p.conflict_threshold,
        )
        self.confidence_fuser = ConfidenceFuser(
            strategy_weights=p.strategy_weights,
            min_aggregated_confidence=p.min_aggregated_confidence,
        )
        self.risk_gate = RiskGate(
            max_position_per_market=p.max_position_per_market,
            max_global_exposure=p.max_global_exposure,
            settlement_lock_hours=p.settlement_lock_hours,
            daily_loss_limit=p.daily_loss_limit,
        )
        self.position_sizer = PositionSizer(
            method=p.position_sizing,
            kelly_fraction=p.kelly_fraction,
            fixed_fraction=p.fixed_fraction,
            base_capital=p.base_capital,
        )

        # 下单金额上限 (默认无限制用于回测)
        if p.total_portfolio_value is not None:
            self.size_limits = OrderSizeLimits.from_portfolio(
                p.total_portfolio_value
            )
        else:
            self.size_limits = OrderSizeLimits.unlimited()

    def aggregate(
        self,
        signals: List[TradingSignal],
        risk_state: Optional[RiskState] = None,
    ) -> List[AggregatedOrder]:
        """主入口: 收敛多策略信号为最终交易指令列表。

        Args:
            signals: 来自所有策略的信号 (可含不同 token_id / 不同方向)
            risk_state: 当前风控状态 (默认空状态)

        Returns:
            List[AggregatedOrder] -- 每 token_id 至多一条,
            含 actionable 和 blocked 两种, 供审计和执行
        """
        if risk_state is None:
            risk_state = RiskState()

        if not signals:
            logger.info("聚合器: 无输入信号")
            return []

        # Step 1: 按 token_id 分组并做冲突仲裁
        resolutions = self.conflict_resolver.resolve(signals)

        # 统计每个 token 的输入信号总数 (含被冲突相消的)
        input_counts: Dict[str, int] = {}
        for s in signals:
            input_counts[s.token_id] = input_counts.get(s.token_id, 0) + 1

        orders: List[AggregatedOrder] = []
        for token_id, resolution in resolutions.items():
            order = self._build_order(
                resolution=resolution,
                input_signal_count=input_counts.get(token_id, 0),
                risk_state=risk_state,
            )
            orders.append(order)

        # -- 后处理: 单市场敞口上限 --
        orders = self._apply_market_exposure_caps(orders)

        actionable = [o for o in orders if o.is_actionable]
        blocked = [o for o in orders if o.status == OrderStatus.BLOCKED]
        void = [o for o in orders if o.status == OrderStatus.CONFLICT_VOID]

        logger.info(
            "聚合完成: %d 个 token -> %d 可执行 / %d 风控拦截 / %d 冲突相消",
            len(orders), len(actionable), len(blocked), len(void),
        )
        return orders

    # ================================================================
    # 内部: 单个 token 的四步流水线
    # ================================================================

    def _build_order(
        self,
        resolution: ConflictResolution,
        input_signal_count: int,
        risk_state: RiskState,
    ) -> AggregatedOrder:
        """对单个 token 的仲裁结果执行融合 -> 风控 -> 仓位流水线。"""

        # -- 冲突相消 --
        if resolution.is_void:
            return self._make_void_order(
                resolution, input_signal_count
            )

        # -- Step 2: 置信度融合 --
        confidence, target_price, strategies, market_price = (
            self.confidence_fuser.fuse(resolution)
        )

        # 取事件类型和位置 (同 token 应一致)
        ref_signal = resolution.contributing_signals[0]
        order = AggregatedOrder(
            token_id=resolution.token_id,
            direction=resolution.direction,
            event_type=ref_signal.event_type,
            location=ref_signal.location,
            target_price=target_price,
            market_price=market_price,
            quantity=0.0,           # 暂未计算
            confidence=confidence,
            contributing_strategies=strategies,
            input_signal_count=input_signal_count,
            status=OrderStatus.OPEN,
        )

        # 置信度不足 -- 标记拦截
        if not self.confidence_fuser.is_confidence_sufficient(confidence):
            order.status = OrderStatus.BLOCKED
            order.block_reason = (
                f"综合置信度 {confidence:.4f} < 下限 "
                f"{self.confidence_fuser.min_confidence:.4f}"
            )
            logger.info(
                "token=%s 置信度不足: %.4f < %.4f",
                order.token_id, confidence,
                self.confidence_fuser.min_confidence,
            )
            return order

        # -- Step 3a: 预检查 (不需仓位) --
        pre_results = [
            self.risk_gate._check_halt(order, risk_state),
            self.risk_gate._check_settlement_window(order, risk_state),
            self.risk_gate._check_daily_loss(order, risk_state),
        ]
        order.risk_checks = list(pre_results)

        failure = self.risk_gate.first_failure(pre_results)
        if failure is not None:
            order.status = OrderStatus.BLOCKED
            order.block_reason = failure.reason
            logger.info(
                "token=%s 预检拦截 [%s]: %s",
                order.token_id, failure.rule, failure.reason,
            )
            return order

        # -- Step 4: 仓位计算 --
        # P0-08 修复：Kelly 使用 model_probability（= target_price），
        # confidence 仅作保守缩放因子，不作为概率本身。
        quantity, sizing_detail = self.position_sizer.size(
            model_probability=target_price,
            market_price=market_price,
            confidence=confidence,
            direction=resolution.direction,
        )

        # -- Step 4b: 下单金额上限 (总资产分档) --
        cost_per_share = (
            market_price if resolution.direction == SignalDirection.BUY_YES
            else (1.0 - market_price)
        )
        order_cost = quantity * cost_per_share
        size_detail = {
            "raw_quantity": quantity,
            "raw_cost": round(order_cost, 4),
            "limit": self.size_limits.max_order_cost,
        }
        if order_cost > self.size_limits.max_order_cost:
            quantity = self.size_limits.max_order_cost / cost_per_share
            order_cost = self.size_limits.max_order_cost
            size_detail["capped"] = True
            size_detail["capped_quantity"] = quantity
            logger.info(
                "token=%s 下单金额 $%.2f > 上限 $%.2f, 缩减至 %.2f 份",
                order.token_id, size_detail["raw_cost"],
                self.size_limits.max_order_cost, quantity,
            )
        else:
            size_detail["capped"] = False

        order.quantity = quantity
        order.metadata["sizing"] = sizing_detail
        order.metadata["order_size_limit"] = size_detail

        # -- Step 3b: 后检查 (需仓位) --
        post_results = [
            self.risk_gate._check_position_limit(order, risk_state),
            self.risk_gate._check_global_exposure(order, risk_state),
        ]
        order.risk_checks.extend(post_results)

        failure = self.risk_gate.first_failure(post_results)
        if failure is not None:
            order.status = OrderStatus.BLOCKED
            order.block_reason = failure.reason
            order.quantity = 0.0     # 拦截后清零, 防误执行
            logger.info(
                "token=%s 后检拦截 [%s]: %s",
                order.token_id, failure.rule, failure.reason,
            )
            return order

        # -- 通过全部检查 --
        order.status = OrderStatus.OPEN
        logger.info(
            "token=%s 可执行: %s qty=%.2f target=%.4f conf=%.4f",
            order.token_id, order.direction.value,
            order.quantity, order.target_price, order.confidence,
        )
        return order

    # ================================================================
    # 辅助
    # ================================================================

    def _make_void_order(
        self,
        resolution: ConflictResolution,
        input_signal_count: int,
    ) -> AggregatedOrder:
        """构造冲突相消的空指令 (不交易, 留审计记录)。"""
        ref_signal = (
            resolution.contributing_signals[0]
            if resolution.contributing_signals
            else None
        )
        return AggregatedOrder(
            token_id=resolution.token_id,
            direction=SignalDirection.BUY_YES,   # 占位, status=void
            event_type=ref_signal.event_type if ref_signal else EventType.RAIN,
            location=ref_signal.location if ref_signal else "",
            confidence=0.0,
            contributing_strategies=[],
            input_signal_count=input_signal_count,
            status=OrderStatus.CONFLICT_VOID,
            block_reason=resolution.void_reason,
        )

    def _apply_market_exposure_caps(
        self,
        orders: List[AggregatedOrder],
    ) -> List[AggregatedOrder]:
        """同市场敞口上限后处理。

        按 token_id (市场) 分组，若某市场有多笔 actionable 订单
        且总 cost 超过 max_market_exposure，则按比例缩减全部订单。
        实际场景中同轮聚合每个市场最多一笔，但保留正确性。
        """
        cap = self.size_limits.max_market_exposure
        if cap == float("inf"):
            return orders

        # 按 market (token_id → location 前两位) 分组
        from collections import defaultdict
        groups: dict = defaultdict(list)
        for o in orders:
            if o.is_actionable:
                # 用 token_id 前 12 字符做市场分组键 (同一城市+事件类型)
                key = o.token_id[:12] if len(o.token_id) >= 12 else o.token_id
                groups[key].append(o)

        capped_count = 0
        for key, group in groups.items():
            if len(group) <= 1:
                continue

            total_cost = sum(
                o.quantity * (
                    o.market_price if o.direction == SignalDirection.BUY_YES
                    else (1.0 - o.market_price)
                )
                for o in group
            )
            if total_cost <= cap:
                continue

            # 按比例缩减
            scale = cap / total_cost
            for o in group:
                o.quantity *= scale
                o.metadata.setdefault("order_size_limit", {})
                o.metadata["order_size_limit"]["market_capped"] = True
                o.metadata["order_size_limit"]["market_scale"] = round(scale, 4)
            capped_count += len(group)
            logger.info(
                "市场 %s: 总敞口 $%.2f > 上限 $%.2f, 缩减至 %.1f%%",
                key, total_cost, cap, scale * 100,
            )

        if capped_count:
            logger.info("敞口上限: 缩减了 %d 笔订单", capped_count)

        return orders

    def get_summary(self, orders: List[AggregatedOrder]) -> dict:
        """聚合结果摘要 (供日志 / GUI 展示)。"""
        actionable = [o for o in orders if o.is_actionable]
        blocked = [o for o in orders if o.status == OrderStatus.BLOCKED]
        void = [o for o in orders if o.status == OrderStatus.CONFLICT_VOID]

        return {
            "total_tokens": len(orders),
            "actionable": len(actionable),
            "blocked": len(blocked),
            "conflict_void": len(void),
            "total_quantity": sum(o.quantity for o in actionable),
            "avg_confidence": (
                sum(o.confidence for o in actionable) / len(actionable)
                if actionable else 0.0
            ),
            "block_reasons": [
                {"token_id": o.token_id, "reason": o.block_reason}
                for o in blocked
            ],
        }
