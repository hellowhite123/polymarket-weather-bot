"""回测引擎 -- 主循环。

回测流程:
  1. 加载历史事件数据
  2. 按时间步遍历
  3. 每个时间步:
     a. 检查是否有持仓需要结算 (事件截止)
     b. 为活跃事件生成策略信号
     c. 聚合信号 -> 交易指令
     d. 模拟下单
     e. 记录权益快照
  4. 结算所有剩余持仓
  5. 计算绩效指标
  6. 生成报告

集成现有模块:
  - StrategyPipeline: 策略评估 + 信号生成
  - SignalAggregator: 多策略信号聚合
  - MarketSimulator: 订单成交 + 事件结算
  - Portfolio: 持仓 + 资金管理
  - MetricsCalculator: 绩效指标
"""

import asyncio
import json
import logging
import os
from datetime import datetime, timezone, timedelta
from pathlib import Path
from typing import Dict, List, Optional

from .models import (
    BacktestConfig,
    BacktestResult,
    EventOutcome,
    HistoricalEvent,
    Position,
    TradeAction,
    TradeRecord,
)
from .data_loader import DataLoader
from .market_simulator import MarketSimulator
from .portfolio import Portfolio
from .metrics import MetricsCalculator
from .reporter import BacktestReporter

logger = logging.getLogger(__name__)


class BacktestEngine:
    """回测引擎。

    用法:
        config = BacktestConfig(initial_capital=10000)
        engine = BacktestEngine(config)

        # 加载历史数据
        events = DataLoader().generate_synthetic_events(n_events=100)

        # 运行回测
        result = await engine.run(events)

        # 生成报告
        reporter = BacktestReporter()
        reporter.print_summary(result)
        reporter.save_json(result, "backtest_result.json")
    """

    def __init__(self, config: Optional[BacktestConfig] = None):
        self.config = config or BacktestConfig()
        self.simulator = MarketSimulator(
            slippage_bps=self.config.slippage_bps,
            fee_bps=self.config.fee_bps,
        )
        self.portfolio = Portfolio(
            initial_capital=self.config.initial_capital,
            max_open_positions=self.config.max_open_positions,
            max_position_size=self.config.max_position_size,
        )
        self.metrics = MetricsCalculator()

    async def run(
        self,
        events: List[HistoricalEvent],
    ) -> BacktestResult:
        """运行回测。

        Args:
            events: 历史事件列表 (按 deadline 排序)

        Returns:
            BacktestResult
        """
        if not events:
            logger.warning("无历史事件, 回测终止")
            return BacktestResult()

        # 按截止时间排序
        events = sorted(events, key=lambda e: e.deadline)

        # 确定回测时间范围
        start_time = events[0].deadline - timedelta(hours=48)
        end_time = events[-1].deadline + timedelta(hours=1)
        total_hours = (end_time - start_time).total_seconds() / 3600

        logger.info(
            "回测开始: %d 个事件, %s ~ %s (%.1f 小时, %d 步)",
            len(events), start_time.strftime("%Y-%m-%d %H:%M"),
            end_time.strftime("%Y-%m-%d %H:%M"),
            total_hours, int(total_hours / self.config.step_hours),
        )

        # 时间步遍历
        current_time = start_time
        step_count = 0
        active_events: List[HistoricalEvent] = []

        while current_time <= end_time:
            step_count += 1

            # Step 1: 结算到期事件
            settled = self._settle_expired_events(events, current_time)

            # Step 2: 找到当前活跃事件 (有市场数据且未结算)
            active_events = self._get_active_events(events, current_time)

            # Step 3: 生成交易信号
            if active_events:
                signals = await self._generate_signals(active_events, current_time)

                # Step 4: 聚合 + 下单
                if signals:
                    orders = self._aggregate_signals(signals)
                    self._execute_orders(orders, active_events, current_time)

            # Step 5: 权益快照
            market_values = self._calc_positions_value(active_events, current_time)
            total_positions_value = sum(market_values.values()) if market_values else 0.0
            self.portfolio.snapshot(current_time, total_positions_value)

            # 日志
            if step_count % 10 == 0 or settled > 0:
                equity = self.portfolio.equity_curve[-1].equity
                logger.info(
                    "Step %d | %s | cash=%.2f equity=%.2f positions=%d settled=%d",
                    step_count,
                    current_time.strftime("%m-%d %H:%M"),
                    self.portfolio.cash,
                    equity,
                    self.portfolio.get_open_position_count(),
                    settled,
                )

            # 推进时间
            current_time += timedelta(hours=self.config.step_hours)

        # 结算所有剩余持仓
        self._settle_all_remaining(events, end_time)

        # 计算绩效
        metrics = self.metrics.calculate(
            equity_curve=self.portfolio.equity_curve,
            trades=self.portfolio.trades,
            initial_capital=self.config.initial_capital,
            total_hours=total_hours,
        )

        # 构建结果
        result = BacktestResult(
            config=self.config.to_dict(),
            start_time=start_time.isoformat(),
            end_time=end_time.isoformat(),
            total_steps=step_count,
            equity_curve=[p.to_dict() for p in self.portfolio.equity_curve],
            trades=[t.to_dict() for t in self.portfolio.trades],
            open_positions=[p.to_dict() for p in self.portfolio.get_positions_list()],
            **metrics,
        )

        logger.info(
            "回测完成: %d 步, %d 笔交易, ROI=%.2f%%, Sharpe=%.4f, MaxDD=%.2f",
            step_count,
            metrics["total_trades"],
            metrics["roi"],
            metrics["sharpe_ratio"],
            metrics["max_drawdown"],
        )

        return result

    # ================================================================
    # 内部方法
    # ================================================================

    def _settle_expired_events(
        self,
        events: List[HistoricalEvent],
        current_time: datetime,
    ) -> int:
        """结算已到期且有持仓的事件。

        Returns:
            结算数量
        """
        settled = 0
        for event in events:
            if event.deadline > current_time:
                continue  # 未到期
            if event.outcome == EventOutcome.PENDING:
                continue  # 无结果
            if not self.portfolio.has_position(event.token_id):
                continue  # 无持仓

            # 结算
            pos = self.portfolio.positions[event.token_id]
            trade = self.simulator.settle_position(
                token_id=event.token_id,
                direction=pos.direction,
                quantity=pos.quantity,
                entry_price=pos.entry_price,
                event=event,
                settle_time=event.deadline,
                strategy=pos.strategy,
            )
            self.portfolio.settle_position(trade)
            settled += 1

        return settled

    def _get_active_events(
        self,
        events: List[HistoricalEvent],
        current_time: datetime,
    ) -> List[HistoricalEvent]:
        """获取当前活跃的事件 (有市场数据, 未到期, 未被结算)。"""
        active = []
        for event in events:
            # 事件已到期 -> 跳过
            if event.deadline <= current_time:
                continue
            # 无市场数据 -> 跳过
            if not event.market_history:
                continue
            # 已有持仓 -> 不再开新仓 (简化逻辑)
            if self.portfolio.has_position(event.token_id):
                continue

            # 检查是否有当前时间附近的市场数据
            prices = self.simulator.get_market_price(event, current_time)
            if prices is not None:
                active.append(event)

        return active

    async def _generate_signals(
        self,
        events: List[HistoricalEvent],
        current_time: datetime,
    ) -> list:
        """为活跃事件生成交易信号。

        使用现有的 StrategyPipeline 或直接构造信号。
        """
        from ..models import (
            TradingSignal,
            SignalDirection,
            EventType,
            MarketSnapshot,
        )

        signals = []

        for event in events:
            prices = self.simulator.get_market_price(event, current_time)
            if prices is None:
                continue

            yes_price, no_price, spread = prices

            # 构造 MarketSnapshot
            market = MarketSnapshot(
                token_id=event.token_id,
                yes_price=yes_price,
                no_price=no_price,
                spread=spread,
                timestamp=current_time,
            )

            # 简单信号: 基于市场隐含概率 vs 真实概率的偏差
            # (在回测中我们用 actual_data 中的概率作为 "模型概率")
            true_prob = event.actual_data.get("prob", 0.5)
            implied = market.implied_probability
            deviation = true_prob - implied

            # 只在偏差足够大时生成信号
            if abs(deviation) < 0.05:
                continue

            direction = SignalDirection.BUY_YES if deviation > 0 else SignalDirection.BUY_NO
            confidence = min(0.95, abs(deviation) * 3.0)

            signal = TradingSignal(
                strategy="backtest_simple",
                direction=direction,
                token_id=event.token_id,
                event_type=EventType(event.event_type),
                location=event.location,
                target_price=max(0.01, min(0.99, true_prob)),
                market_price=yes_price,
                confidence=confidence,
                deviation=deviation,
                sigma_level=abs(deviation) / 0.1,  # 简化 sigma
                metadata={
                    "true_prob": true_prob,
                    "implied_prob": implied,
                    "hours_to_deadline": (
                        event.deadline - current_time
                    ).total_seconds() / 3600,
                },
            )
            signals.append(signal)

        return signals

    def _aggregate_signals(self, signals: list) -> list:
        """聚合信号为交易指令。"""
        if not signals or not self.config.use_aggregator:
            # 不使用聚合器: 直接构造简单指令
            return self._simple_orders(signals)

        try:
            from ..aggregator.aggregator import SignalAggregator, AggregatorParams
            from ..aggregator.models import RiskState

            params = AggregatorParams(
                base_capital=self.config.initial_capital,
                max_position_per_market=self.config.max_position_size,
            )
            aggregator = SignalAggregator(params)
            risk_state = RiskState()

            return aggregator.aggregate(signals, risk_state)
        except Exception as e:
            logger.warning("聚合器失败, 降级为简单模式: %s", e)
            return self._simple_orders(signals)

    def _simple_orders(self, signals: list) -> list:
        """简单指令构造 (不使用聚合器)。"""
        from ..aggregator.models import AggregatedOrder, OrderStatus
        from ..models import SignalDirection

        orders = []
        for sig in signals:
            # 简单仓位: 基于置信度的固定比例
            fraction = min(0.1, sig.confidence * 0.15)
            qty = fraction * self.config.initial_capital / max(0.01, sig.market_price)

            order = AggregatedOrder(
                token_id=sig.token_id,
                direction=sig.direction,
                event_type=sig.event_type,
                location=sig.location,
                target_price=sig.target_price,
                market_price=sig.market_price,
                quantity=qty,
                confidence=sig.confidence,
                contributing_strategies=[sig.strategy],
                status=OrderStatus.OPEN,
            )
            orders.append(order)

        return orders

    def _execute_orders(
        self,
        orders: list,
        events: List[HistoricalEvent],
        current_time: datetime,
    ) -> None:
        """执行交易指令。"""
        # 建立 token_id -> event 映射
        event_map = {e.token_id: e for e in events}

        for order in orders:
            # 检查是否可执行
            if hasattr(order, "is_actionable") and not order.is_actionable:
                continue

            event = event_map.get(order.token_id)
            if event is None:
                continue

            # 模拟下单
            trade = self.simulator.simulate_order(
                event=event,
                direction=order.direction.value,
                quantity=order.quantity,
                current_time=current_time,
                strategy=",".join(order.contributing_strategies) if hasattr(order, "contributing_strategies") else "backtest",
                confidence=order.confidence,
            )

            if trade is not None:
                self.portfolio.open_position(trade, deadline=event.deadline)

    def _calc_positions_value(
        self,
        events: List[HistoricalEvent],
        current_time: datetime,
    ) -> Dict[str, float]:
        """计算所有持仓的当前市值。"""
        values = {}
        event_map = {e.token_id: e for e in events}

        for token_id, pos in self.portfolio.positions.items():
            event = event_map.get(token_id)
            if event:
                val = self.simulator.get_position_value(
                    direction=pos.direction,
                    quantity=pos.quantity,
                    entry_price=pos.entry_price,
                    event=event,
                    current_time=current_time,
                )
            else:
                val = pos.cost
            values[token_id] = val

        return values if values else {}

    def _settle_all_remaining(
        self,
        events: List[HistoricalEvent],
        end_time: datetime,
    ) -> None:
        """结算所有剩余持仓。"""
        for event in events:
            if not self.portfolio.has_position(event.token_id):
                continue
            if event.outcome == EventOutcome.PENDING:
                continue

            pos = self.portfolio.positions[event.token_id]
            trade = self.simulator.settle_position(
                token_id=event.token_id,
                direction=pos.direction,
                quantity=pos.quantity,
                entry_price=pos.entry_price,
                event=event,
                settle_time=end_time,
                strategy=pos.strategy,
            )
            self.portfolio.settle_position(trade)

        # 对仍无结果的持仓, 按当前市场价平仓
        if self.portfolio.positions:
            logger.warning(
                "有 %d 个持仓无事件结果, 按成本价平仓",
                len(self.portfolio.positions),
            )
            for token_id in list(self.portfolio.positions.keys()):
                pos = self.portfolio.positions[token_id]
                # 按开仓价退回资金
                self.portfolio.cash += pos.cost
                del self.portfolio.positions[token_id]
