"""回测框架单元测试。

测试覆盖:
  1. DataLoader: 模拟历史事件生成
  2. MarketSimulator: 订单成交 + 结算
  3. Portfolio: 开仓 + 结算 + 权益快照
  4. MetricsCalculator: Sharpe / MaxDD / 胜率
  5. BacktestEngine: 端到端回测
"""

import asyncio
import json
import os
import sys
import unittest
from datetime import datetime, timezone, timedelta

# 确保能导入 strategy_engine
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from strategy_engine.backtest import (
    BacktestConfig,
    BacktestEngine,
    BacktestReporter,
    DataLoader,
    MarketSimulator,
    Portfolio,
    MetricsCalculator,
    HistoricalEvent,
    EventOutcome,
    TradeRecord,
    TradeAction,
    Position,
    EquityPoint,
)


class TestDataLoader(unittest.TestCase):
    """测试数据加载器。"""

    def test_generate_synthetic_events(self):
        """测试模拟事件生成。"""
        loader = DataLoader(seed=42)
        events = loader.generate_synthetic_events(
            n_events=20,
            days_span=30,
        )

        self.assertEqual(len(events), 20)

        # 检查事件结构
        for event in events:
            self.assertIsInstance(event.token_id, str)
            self.assertIsInstance(event.location, str)
            self.assertIsInstance(event.event_type, str)
            self.assertIsInstance(event.deadline, datetime)
            self.assertIn(event.outcome, [EventOutcome.YES, EventOutcome.NO])
            self.assertGreater(len(event.market_history), 0)

        # 检查排序
        for i in range(len(events) - 1):
            self.assertLessEqual(events[i].deadline, events[i + 1].deadline)

    def test_market_history_format(self):
        """测试市场快照格式。"""
        loader = DataLoader(seed=42)
        events = loader.generate_synthetic_events(n_events=5)
        event = events[0]

        for snap in event.market_history:
            self.assertIn("timestamp", snap)
            self.assertIn("yes_price", snap)
            self.assertIn("no_price", snap)
            self.assertIn("volume", snap)
            self.assertIn("spread", snap)
            self.assertIn("hours_to_deadline", snap)
            self.assertGreater(snap["yes_price"], 0)
            self.assertLess(snap["yes_price"], 1)

    def test_save_and_load(self):
        """测试保存和加载。"""
        loader = DataLoader(seed=42)
        events = loader.generate_synthetic_events(n_events=5)

        filepath = "data/backtest/test_events.json"
        loader.save_events(events, filepath)

        # 加载回来
        loaded = loader.load_events_from_file(filepath)
        self.assertEqual(len(loaded), 5)
        self.assertEqual(loaded[0].token_id, events[0].token_id)


class TestMarketSimulator(unittest.TestCase):
    """测试市场模拟器。"""

    def setUp(self):
        self.loader = DataLoader(seed=42)
        self.events = self.loader.generate_synthetic_events(n_events=10)
        self.sim = MarketSimulator(slippage_bps=50, fee_bps=20)

    def test_get_market_price(self):
        """测试获取市场价格。"""
        event = self.events[0]
        # 取截止前 24h
        test_time = event.deadline - timedelta(hours=24)
        prices = self.sim.get_market_price(event, test_time)

        self.assertIsNotNone(prices)
        yes_price, no_price, spread = prices
        self.assertGreater(yes_price, 0)
        self.assertLess(yes_price, 1)
        self.assertGreater(no_price, 0)

    def test_simulate_order(self):
        """测试订单成交。"""
        event = self.events[0]
        test_time = event.deadline - timedelta(hours=24)

        trade = self.sim.simulate_order(
            event=event,
            direction="BUY_YES",
            quantity=100.0,
            current_time=test_time,
            strategy="test",
        )

        self.assertIsNotNone(trade)
        self.assertEqual(trade.action, TradeAction.OPEN)
        self.assertEqual(trade.direction, "BUY_YES")
        self.assertEqual(trade.quantity, 100.0)
        self.assertGreater(trade.cost, 0)
        self.assertGreater(trade.fee, 0)
        self.assertGreater(trade.slippage, 0)

    def test_settle_position_win(self):
        """测试赢的结算。"""
        event = self.events[0]
        # 确保事件结果为 YES
        event.outcome = EventOutcome.YES

        trade = self.sim.settle_position(
            token_id=event.token_id,
            direction="BUY_YES",
            quantity=100.0,
            entry_price=0.40,
            event=event,
            settle_time=event.deadline,
        )

        self.assertEqual(trade.action, TradeAction.SETTLE)
        self.assertTrue(trade.metadata["won"])
        # P&L = 100 * (1 - 0.40) - fee = 60 - fee
        self.assertGreater(trade.pnl, 0)
        self.assertAlmostEqual(trade.pnl, 60.0 - trade.fee, places=2)

    def test_settle_position_loss(self):
        """测试输的结算。"""
        event = self.events[0]
        event.outcome = EventOutcome.NO

        trade = self.sim.settle_position(
            token_id=event.token_id,
            direction="BUY_YES",
            quantity=100.0,
            entry_price=0.40,
            event=event,
            settle_time=event.deadline,
        )

        self.assertFalse(trade.metadata["won"])
        # P&L = -100 * 0.40 - fee = -40 - fee
        self.assertLess(trade.pnl, 0)
        self.assertAlmostEqual(trade.pnl, -40.0 - trade.fee, places=2)


class TestPortfolio(unittest.TestCase):
    """测试组合管理器。"""

    def setUp(self):
        self.portfolio = Portfolio(
            initial_capital=10000.0,
            max_open_positions=10,
            max_position_size=1000.0,
        )

    def _make_trade(self, token_id="tok_1", quantity=100, price=0.4):
        return TradeRecord(
            timestamp=datetime.now(timezone.utc),
            action=TradeAction.OPEN,
            token_id=token_id,
            direction="BUY_YES",
            quantity=quantity,
            price=price,
            cost=quantity * price + 1.0,
            fee=1.0,
            slippage=0.5,
            strategy="test",
        )

    def test_open_position(self):
        """测试开仓。"""
        trade = self._make_trade(quantity=100, price=0.4)
        success = self.portfolio.open_position(trade)

        self.assertTrue(success)
        self.assertEqual(self.portfolio.get_open_position_count(), 1)
        self.assertLess(self.portfolio.cash, 10000.0)

    def test_insufficient_funds(self):
        """测试资金不足。"""
        trade = self._make_trade(quantity=100000, price=0.9)
        success = self.portfolio.open_position(trade)
        self.assertFalse(success)

    def test_max_positions(self):
        """测试持仓数量上限。"""
        self.portfolio.max_open_positions = 3
        for i in range(5):
            trade = self._make_trade(token_id=f"tok_{i}", quantity=10, price=0.1)
            self.portfolio.open_position(trade)

        self.assertEqual(self.portfolio.get_open_position_count(), 3)

    def test_settle(self):
        """测试结算。"""
        # 开仓
        trade = self._make_trade(quantity=100, price=0.4)
        self.portfolio.open_position(trade)

        # 结算
        settle_trade = TradeRecord(
            timestamp=datetime.now(timezone.utc),
            action=TradeAction.SETTLE,
            token_id="tok_1",
            direction="BUY_YES",
            quantity=100.0,
            price=1.0,
            cost=0.0,
            pnl=55.0,
        )
        self.portfolio.settle_position(settle_trade)

        self.assertEqual(self.portfolio.get_open_position_count(), 0)
        self.assertAlmostEqual(self.portfolio.realized_pnl, 55.0, places=2)

    def test_equity_snapshot(self):
        """测试权益快照。"""
        trade = self._make_trade(quantity=100, price=0.4)
        self.portfolio.open_position(trade)

        self.portfolio.snapshot(
            datetime.now(timezone.utc),
            positions_value=50.0,
        )

        self.assertEqual(len(self.portfolio.equity_curve), 1)
        point = self.portfolio.equity_curve[0]
        self.assertGreater(point.equity, 0)


class TestMetricsCalculator(unittest.TestCase):
    """测试绩效指标计算。"""

    def test_basic_metrics(self):
        """测试基本指标计算。"""
        now = datetime.now(timezone.utc)

        # 构造权益曲线 (上升)
        equity_curve = []
        for i in range(20):
            equity_curve.append(EquityPoint(
                timestamp=now + timedelta(hours=i),
                cash=10000 + i * 50,
                positions_value=0,
                equity=10000 + i * 50,
                open_positions=0,
                realized_pnl=i * 50,
                unrealized_pnl=0,
            ))

        # 构造交易记录
        trades = []
        for i in range(10):
            # 开仓
            trades.append(TradeRecord(
                timestamp=now + timedelta(hours=i),
                action=TradeAction.OPEN,
                token_id=f"tok_{i}",
                direction="BUY_YES",
                quantity=50,
                price=0.4,
                cost=21,
                fee=1,
                strategy="test",
            ))
            # 结算 (6 赢 4 输)
            won = i < 6
            pnl = 30 if won else -20
            trades.append(TradeRecord(
                timestamp=now + timedelta(hours=i + 1),
                action=TradeAction.SETTLE,
                token_id=f"tok_{i}",
                direction="BUY_YES",
                quantity=50,
                price=1.0 if won else 0.0,
                cost=0,
                fee=1,
                pnl=pnl,
                strategy="test",
            ))

        calc = MetricsCalculator()
        metrics = calc.calculate(
            equity_curve=equity_curve,
            trades=trades,
            initial_capital=10000,
            total_hours=240,
        )

        self.assertEqual(metrics["settle_trades"], 10)
        self.assertEqual(metrics["winning_trades"], 6)
        self.assertEqual(metrics["losing_trades"], 4)
        self.assertAlmostEqual(metrics["win_rate"], 60.0, places=1)
        self.assertGreater(metrics["roi"], 0)
        self.assertGreater(metrics["sharpe_ratio"], 0)
        self.assertGreaterEqual(metrics["max_drawdown"], 0)

    @staticmethod
    def _make_curve(now, step_hours, n=6):
        values = [10000, 10100, 10050, 10200, 10150, 10300][:n]
        return [
            EquityPoint(
                timestamp=now + timedelta(hours=step_hours * i),
                cash=v,
                positions_value=0,
                equity=v,
                open_positions=0,
                realized_pnl=0,
                unrealized_pnl=0,
            )
            for i, v in enumerate(values)
        ]

    def test_sharpe_annualization_follows_sampling(self):
        """R5-007：Sharpe 年化因子应按**真实时间戳**采样间隔缩放，而非硬编码 365×4。"""
        now = datetime.now(timezone.utc)
        calc = MetricsCalculator()

        # 同一权益序列，时间戳分别按 1h / 6h 采样：
        # periods_per_year 之比 = 6 → Sharpe 之比 = sqrt(6)
        c1 = self._make_curve(now, 1)
        c6 = self._make_curve(now, 6)
        m1 = calc.calculate(c1, [], 10000, total_hours=0)
        m6 = calc.calculate(c6, [], 10000, total_hours=0)

        self.assertGreater(m1["sharpe_ratio"], 0)
        self.assertAlmostEqual(
            m1["sharpe_ratio"] / m6["sharpe_ratio"],
            6 ** 0.5,
            places=3,
        )

    def test_periods_per_year_uses_real_timestamps_not_total_hours(self):
        """年化因子必须由真实 timestamp 反推；`total_hours`（窗口跨度）不得污染。

        `total_hours` 生产定义 = 回测窗口首尾跨度（engine.py），与 equity 曲线首尾不同，
        且 n 个点只有 n-1 个间隔。此处验证：无论 total_hours 传何值，结果一致。
        """
        now = datetime.now(timezone.utc)
        calc = MetricsCalculator()
        curve = self._make_curve(now, step_hours=6)  # 6 点、间隔 6h、跨度 30h
        expected = (365.25 * 24) / 6

        self.assertAlmostEqual(
            calc._resolve_periods_per_year(curve, total_hours=0), expected, places=6
        )
        # 误导性 total_hours 也不应改变结果（证明以时间戳为准，而非 total_hours/n）
        self.assertAlmostEqual(
            calc._resolve_periods_per_year(curve, total_hours=999), expected, places=6
        )

    def test_periods_per_year_fallback_uses_n_minus_1(self):
        """时间戳不可用时回退 `total_hours/(n-1)`（旧实现误用 /n，低估间隔、放大年化）。"""
        now = datetime.now(timezone.utc)
        calc = MetricsCalculator()
        n = 6
        curve = [
            EquityPoint(
                timestamp=now,  # 全部相同 → 跨度 0 → 触发回退
                cash=10000 + i,
                positions_value=0,
                equity=10000 + i,
                open_positions=0,
                realized_pnl=0,
                unrealized_pnl=0,
            )
            for i in range(n)
        ]
        total_hours = 30.0
        ppy = calc._resolve_periods_per_year(curve, total_hours=total_hours)
        # 间隔 = 30/(6-1) = 6h（旧实现为 30/6 = 5h）
        self.assertAlmostEqual(
            ppy, (365.25 * 24) / (total_hours / (n - 1)), places=6
        )

    def test_avg_hold_time_same_token_multiple_rounds(self):
        """R5-008：同一 token 多次开平应按 FIFO 一一配对，而非覆盖错配。"""
        now = datetime.now(timezone.utc)

        def rec(hours, action):
            return TradeRecord(
                timestamp=now + timedelta(hours=hours),
                action=action,
                token_id="tok",
                direction="BUY_YES",
                quantity=10,
                price=0.4,
                cost=4,
            )

        trades = [
            rec(0, TradeAction.OPEN),
            rec(10, TradeAction.OPEN),
            rec(12, TradeAction.SETTLE),
            rec(20, TradeAction.SETTLE),
        ]
        metrics = MetricsCalculator().calculate([], trades, 10000, total_hours=20)
        # FIFO：SETTLE@12 ↔ OPEN@0 (12h)，SETTLE@20 ↔ OPEN@10 (10h) → 平均 11h。
        # 旧实现会覆盖 OPEN，只剩 2h 的错误结果。
        self.assertAlmostEqual(metrics["avg_hold_hours"], 11.0, places=1)

    def test_max_drawdown(self):
        """测试最大回撤计算。"""
        now = datetime.now(timezone.utc)

        # 先涨后跌再涨
        values = [10000, 11000, 12000, 9000, 9500, 11000]
        equity_curve = [
            EquityPoint(
                timestamp=now + timedelta(hours=i),
                cash=v,
                positions_value=0,
                equity=v,
                open_positions=0,
                realized_pnl=0,
                unrealized_pnl=0,
            )
            for i, v in enumerate(values)
        ]

        calc = MetricsCalculator()
        _, dd_pct = calc._calc_max_drawdown(equity_curve)

        # 峰值 12000, 谷值 9000, 回撤 = 25%
        self.assertAlmostEqual(dd_pct, 25.0, places=1)


class TestBacktestEngine(unittest.TestCase):
    """测试回测引擎端到端。"""

    def test_full_backtest(self):
        """测试完整回测流程。"""
        config = BacktestConfig(
            initial_capital=10000.0,
            step_hours=6.0,
            slippage_bps=50.0,
            fee_bps=20.0,
            max_open_positions=20,
            max_position_size=1000.0,
            use_aggregator=False,
            seed=42,
        )

        engine = BacktestEngine(config)

        # 生成模拟事件
        loader = DataLoader(seed=42)
        events = loader.generate_synthetic_events(
            n_events=30,
            days_span=60,
        )

        # 运行回测
        result = asyncio.run(engine.run(events))

        # 验证结果
        self.assertIsNotNone(result)
        self.assertGreater(result.total_steps, 0)
        self.assertGreater(result.total_trades, 0)
        self.assertEqual(result.initial_capital, 10000.0)

        # 应该有盈亏记录
        self.assertIsNotNone(result.final_equity)
        self.assertIsNotNone(result.roi)

        # 应该有权益曲线
        self.assertGreater(len(result.equity_curve), 0)

        # 打印摘要
        reporter = BacktestReporter()
        reporter.print_summary(result)

    def test_backtest_with_aggregator(self):
        """测试使用聚合器的回测。"""
        config = BacktestConfig(
            initial_capital=5000.0,
            step_hours=6.0,
            use_aggregator=True,
            seed=42,
        )

        engine = BacktestEngine(config)

        loader = DataLoader(seed=42)
        events = loader.generate_synthetic_events(
            n_events=20,
            days_span=40,
        )

        result = asyncio.run(engine.run(events))

        self.assertIsNotNone(result)
        self.assertGreater(result.total_steps, 0)

    def test_save_results(self):
        """测试保存结果。"""
        config = BacktestConfig(
            initial_capital=5000.0,
            seed=42,
            output_path="data/backtest",
        )

        engine = BacktestEngine(config)
        loader = DataLoader(seed=42)
        events = loader.generate_synthetic_events(n_events=15, days_span=30)

        result = asyncio.run(engine.run(events))

        reporter = BacktestReporter()

        # 保存 JSON
        json_path = "data/backtest/test_result.json"
        reporter.save_json(result, json_path)
        self.assertTrue(os.path.exists(json_path))

        # 保存 CSV
        csv_path = "data/backtest/test_equity.csv"
        reporter.save_equity_curve_csv(result, csv_path)
        self.assertTrue(os.path.exists(csv_path))

        trades_path = "data/backtest/test_trades.csv"
        reporter.save_trades_csv(result, trades_path)
        self.assertTrue(os.path.exists(trades_path))


if __name__ == "__main__":
    unittest.main(verbosity=2)
