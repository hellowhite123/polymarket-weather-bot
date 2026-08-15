"""端到端聚合验证 -- 模拟多策略信号 -> 聚合器 -> 最终指令。

模拟场景:
  市场 1 (NYC 降雨): 预报偏差 + 多源验证都看涨 BUY_YES -> 同方向合并
  市场 2 (LA 温度): 仅预报偏差看跌 BUY_NO -> 单策略直通
  市场 3 (CHI 降雪): 两策略方向冲突但旗鼓相当 -> 相消不交易
  市场 4 (HOU 风速): 高置信度但接近结算 -> 风控拦截
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from datetime import datetime, timedelta, timezone

from strategy_engine.aggregator.aggregator import AggregatorParams, SignalAggregator
from strategy_engine.aggregator.models import RiskState
from strategy_engine.models import EventType, SignalDirection, TradingSignal


def main():
    # ---- 模拟 4 个市场的策略信号 ----
    signals = [
        # 市场 1: NYC 降雨 -- 两策略同方向 BUY_YES
        TradingSignal(
            strategy="forecast_deviation",
            direction=SignalDirection.BUY_YES,
            token_id="cmt_nyc_rain",
            event_type=EventType.RAIN,
            location="New York",
            target_price=0.80,
            market_price=0.60,
            confidence=0.85,
        ),
        TradingSignal(
            strategy="multi_source",
            direction=SignalDirection.BUY_YES,
            token_id="cmt_nyc_rain",
            event_type=EventType.RAIN,
            location="New York",
            target_price=0.82,
            market_price=0.60,
            confidence=0.90,
        ),
        # 市场 2: LA 温度 -- 仅预报偏差 BUY_NO
        TradingSignal(
            strategy="forecast_deviation",
            direction=SignalDirection.BUY_NO,
            token_id="cmt_la_temp",
            event_type=EventType.TEMPERATURE,
            location="Los Angeles",
            target_price=0.25,
            market_price=0.70,
            confidence=0.88,
        ),
        # 市场 3: CHI 降雪 -- 两策略方向冲突, 旗鼓相当
        TradingSignal(
            strategy="forecast_deviation",
            direction=SignalDirection.BUY_YES,
            token_id="cmt_chi_snow",
            event_type=EventType.SNOW,
            location="Chicago",
            target_price=0.55,
            market_price=0.50,
            confidence=0.60,
        ),
        TradingSignal(
            strategy="multi_source",
            direction=SignalDirection.BUY_NO,
            token_id="cmt_chi_snow",
            event_type=EventType.SNOW,
            location="Chicago",
            target_price=0.45,
            market_price=0.50,
            confidence=0.58,
        ),
        # 市场 4: HOU 风速 -- 高置信度但接近结算
        TradingSignal(
            strategy="forecast_deviation",
            direction=SignalDirection.BUY_YES,
            token_id="cmt_hou_wind",
            event_type=EventType.WIND,
            location="Houston",
            target_price=0.85,
            market_price=0.55,
            confidence=0.92,
        ),
    ]

    # ---- 风控状态 ----
    risk_state = RiskState(
        positions={"cmt_la_temp": 200.0},
        settlement_times={
            "cmt_hou_wind": datetime.now(timezone.utc) + timedelta(hours=1),
        },
        daily_pnl=-30.0,
    )

    # ---- 聚合 ----
    params = AggregatorParams(
        base_capital=2000.0,
        kelly_fraction=0.25,
        conflict_threshold=0.15,
        min_aggregated_confidence=0.5,
        settlement_lock_hours=2.0,
        daily_loss_limit=100.0,
        max_position_per_market=2000.0,
        max_global_exposure=5000.0,
    )
    aggregator = SignalAggregator(params)
    orders = aggregator.aggregate(signals, risk_state)

    # ---- 输出 ----
    print("=" * 64)
    print("  信号聚合器 -- 端到端验证")
    print("  输入: 6 个策略信号 (4 个市场) + 风控状态")
    print("=" * 64)

    for i, o in enumerate(orders, 1):
        status_label = {
            "open": "可执行",
            "blocked": "风控拦截",
            "void": "冲突相消",
        }.get(o.status.value, o.status.value)

        print(f"\n  [{i}] {status_label} -- {o.direction.value}")
        print(f"      市场:        {o.token_id}")
        print(f"      事件/位置:    {o.event_type.value} / {o.location}")
        print(f"      目标价格:    {o.target_price:.4f}  (市场: {o.market_price:.4f})")
        print(f"      下单数量:    {o.quantity:.2f}")
        print(f"      综合置信度:  {o.confidence:.4f}")
        contributing = ", ".join(o.contributing_strategies) or "(无)"
        print(f"      贡献策略:    {contributing}")
        print(f"      输入信号数:  {o.input_signal_count}")

        if o.block_reason:
            print(f"      拦截原因:    {o.block_reason}")

        failed = [r for r in o.risk_checks if not r.passed]
        if failed:
            print(f"      风控检查:")
            for r in failed:
                print(f"        - {r.rule}: {r.reason}")

        sz = o.metadata.get("sizing", {})
        if "f_star" in sz:
            print(
                f"      Kelly:       f*={sz['f_star']:.4f} "
                f"b={sz.get('b', 0):.4f} "
                f"f={sz.get('f_applied', 0):.4f}"
            )

    summary = aggregator.get_summary(orders)
    print(f"\n  {'-' * 60}")
    print(
        f"  汇总: {summary['total_tokens']} 个市场 | "
        f"{summary['actionable']} 可执行 | "
        f"{summary['blocked']} 拦截 | "
        f"{summary['conflict_void']} 相消"
    )
    if summary["actionable"]:
        print(
            f"  总下单量: {summary['total_quantity']:.2f} USDC | "
            f"平均置信度: {summary['avg_confidence']:.4f}"
        )
    print("=" * 64)


if __name__ == "__main__":
    main()
