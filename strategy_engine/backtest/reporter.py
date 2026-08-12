"""回测报告生成器。

支持两种输出:
  1. JSON 文件 (完整回测结果, 供后续分析)
  2. 文本摘要 (控制台打印, 人类可读)
"""

import json
import logging
import os
from datetime import datetime
from typing import Dict

from .models import BacktestResult

logger = logging.getLogger(__name__)


class BacktestReporter:
    """回测报告生成器。"""

    def print_summary(self, result: BacktestResult) -> None:
        """打印人类可读的回测摘要。"""
        print()
        print("=" * 64)
        print("  回测报告 -- Polymarket 天气预测市场")
        print("=" * 64)

        # 时间范围
        print()
        print("  [时间范围]")
        print(f"    开始:      {result.start_time}")
        print(f"    结束:      {result.end_time}")
        print(f"    总步数:    {result.total_steps}")

        # 交易统计
        print()
        print("  [交易统计]")
        print(f"    总交易数:    {result.total_trades}")
        print(f"    开仓:        {result.open_trades}")
        print(f"    结算:        {result.settle_trades}")
        print(f"    盈利:        {result.winning_trades}")
        print(f"    亏损:        {result.losing_trades}")
        print(f"    胜率:        {result.win_rate:.1f}%")

        # 盈亏
        print()
        print("  [盈亏分析]")
        print(f"    初始资金:    ${result.initial_capital:,.2f}")
        print(f"    最终权益:    ${result.final_equity:,.2f}")
        print(f"    总盈亏:      ${result.total_pnl:+,.2f}")
        print(f"    总手续费:    ${result.total_fees:,.2f}")
        print(f"    总滑点:      ${result.total_slippage:,.2f}")

        # 绩效指标
        print()
        print("  [绩效指标]")
        print(f"    收益率:        {result.roi:+.2f}%")
        print(f"    年化收益率:    {result.annualized_roi:+.2f}%")
        print(f"    Sharpe 比率:   {result.sharpe_ratio:.4f}")
        print(f"    Sortino 比率:  {result.sortino_ratio:.4f}")
        print(f"    最大回撤:      ${result.max_drawdown:,.2f} ({result.max_drawdown_pct:.2f}%)")
        print(f"    盈亏比:        {result.profit_factor:.4f}")
        print(f"    平均盈利:      ${result.avg_win:,.2f}")
        print(f"    平均亏损:      ${result.avg_loss:,.2f}")
        print(f"    平均持仓:      {result.avg_hold_hours:.1f} 小时")

        # 按策略分组
        if result.strategy_stats:
            print()
            print("  [策略分组统计]")
            for name, stats in result.strategy_stats.items():
                print(f"    [{name}]")
                print(f"      开仓: {stats['open_count']}  结算: {stats['settle_count']}")
                print(f"      胜: {stats['wins']}  负: {stats['losses']}  胜率: {stats['win_rate']:.1f}%")
                print(f"      盈亏: ${stats['total_pnl']:+,.2f}  成本: ${stats['total_cost']:,.2f}")

        # 权益曲线摘要
        if result.equity_curve:
            print()
            print("  [权益曲线摘要]")
            n = len(result.equity_curve)
            first = result.equity_curve[0]
            last = result.equity_curve[-1]
            peak = max(p["equity"] for p in result.equity_curve)
            trough = min(p["equity"] for p in result.equity_curve)
            print(f"    数据点:     {n}")
            print(f"    起始权益:   ${first['equity']:,.2f}")
            print(f"    最终权益:   ${last['equity']:,.2f}")
            print(f"    峰值:       ${peak:,.2f}")
            print(f"    谷值:       ${trough:,.2f}")

        print()
        print("=" * 64)

    def save_json(
        self,
        result: BacktestResult,
        filepath: str,
    ) -> None:
        """保存完整回测结果到 JSON 文件。"""
        os.makedirs(os.path.dirname(filepath) or ".", exist_ok=True)
        with open(filepath, "w", encoding="utf-8") as f:
            json.dump(result.to_dict(), f, indent=2, ensure_ascii=False)
        logger.info("回测结果已保存: %s", filepath)

    def save_equity_curve_csv(
        self,
        result: BacktestResult,
        filepath: str,
    ) -> None:
        """保存权益曲线到 CSV 文件。"""
        os.makedirs(os.path.dirname(filepath) or ".", exist_ok=True)
        with open(filepath, "w", encoding="utf-8") as f:
            f.write("timestamp,cash,positions_value,equity,open_positions,realized_pnl,unrealized_pnl\n")
            for p in result.equity_curve:
                f.write(
                    f"{p['timestamp']},{p['cash']:.4f},"
                    f"{p['positions_value']:.4f},{p['equity']:.4f},"
                    f"{p['open_positions']},{p['realized_pnl']:.4f},"
                    f"{p['unrealized_pnl']:.4f}\n"
                )
        logger.info("权益曲线已保存: %s", filepath)

    def save_trades_csv(
        self,
        result: BacktestResult,
        filepath: str,
    ) -> None:
        """保存交易记录到 CSV 文件。"""
        os.makedirs(os.path.dirname(filepath) or ".", exist_ok=True)
        with open(filepath, "w", encoding="utf-8") as f:
            f.write("timestamp,action,token_id,direction,quantity,price,cost,fee,pnl,strategy,confidence\n")
            for t in result.trades:
                f.write(
                    f"{t['timestamp']},{t['action']},{t['token_id']},"
                    f"{t['direction']},{t['quantity']:.4f},{t['price']:.4f},"
                    f"{t['cost']:.4f},{t['fee']:.4f},{t['pnl']:.4f},"
                    f"{t['strategy']},{t['confidence']:.4f}\n"
                )
        logger.info("交易记录已保存: %s", filepath)
