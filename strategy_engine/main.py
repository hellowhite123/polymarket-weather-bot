"""策略引擎 CLI 入口。

用法:
    # 预报偏差策略 (单源)
    python -m strategy_engine.main \\
        --forecast data/raw/noaa_NewYork_*.json \\
        --event-types rain \\
        --target-hours 24 \\
        --mock-bias 0.03 --mock-noise 0.05 --summary

    # 多源验证策略 (NOAA + 彩云)
    python -m strategy_engine.main \\
        --multi-source \\
        --ms-forecast "data/raw/noaa_NewYork.json,data/raw/caiyun_NewYork.json" \\
        --event-types rain \\
        --target-hours 24 --summary

    # 临近修正策略 (实况观测修正预报)
    python -m strategy_engine.main \\
        --nowcast --summary \\
        --forecast data/raw/noaa_NewYork_*.json \\
        --event-types rain --deadline-hours 4 \\
        --precip-deviation 0.3

    # 历史统计策略 (气候学基线 vs 市场)
    python -m strategy_engine.main \\
        --historical --summary \\
        --forecast data/raw/noaa_NewYork_*.json \\
        --event-types rain --target-hours 24

    # 信号聚合 (策略 + 聚合 -> 最终交易指令)
    python -m strategy_engine.main \\
        --aggregate --summary \\
        --forecast data/raw/noaa_NewYork_*.json \\
        --ms-forecast "data/raw/noaa_NewYork.json,data/raw/caiyun_NewYork.json" \\
        --base-capital 2000 --position-sizing kelly

    # 查看追踪器统计
    python -m strategy_engine.main --tracker-stats

    # 查看最近信号
    python -m strategy_engine.main --recent-signals 10
"""

import argparse
import asyncio
import json
import logging
import os
import sys
from typing import List

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from strategy_engine.config import StrategyConfig
from strategy_engine.models import TradingSignal


def setup_logging(verbose: bool = False):
    level = logging.DEBUG if verbose else logging.INFO
    logging.basicConfig(
        level=level,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
        datefmt="%H:%M:%S",
    )
    logging.getLogger("asyncio").setLevel(logging.WARNING)
    logging.getLogger("aiohttp").setLevel(logging.WARNING)


def parse_args():
    p = argparse.ArgumentParser(
        description="策略引擎 -- 预报偏差 / 多源验证 / 临近修正 / 历史统计 / 信号聚合",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
示例:
  # 预报偏差策略
  python -m strategy_engine.main \\
      --forecast data/raw/noaa_NewYork_*.json \\
      --event-types rain --target-hours 24 --summary

  # 多源验证策略
  python -m strategy_engine.main \\
      --multi-source \\
      --ms-forecast "noaa_nyc.json,caiyun_nyc.json" \\
      --event-types rain --target-hours 24 --summary

  # 临近修正策略
  python -m strategy_engine.main \\
      --nowcast --summary \\
      --forecast data/raw/noaa_NewYork_*.json \\
      --event-types rain --deadline-hours 4 --precip-deviation 0.3

  # 历史统计策略
  python -m strategy_engine.main \\
      --historical --summary \\
      --forecast data/raw/noaa_NewYork_*.json \\
      --event-types rain --target-hours 24

  # 查看追踪器统计
  python -m strategy_engine.main --tracker-stats

  # 信号聚合 (策略 -> 聚合 -> 最终交易指令)
  python -m strategy_engine.main \\
      --aggregate --summary \\
      --forecast data/raw/noaa_NewYork_*.json \\
      --ms-forecast "noaa_nyc.json,caiyun_nyc.json" \\
      --base-capital 2000 --position-sizing kelly
        """,
    )

    # --- 单源模式 ---
    p.add_argument(
        "--forecast", nargs="+",
        help="WeatherForecast JSON 文件路径 (单源模式, 支持通配符)",
    )

    # --- 多源模式 ---
    p.add_argument(
        "--multi-source", action="store_true",
        help="启用多源验证策略模式",
    )
    p.add_argument(
        "--ms-forecast", action="append", default=[],
        help="多源预报组 (逗号分隔的文件列表, 可重复指定多组)",
    )

    # --- 通用参数 ---
    p.add_argument(
        "--event-types", nargs="+", default=["rain"],
        choices=["rain", "temperature", "snow", "wind", "humidity"],
        help="事件类型 (与预报组一一对应)",
    )
    p.add_argument(
        "--target-hours", type=int, default=24,
        help="目标时间窗口 (小时, 默认 24)",
    )

    # Mock 市场参数
    p.add_argument(
        "--mock-bias", type=float, default=0.0,
        help="Mock 市场系统性偏差 (正=高估, 负=低估)",
    )
    p.add_argument(
        "--mock-noise", type=float, default=0.05,
        help="Mock 市场噪声标准差",
    )
    p.add_argument(
        "--mock-only", action="store_true",
        help="仅模拟市场数据, 不运行策略",
    )

    # 策略参数
    p.add_argument("--sigma", type=float, default=2.0, help="偏差阈值 (sigma 倍数)")
    p.add_argument("--min-deviation", type=float, default=0.02, help="最小绝对偏差")
    p.add_argument("--window-size", type=int, default=100, help="追踪器窗口大小")

    # 多源策略参数
    p.add_argument(
        "--div-sigma", type=float, default=2.0,
        help="源间分歧阈值 (sigma 倍数)",
    )
    p.add_argument(
        "--min-divergence", type=float, default=0.05,
        help="最小源间分歧 (绝对值)",
    )
    p.add_argument(
        "--div-conf-boost", type=float, default=0.3,
        help="分歧置信度放大因子 (0~1)",
    )

    # 临近修正策略参数
    p.add_argument(
        "--nowcast", action="store_true",
        help="启用临近修正策略模式 (实况观测修正预报)",
    )
    p.add_argument(
        "--observation", nargs="+",
        help="RadarObservation JSON 文件路径 (不指定则用 Mock)",
    )
    p.add_argument(
        "--deadline-hours", type=float, default=4.0,
        help="事件截止时间 (从现在起 N 小时后, 默认 4)",
    )
    p.add_argument(
        "--precip-deviation", type=float, default=0.0,
        help="Mock 观测降水偏差 (正值=比预报更湿, 负值=更干)",
    )

    # 历史统计策略参数
    p.add_argument(
        "--historical", action="store_true",
        help="启用历史统计策略模式 (气候学基线 vs 市场)",
    )
    p.add_argument(
        "--historical-data",
        help="历史气候观测 JSON 文件 (不指定则用 Mock 生成)",
    )

    # 聚合器参数
    p.add_argument(
        "--aggregate", action="store_true",
        help="启用信号聚合 (运行策略 + 聚合为最终指令)",
    )
    p.add_argument(
        "--position-sizing", choices=["kelly", "fixed_fraction"],
        default="kelly", help="仓位计算模式 (默认 kelly)",
    )
    p.add_argument(
        "--base-capital", type=float, default=1000.0,
        help="基础资金 (USDC)",
    )
    p.add_argument(
        "--settlement-hours", type=float, default=2.0,
        help="结算前 N 小时禁开仓 (风控模拟)",
    )
    p.add_argument(
        "--daily-pnl", type=float, default=0.0,
        help="当日盈亏 (USDC, 风控模拟, 负数=亏损)",
    )

    # gRPC 发送参数
    p.add_argument(
        "--grpc", action="store_true",
        help="将聚合指令通过 gRPC 发送到 Rust 执行层",
    )
    p.add_argument(
        "--grpc-endpoint", default="127.0.0.1:50051",
        help="Rust gRPC 服务地址 (TCP: 127.0.0.1:50051, UDS: unix:///tmp/polymarket.sock)",
    )
    p.add_argument(
        "--grpc-timeout", type=float, default=10.0,
        help="gRPC RPC 超时 (秒)",
    )

    # 输出
    p.add_argument("--summary", action="store_true", help="输出摘要")
    p.add_argument("--json", action="store_true", help="JSON 格式输出信号")
    p.add_argument("--no-save", action="store_true", help="不保存信号文件")

    # 查询
    p.add_argument("--tracker-stats", action="store_true", help="查看追踪器统计")
    p.add_argument("--recent-signals", type=int, help="查看最近 N 条信号")

    # 回测
    p.add_argument(
        "--backtest", action="store_true",
        help="运行回测 (模拟历史事件 -> 策略 -> 模拟交易 -> 绩效统计)",
    )
    p.add_argument(
        "--bt-events", type=int, default=50,
        help="回测事件数量 (默认 50)",
    )
    p.add_argument(
        "--bt-days", type=int, default=90,
        help="回测时间跨度 (天, 默认 90)",
    )
    p.add_argument(
        "--bt-capital", type=float, default=10000.0,
        help="回测初始资金 (USDC, 默认 10000)",
    )
    p.add_argument(
        "--bt-aggregator", action="store_true",
        help="回测中使用信号聚合器",
    )
    p.add_argument(
        "--bt-save", action="store_true",
        help="保存回测结果到文件 (JSON + CSV)",
    )

    p.add_argument("-v", "--verbose", action="store_true", help="详细日志")
    return p.parse_args()


# ================================================================
# 命令处理
# ================================================================


async def cmd_run(args):
    """单源模式: 预报偏差策略。"""
    from strategy_engine.pipeline import StrategyPipeline

    forecast_files = _expand_files(args.forecast or [])
    if not forecast_files:
        print("错误: 未指定预报文件", file=sys.stderr)
        sys.exit(1)

    config = StrategyConfig.from_env()
    config.deviation.deviation_threshold_sigma = args.sigma
    config.deviation.min_deviation_abs = args.min_deviation
    config.deviation.window_size = args.window_size

    pipeline = StrategyPipeline(config)

    if args.mock_only:
        forecasts = pipeline._load_forecasts(forecast_files)
        from strategy_engine.models import EventType
        event_types = [EventType(et) for et in args.event_types]
        markets = pipeline._generate_mock_markets(
            forecasts, event_types, args.target_hours,
            bias=args.mock_bias, noise=args.mock_noise,
        )
        for i, (fc, mk) in enumerate(zip(forecasts, markets)):
            print(f"\n--- {fc.location_name} ({event_types[i].value}) ---")
            print(f"  YES 价格:   {mk.yes_price:.4f}")
            print(f"  NO 价格:    {mk.no_price:.4f}")
            print(f"  隐含概率:   {mk.implied_probability:.4f}")
            print(f"  买卖价差:   {mk.spread:.4f}")
        return

    signals = await pipeline.run(
        forecast_files=forecast_files,
        event_types=args.event_types,
        target_hours=args.target_hours,
        market_mode="mock",
        mock_bias=args.mock_bias,
        mock_noise=args.mock_noise,
        output_signals=not args.no_save,
    )

    _output_signals(signals, args, pipeline, mode="deviation")


async def cmd_run_multi_source(args):
    """多源模式: 多源验证策略。"""
    from strategy_engine.pipeline import StrategyPipeline

    # 解析多源预报组
    multi_forecast_files = []
    for group_str in args.ms_forecast:
        files = [f.strip() for f in group_str.split(",") if f.strip()]
        expanded = _expand_files(files)
        if len(expanded) >= 2:
            multi_forecast_files.append(expanded)
        else:
            print(
                f"警告: 跳过预报组 (需 >=2 个文件, 实际 {len(expanded)})",
                file=sys.stderr,
            )

    if not multi_forecast_files:
        print("错误: 无有效的多源预报组", file=sys.stderr)
        print("用法: --ms-forecast \"file1.json,file2.json\" [...]")
        sys.exit(1)

    config = StrategyConfig.from_env()
    config.multi_source.divergence_threshold_sigma = args.div_sigma
    config.multi_source.min_divergence_abs = args.min_divergence
    config.multi_source.divergence_confidence_boost = args.div_conf_boost
    config.multi_source.window_size = args.window_size

    pipeline = StrategyPipeline(config)

    signals = await pipeline.run_multi_source(
        multi_forecast_files=multi_forecast_files,
        event_types=args.event_types,
        target_hours=args.target_hours,
        market_mode="mock",
        mock_bias=args.mock_bias,
        mock_noise=args.mock_noise,
        output_signals=not args.no_save,
    )

    _output_signals(signals, args, pipeline, mode="multi_source")


# ================================================================
# 临近修正模式
# ================================================================


async def cmd_run_nowcast(args):
    """临近修正模式: 实况观测修正预报概率。"""
    from strategy_engine.pipeline import StrategyPipeline

    forecast_files = _expand_files(args.forecast or [])
    if not forecast_files:
        print("错误: 未指定预报文件", file=sys.stderr)
        sys.exit(1)

    config = StrategyConfig.from_env()

    pipeline = StrategyPipeline(config)

    observation_files = _expand_files(args.observation or []) if args.observation else None

    signals = await pipeline.run_nowcast(
        forecast_files=forecast_files,
        observation_files=observation_files,
        event_types=args.event_types,
        target_hours=args.target_hours,
        event_deadline_hours=args.deadline_hours,
        market_mode="mock",
        mock_bias=args.mock_bias,
        mock_noise=args.mock_noise,
        mock_precip_deviation=args.precip_deviation,
        output_signals=not args.no_save,
    )

    _output_signals(signals, args, pipeline, mode="nowcast")


# ================================================================
# 历史统计模式
# ================================================================


async def cmd_run_historical(args):
    """历史统计模式: 气候学基线 vs 市场隐含概率。"""
    from strategy_engine.pipeline import StrategyPipeline

    forecast_files = _expand_files(args.forecast or [])
    if not forecast_files:
        print("错误: 未指定预报文件", file=sys.stderr)
        sys.exit(1)

    config = StrategyConfig.from_env()

    pipeline = StrategyPipeline(config)

    signals = await pipeline.run_historical(
        forecast_files=forecast_files,
        historical_data_file=args.historical_data,
        event_types=args.event_types,
        target_hours=args.target_hours,
        market_mode="mock",
        mock_bias=args.mock_bias,
        mock_noise=args.mock_noise,
        output_signals=not args.no_save,
    )

    _output_signals(signals, args, pipeline, mode="historical")


# ================================================================
# 信号聚合模式
# ================================================================


async def cmd_aggregate(args):
    """聚合模式: 运行策略 + 信号聚合 -> 最终交易指令。"""
    from strategy_engine.pipeline import StrategyPipeline
    from strategy_engine.aggregator.models import RiskState

    forecast_files = _expand_files(args.forecast or [])
    multi_forecast_files = []
    for group_str in args.ms_forecast:
        files = [f.strip() for f in group_str.split(",") if f.strip()]
        expanded = _expand_files(files)
        if len(expanded) >= 2:
            multi_forecast_files.append(expanded)

    if not forecast_files and not multi_forecast_files:
        print(
            "错误: 聚合模式需要 --forecast 或 --ms-forecast",
            file=sys.stderr,
        )
        sys.exit(1)

    config = StrategyConfig.from_env()
    config.aggregator.position_sizing = args.position_sizing
    config.aggregator.base_capital = args.base_capital
    config.aggregator.settlement_lock_hours = args.settlement_hours
    config.deviation.deviation_threshold_sigma = args.sigma
    config.deviation.min_deviation_abs = args.min_deviation
    config.deviation.window_size = args.window_size
    config.multi_source.divergence_threshold_sigma = args.div_sigma
    config.multi_source.min_divergence_abs = args.min_divergence
    config.multi_source.divergence_confidence_boost = args.div_conf_boost
    config.multi_source.window_size = args.window_size

    pipeline = StrategyPipeline(config)

    risk_state = RiskState(daily_pnl=args.daily_pnl)

    # 如果指定了 --nowcast，将预报文件也用于临近修正
    nowcast_forecast_files = forecast_files if args.nowcast else None
    nowcast_observation_files = (
        _expand_files(args.observation or []) if args.observation else None
    )

    # 如果指定了 --historical，将预报文件也用于历史统计
    historical_forecast_files = forecast_files if args.historical else None

    orders = await pipeline.run_aggregated(
        forecast_files=forecast_files or None,
        multi_forecast_files=multi_forecast_files or None,
        nowcast_forecast_files=nowcast_forecast_files,
        nowcast_observation_files=nowcast_observation_files,
        event_deadline_hours=args.deadline_hours,
        mock_precip_deviation=args.precip_deviation,
        historical_forecast_files=historical_forecast_files,
        historical_data_file=args.historical_data,
        event_types=args.event_types,
        target_hours=args.target_hours,
        market_mode="mock",
        mock_bias=args.mock_bias,
        mock_noise=args.mock_noise,
        risk_state=risk_state,
        output_orders=not args.no_save,
    )

    _output_orders(orders, args, pipeline)

    # gRPC 发送到 Rust 执行层
    if args.grpc and orders:
        print(f"\n{'='*60}")
        print("  gRPC 发送 -> Rust 执行层")
        print(f"{'='*60}")
        try:
            results = await pipeline.send_to_rust(
                orders,
                endpoint=args.grpc_endpoint,
                timeout=args.grpc_timeout,
            )
            for r in results:
                status_icon = "[OK]  " if r["success"] else "[FAIL]"
                print(
                    f"  {status_icon} {r['order_id']} "
                    f"-> filled={r['filled_size']:.2f}@{r['filled_price']:.4f} "
                    f"({r['status_name']})"
                )
                if r["error_message"]:
                    print(f"         -> {r['error_message']}")
            success_n = sum(1 for r in results if r["success"])
            print(f"\n  发送完成: {success_n}/{len(results)} 成功")
        except Exception as e:
            print(f"\n  gRPC 发送失败: {e}", file=sys.stderr)


def _output_orders(orders, args, pipeline):
    """输出聚合指令。"""
    if args.json:
        print(
            json.dumps(
                [o.to_dict() for o in orders],
                indent=2, ensure_ascii=False,
            )
        )
    elif args.summary:
        _print_order_summary(orders, pipeline)
    elif orders:
        for o in orders:
            status = {
                "open": "[OK]  ",
                "blocked": "[BLOCK]",
                "void": "[VOID] ",
            }.get(o.status.value, "[?]   ")
            print(
                f"{status} {o.direction.value} {o.token_id[:20]} "
                f"qty={o.quantity:.2f} target={o.target_price:.4f} "
                f"conf={o.confidence:.3f} "
                f"src={','.join(o.contributing_strategies)}"
            )
            if o.block_reason:
                print(f"         -> {o.block_reason}")
    else:
        print("无聚合指令输出")

    summary = pipeline.aggregator.get_summary(orders)
    print(
        f"\n聚合摘要: {summary['total_tokens']} 个市场 "
        f"| {summary['actionable']} 可执行 "
        f"| {summary['blocked']} 风控拦截 "
        f"| {summary['conflict_void']} 冲突相消"
    )
    if summary["actionable"]:
        print(
            f"  总下单量: {summary['total_quantity']:.2f} "
            f"| 平均置信度: {summary['avg_confidence']:.3f}"
        )


def _print_order_summary(orders, pipeline):
    """打印人类可读的聚合指令摘要。"""
    print(f"\n{'='*60}")
    print("  信号聚合器 -- 最终交易指令")
    print(f"{'='*60}")

    if not orders:
        print("\n  无聚合指令 (无有效信号输入)")
        print(f"{'='*60}\n")
        return

    for i, o in enumerate(orders, 1):
        status_label = {
            "open": "可执行",
            "blocked": "风控拦截",
            "void": "冲突相消",
        }.get(o.status.value, o.status.value)

        print(f"\n  [{i}] {status_label} -- {o.direction.value}")
        print(f"      Token ID:    {o.token_id}")
        print(f"      事件类型:    {o.event_type.value}")
        print(f"      位置:        {o.location}")
        print(f"      目标价格:    {o.target_price:.4f}")
        print(f"      市场价格:    {o.market_price:.4f}")
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
                f"applied={sz.get('f_applied', 0):.4f}"
            )

    summary = pipeline.aggregator.get_summary(orders)
    print(f"\n  {'-'*56}")
    print(
        f"  可执行: {summary['actionable']}  |  "
        f"拦截: {summary['blocked']}  |  "
        f"相消: {summary['conflict_void']}"
    )
    if summary["actionable"]:
        print(
            f"  总下单量: {summary['total_quantity']:.2f} USDC  |  "
            f"平均置信度: {summary['avg_confidence']:.3f}"
        )
    print(f"{'='*60}\n")


def cmd_tracker_stats(args):
    """查看追踪器统计。"""
    from strategy_engine.pipeline import StrategyPipeline

    config = StrategyConfig.from_env()
    pipeline = StrategyPipeline(config)
    summary = pipeline.get_tracker_summary()

    # 偏差追踪器
    dev = summary["deviation_tracker"]
    print("=" * 60)
    print("  偏差追踪器 (model vs market)")
    print("=" * 60)
    print(f"  总记录数: {dev['total_records']}")
    print(f"  追踪 Key 数: {dev['tracked_keys']}")
    print(f"  累计信号数: {dev['signal_count']}")
    if dev["keys"]:
        print()
        for k in dev["keys"]:
            print(
                f"  [{k['event_type']}] {k['location']} "
                f"token={k['token_id']} "
                f"mu={k['mu']:.6f} sigma={k['sigma']:.6f} n={k['count']}"
            )

    # 分歧追踪器
    div = summary["divergence_tracker"]
    print()
    print("=" * 60)
    print("  分歧追踪器 (source vs source)")
    print("=" * 60)
    print(f"  总记录数: {div['total_records']}")
    print(f"  追踪 Key 数: {div['tracked_keys']}")
    print(f"  累计信号数: {div['signal_count']}")
    if div["keys"]:
        print()
        for k in div["keys"]:
            print(
                f"  [{k['event_type']}] {k['location']} "
                f"sources={k.get('source_pair', '')} "
                f"mu_div={k['mu_divergence']:.6f} "
                f"sigma_div={k['sigma_divergence']:.6f} n={k['count']}"
            )
            if k.get("source_weights"):
                w = ", ".join(
                    f"{s}={v:.2f}" for s, v in k["source_weights"].items()
                )
                print(f"    权重: {w}")

    print("=" * 60)


def cmd_recent_signals(args):
    """查看最近信号。"""
    from strategy_engine.pipeline import StrategyPipeline

    config = StrategyConfig.from_env()
    pipeline = StrategyPipeline(config)
    signals = pipeline.get_recent_signals(limit=args.recent_signals or 10)

    if not signals:
        print("无历史信号记录")
        return

    print(f"=== 最近 {len(signals)} 条信号 ===")
    for s in signals:
        expired = " [过期]" if s.is_expired() else ""
        print(
            f"  {s.generated_at.strftime('%m-%d %H:%M')} "
            f"[{s.strategy}] {s.direction.value} {s.token_id[:16]} "
            f"dev={s.deviation:+.4f} sigma={s.sigma_level:.1f} "
            f"conf={s.confidence:.3f}{expired}"
        )


# ================================================================
# 工具函数
# ================================================================


def _expand_files(patterns: List[str]) -> List[str]:
    """展开通配符模式为文件列表。"""
    import glob as glob_module
    files = []
    for pattern in patterns:
        matches = glob_module.glob(pattern, recursive=True)
        if matches:
            files.extend(sorted(matches))
        else:
            files.append(pattern)
    return files


def _output_signals(signals, args, pipeline, mode="deviation"):
    """统一信号输出。"""
    if args.json:
        print(json.dumps([s.to_dict() for s in signals], indent=2, ensure_ascii=False))
    elif args.summary:
        _print_summary(signals, pipeline, mode)
    elif signals:
        for s in signals:
            extra = ""
            if mode == "multi_source" and s.metadata:
                src_probs = s.metadata.get("source_probs", {})
                div = s.metadata.get("divergence", 0)
                extra = (
                    f" sources={{{','.join(f'{k}={v:.2f}' for k,v in src_probs.items())}}} "
                    f"div={div:.4f}"
                )
            elif mode == "nowcast" and s.metadata:
                fc = s.metadata.get("forecast_prob", 0)
                obs = s.metadata.get("obs_signal", 0)
                corr = s.metadata.get("correction", 0)
                tw = s.metadata.get("time_weight", 0)
                extra = (
                    f" fc={fc:.2f} obs={obs:.2f} corr={corr:+.4f} tw={tw:.2f}"
                )
            elif mode == "historical" and s.metadata:
                fp = s.metadata.get("forecast_prob", 0)
                cl = s.metadata.get("climatology_prob", 0)
                bp = s.metadata.get("blended_prob", 0)
                sd = s.metadata.get("seasonal_deviation", 0)
                extra = (
                    f" fc={fp:.2f} clim={cl:.2f} blend={bp:.2f} seasonal={sd:+.4f}"
                )
            print(
                f"[{s.strategy}] {s.direction.value} {s.token_id[:16]} "
                f"target={s.target_price:.4f} market={s.market_price:.4f} "
                f"dev={s.deviation:+.4f} sigma={s.sigma_level:.1f} "
                f"conf={s.confidence:.3f}{extra}"
            )

    if not signals:
        print("未生成交易信号 (偏差/分歧在正常范围内)")


def _print_summary(signals, pipeline, mode="deviation"):
    """打印人类可读摘要。"""
    title = {
        "multi_source": "多源验证策略",
        "nowcast": "临近修正策略",
        "historical": "历史统计策略",
    }.get(mode, "预报偏差策略")
    print(f"\n{'='*60}")
    print(f"  策略引擎 -- {title}运行报告")
    print(f"{'='*60}")

    summary = pipeline.get_tracker_summary()
    dev = summary["deviation_tracker"]
    div = summary["divergence_tracker"]

    print(f"\n  追踪器状态:")
    print(f"    偏差追踪器: {dev['total_records']} 条记录, {dev['tracked_keys']} 个 key")
    if mode == "multi_source":
        print(f"    分歧追踪器: {div['total_records']} 条记录, {div['tracked_keys']} 个 key")

    if not signals:
        print(f"\n  无异常信号 -- 当前偏差/分歧均在正常范围内")
        print(f"{'='*60}\n")
        return

    print(f"\n  异常信号 ({len(signals)} 条)")
    print(f"  {'-'*56}")
    for i, s in enumerate(signals, 1):
        print(f"\n  [{i}] {s.direction.value}")
        print(f"      事件类型:  {s.event_type.value}")
        print(f"      位置:      {s.location}")
        print(f"      Token ID:  {s.token_id}")
        print(f"      目标价格:  {s.target_price:.4f}  (融合概率)")
        print(f"      市场价格:  {s.market_price:.4f}  (YES price)")
        print(f"      偏差:      {s.deviation:+.4f}  ({s.sigma_level:.1f}sigma)")
        print(f"      置信度:    {s.confidence:.3f}")
        print(f"      建议数量:  {s.quantity:.2f}")

        if s.metadata:
            if mode == "multi_source":
                src_probs = s.metadata.get("source_probs", {})
                if src_probs:
                    probs_str = "  ".join(f"{k}={v:.2f}" for k, v in src_probs.items())
                    print(f"      源概率:    {probs_str}")
                weights = s.metadata.get("source_weights", {})
                if weights:
                    w_str = "  ".join(f"{k}={v:.2f}" for k, v in weights.items())
                    print(f"      源权重:    {w_str}")
                div_val = s.metadata.get("divergence", 0)
                div_sig = s.metadata.get("divergence_sigma", 0)
                div_ext = s.metadata.get("divergence_is_extreme", False)
                print(f"      源间分歧:  {div_val:.4f} ({div_sig:.2f}sigma{' 异常' if div_ext else ''})")
            elif mode == "nowcast":
                fc_prob = s.metadata.get("forecast_prob")
                obs_sig = s.metadata.get("obs_signal")
                corr = s.metadata.get("correction")
                corrected = s.metadata.get("corrected_prob")
                tw = s.metadata.get("time_weight")
                hrs = s.metadata.get("hours_to_deadline")
                if fc_prob is not None:
                    print(f"      预报概率:  {fc_prob:.4f}")
                if obs_sig is not None:
                    print(f"      实况信号:  {obs_sig:.4f}")
                if corr is not None:
                    print(f"      修正值:    {corr:+.4f}")
                if corrected is not None:
                    print(f"      修正概率:  {corrected:.4f}")
                if tw is not None:
                    print(f"      时间权重:  {tw:.4f}")
                if hrs is not None:
                    print(f"      距截止:    {hrs:.1f}h")
            elif mode == "historical":
                fp = s.metadata.get("forecast_prob")
                cl = s.metadata.get("climatology_prob")
                cs = s.metadata.get("climatology_std")
                cc = s.metadata.get("climatology_count")
                bp = s.metadata.get("blended_prob")
                fw = s.metadata.get("forecast_weight")
                sd = s.metadata.get("seasonal_deviation")
                mo = s.metadata.get("month")
                if fp is not None:
                    print(f"      预报概率:  {fp:.4f}")
                if cl is not None:
                    print(f"      气候概率:  {cl:.4f}")
                if cs is not None:
                    print(f"      气候sigma: {cs:.4f}")
                if cc is not None:
                    print(f"      样本数:    {cc}")
                if bp is not None:
                    print(f"      混合概率:  {bp:.4f}")
                if fw is not None:
                    print(f"      预报权重:  {fw:.4f}")
                if sd is not None:
                    print(f"      季节偏差:  {sd:+.4f}")
                if mo is not None:
                    print(f"      月份:      {mo}")
            mu = s.metadata.get("mu") or s.metadata.get("dev_mu")
            sigma = s.metadata.get("sigma") or s.metadata.get("dev_sigma")
            if mu is not None:
                print(f"      历史均值:  {mu}")
            if sigma is not None:
                print(f"      历史sigma: {sigma}")

    print(f"\n{'='*60}")
    buy_yes = sum(1 for s in signals if s.direction.value == "BUY_YES")
    buy_no = sum(1 for s in signals if s.direction.value == "BUY_NO")
    print(f"  BUY_YES: {buy_yes}  |  BUY_NO: {buy_no}")
    avg_conf = sum(s.confidence for s in signals) / len(signals)
    max_sigma = max(s.sigma_level for s in signals)
    print(f"  平均置信度: {avg_conf:.3f}  |  最大偏差: {max_sigma:.1f}sigma")
    print(f"{'='*60}\n")


# ================================================================
# 回测模式
# ================================================================


async def cmd_backtest(args):
    """回测模式: 模拟历史事件 -> 策略 -> 模拟交易 -> 绩效统计。"""
    from strategy_engine.backtest import (
        BacktestConfig,
        BacktestEngine,
        BacktestReporter,
        DataLoader,
    )

    config = BacktestConfig(
        initial_capital=args.bt_capital,
        use_aggregator=args.bt_aggregator,
        seed=42,
        output_path="./data/backtest",
    )

    engine = BacktestEngine(config)

    # 生成模拟历史事件
    loader = DataLoader(seed=42)
    events = loader.generate_synthetic_events(
        n_events=args.bt_events,
        days_span=args.bt_days,
    )

    print(f"\n  回测配置: {args.bt_events} 个事件, {args.bt_days} 天跨度, "
          f"初始资金 ${args.bt_capital:,.2f}")
    print(f"  聚合器: {'启用' if args.bt_aggregator else '禁用'}")
    print()

    # 运行回测
    result = await engine.run(events)

    # 打印报告
    reporter = BacktestReporter()
    reporter.print_summary(result)

    # 保存结果
    if args.bt_save:
        import os
        os.makedirs("./data/backtest", exist_ok=True)
        reporter.save_json(result, "data/backtest/backtest_result.json")
        reporter.save_equity_curve_csv(result, "data/backtest/equity_curve.csv")
        reporter.save_trades_csv(result, "data/backtest/trades.csv")
        print("  结果已保存到 data/backtest/")


# ================================================================
# 入口
# ================================================================


def main():
    args = parse_args()
    setup_logging(args.verbose)

    if args.tracker_stats:
        cmd_tracker_stats(args)
    elif args.recent_signals:
        cmd_recent_signals(args)
    elif args.backtest:
        asyncio.run(cmd_backtest(args))
    elif args.aggregate:
        asyncio.run(cmd_aggregate(args))
    elif args.multi_source:
        asyncio.run(cmd_run_multi_source(args))
    elif args.nowcast:
        asyncio.run(cmd_run_nowcast(args))
    elif args.historical:
        asyncio.run(cmd_run_historical(args))
    elif args.forecast or args.mock_only:
        asyncio.run(cmd_run(args))
    else:
        print("未指定操作。请使用 --forecast / --multi-source / --nowcast / --historical / --aggregate / --backtest / --tracker-stats")
        print("运行 --help 查看更多选项")


if __name__ == "__main__":
    main()
