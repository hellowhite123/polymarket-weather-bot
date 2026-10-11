"""绩效指标计算。

计算回测的各项绩效指标:
  - ROI / 年化收益率
  - Sharpe / Sortino 比率
  - 最大回撤
  - 胜率 / 盈亏比
  - 平均持仓时间
"""

import logging
import math
from collections import deque
from typing import Deque, Dict, List, Optional, Tuple

from .models import TradeRecord, EquityPoint, TradeAction

logger = logging.getLogger(__name__)


class MetricsCalculator:
    """绩效指标计算器。"""

    def calculate(
        self,
        equity_curve: List[EquityPoint],
        trades: List[TradeRecord],
        initial_capital: float,
        total_hours: float = 0.0,
    ) -> dict:
        """计算全部绩效指标。

        Args:
            equity_curve: 权益曲线
            trades: 交易记录列表
            initial_capital: 初始资金
            total_hours: 回测总时长 (小时, 用于年化)

        Returns:
            包含所有指标的字典
        """
        settle_trades = [t for t in trades if t.action == TradeAction.SETTLE]

        # 基本盈亏
        final_equity = equity_curve[-1].equity if equity_curve else initial_capital
        total_pnl = final_equity - initial_capital
        total_fees = sum(t.fee for t in trades)
        total_slippage = sum(t.slippage for t in trades)

        # ROI
        roi = (total_pnl / initial_capital * 100) if initial_capital > 0 else 0.0

        # 年化 ROI
        if total_hours > 0:
            years = total_hours / (365.25 * 24)
            if years > 0 and final_equity > 0:
                annualized = ((final_equity / initial_capital) ** (1 / years) - 1) * 100
            else:
                annualized = roi
        else:
            annualized = roi

        # Sharpe / Sortino（R5-007：年化因子按实际采样间隔反推）
        periods_per_year = self._resolve_periods_per_year(equity_curve, total_hours)
        sharpe, sortino = self._calc_sharpe_sortino(equity_curve, periods_per_year)

        # 最大回撤
        max_dd, max_dd_pct = self._calc_max_drawdown(equity_curve)

        # 胜率
        wins = [t for t in settle_trades if t.pnl > 0]
        losses = [t for t in settle_trades if t.pnl <= 0]
        win_rate = (len(wins) / len(settle_trades) * 100) if settle_trades else 0.0

        # 盈亏比
        gross_profit = sum(t.pnl for t in wins)
        gross_loss = abs(sum(t.pnl for t in losses))
        profit_factor = (gross_profit / gross_loss) if gross_loss > 0 else float("inf") if gross_profit > 0 else 0.0

        avg_win = (gross_profit / len(wins)) if wins else 0.0
        avg_loss = (gross_loss / len(losses)) if losses else 0.0

        # 平均持仓时间
        avg_hold = self._calc_avg_hold_time(trades)

        # 按策略分组
        strategy_stats = self._calc_strategy_stats(trades, settle_trades)

        return {
            "total_trades": len(trades),
            "open_trades": len([t for t in trades if t.action == TradeAction.OPEN]),
            "settle_trades": len(settle_trades),
            "winning_trades": len(wins),
            "losing_trades": len(losses),
            "initial_capital": initial_capital,
            "final_equity": round(final_equity, 2),
            "total_pnl": round(total_pnl, 2),
            "total_fees": round(total_fees, 2),
            "total_slippage": round(total_slippage, 2),
            "roi": round(roi, 2),
            "annualized_roi": round(annualized, 2),
            "sharpe_ratio": round(sharpe, 4) if not math.isnan(sharpe) else 0.0,
            "sortino_ratio": round(sortino, 4) if not math.isnan(sortino) else 0.0,
            "max_drawdown": round(max_dd, 2),
            "max_drawdown_pct": round(max_dd_pct, 2),
            "win_rate": round(win_rate, 2),
            "profit_factor": round(profit_factor, 4) if profit_factor != float("inf") else 999.0,
            "avg_win": round(avg_win, 2),
            "avg_loss": round(avg_loss, 2),
            "avg_hold_hours": round(avg_hold, 1),
            "strategy_stats": strategy_stats,
        }

    # ================================================================
    # 内部计算方法
    # ================================================================

    def _resolve_periods_per_year(
        self,
        equity_curve: List[EquityPoint],
        total_hours: float,
    ) -> float:
        """由**真实采样间隔**反推年化因子，替代硬编码 `365 × 4`。

        注意：`total_hours` 的生产定义（`engine.py`）是**回测窗口首尾跨度**
        （`events[0].deadline - 48h` → `events[-1].deadline + 1h`），并非
        equity_curve 的首尾跨度；因此**不能**用 `total_hours / len(curve)` 作为
        采样间隔（既混用了窗口与曲线，又把 n 个点当成 n 个间隔——实际只有 n-1 个）。

        取值优先级：
        1. **真实时间戳**：`interval = (last_ts - first_ts) / (n - 1)`（推荐）；
        2. 回退：`interval = total_hours / (n - 1)`（时间戳不可用时，仍按 n-1 个间隔）；
        3. 信息不足（点数 < 2 / 跨度 ≤ 0）时回退旧默认（每 6h 一个采样点）。
        """
        n = len(equity_curve)
        if n >= 2:
            span_hours = (
                equity_curve[-1].timestamp - equity_curve[0].timestamp
            ).total_seconds() / 3600.0
            if span_hours > 0:
                interval_hours = span_hours / (n - 1)
                if interval_hours > 0:
                    return (365.25 * 24) / interval_hours
            if total_hours > 0:
                interval_hours = total_hours / (n - 1)
                if interval_hours > 0:
                    return (365.25 * 24) / interval_hours
        return 365 * 4

    def _calc_sharpe_sortino(
        self,
        equity_curve: List[EquityPoint],
        periods_per_year: float = 365 * 4,  # 假设每天 4 个数据点
    ) -> Tuple[float, float]:
        """计算 Sharpe 和 Sortino 比率。

        基于权益曲线的收益率序列:
          r_t = (equity_t / equity_{t-1}) - 1

        Sharpe = mean(r) / std(r) * sqrt(periods_per_year)
        Sortino = mean(r) / downside_std(r) * sqrt(periods_per_year)
        """
        if len(equity_curve) < 2:
            return 0.0, 0.0

        returns = []
        for i in range(1, len(equity_curve)):
            prev = equity_curve[i - 1].equity
            curr = equity_curve[i].equity
            if prev > 0:
                returns.append((curr / prev) - 1.0)

        if not returns:
            return 0.0, 0.0

        n = len(returns)
        mean_r = sum(returns) / n

        # 标准差
        var = sum((r - mean_r) ** 2 for r in returns) / n
        std_r = math.sqrt(var) if var > 0 else 0.0

        # 下行标准差
        downside_returns = [r for r in returns if r < 0]
        if downside_returns:
            downside_var = sum(r ** 2 for r in downside_returns) / len(downside_returns)
            downside_std = math.sqrt(downside_var) if downside_var > 0 else 0.0
        else:
            downside_std = 0.0

        # 年化
        sqrt_periods = math.sqrt(periods_per_year)

        sharpe = (mean_r / std_r * sqrt_periods) if std_r > 0 else 0.0
        sortino = (mean_r / downside_std * sqrt_periods) if downside_std > 0 else 0.0

        return sharpe, sortino

    def _calc_max_drawdown(
        self,
        equity_curve: List[EquityPoint],
    ) -> Tuple[float, float]:
        """计算最大回撤。

        Returns:
            (max_drawdown_abs, max_drawdown_pct)
        """
        if not equity_curve:
            return 0.0, 0.0

        peak = equity_curve[0].equity
        max_dd = 0.0
        max_dd_pct = 0.0

        for point in equity_curve:
            if point.equity > peak:
                peak = point.equity
            dd = peak - point.equity
            if dd > max_dd:
                max_dd = dd
                max_dd_pct = (dd / peak * 100) if peak > 0 else 0.0

        return max_dd, max_dd_pct

    def _calc_avg_hold_time(self, trades: List[TradeRecord]) -> float:
        """计算平均持仓时间 (小时)。

        R5-008：同一 token 可能多次开平。旧实现用 `Dict[token_id] = OPEN` 会互相
        覆盖，导致同 token 的多笔开仓只剩最后一笔被匹配、持仓时间错配。
        改为按 token 维护 OPEN 队列、FIFO 匹配最早未平仓的 OPEN，保证一一对应。
        """
        open_queues: Dict[str, Deque[TradeRecord]] = {}
        hold_times = []

        for t in trades:
            if t.action == TradeAction.OPEN:
                open_queues.setdefault(t.token_id, deque()).append(t)
            elif t.action == TradeAction.SETTLE:
                queue = open_queues.get(t.token_id)
                if queue:
                    open_t = queue.popleft()
                    delta = (t.timestamp - open_t.timestamp).total_seconds() / 3600
                    hold_times.append(delta)

        return (sum(hold_times) / len(hold_times)) if hold_times else 0.0

    def _calc_strategy_stats(
        self,
        all_trades: List[TradeRecord],
        settle_trades: List[TradeRecord],
    ) -> Dict[str, dict]:
        """按策略分组统计。"""
        strategy_data: Dict[str, dict] = {}

        for t in all_trades:
            s = t.strategy or "unknown"
            if s not in strategy_data:
                strategy_data[s] = {
                    "open_count": 0,
                    "settle_count": 0,
                    "wins": 0,
                    "losses": 0,
                    "total_pnl": 0.0,
                    "total_cost": 0.0,
                }
            if t.action == TradeAction.OPEN:
                strategy_data[s]["open_count"] += 1
                strategy_data[s]["total_cost"] += t.cost
            elif t.action == TradeAction.SETTLE:
                strategy_data[s]["settle_count"] += 1
                strategy_data[s]["total_pnl"] += t.pnl
                if t.pnl > 0:
                    strategy_data[s]["wins"] += 1
                else:
                    strategy_data[s]["losses"] += 1

        # 计算胜率
        for s, stats in strategy_data.items():
            total = stats["wins"] + stats["losses"]
            stats["win_rate"] = round(
                (stats["wins"] / total * 100) if total > 0 else 0.0, 2
            )
            stats["total_pnl"] = round(stats["total_pnl"], 2)
            stats["total_cost"] = round(stats["total_cost"], 2)

        return strategy_data
