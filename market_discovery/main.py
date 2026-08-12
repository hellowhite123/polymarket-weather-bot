"""市场发现模块 -- CLI 入口。

用法:
    # 单次扫描（拉取 + 增量去重 + 打印新盘）
    python -m market_discovery --run-once

    # 常驻扫描（默认每 10 分钟一次）
    python -m market_discovery --daemon

    # 常驻扫描，自定义间隔 5 分钟，强制 mock 数据
    python -m market_discovery --daemon --interval 300 --mock

    # 查看注册表状态
    python -m market_discovery --status

    # 清空注册表（重新从零发现）
    python -m market_discovery --reset

    # 单次扫描 + 新盘直接跑策略引擎出信号（需 data/raw 有对应城市预报）
    python -m market_discovery --run-once --evaluate

    # 常驻扫描，发现新盘自动评估出信号
    python -m market_discovery --daemon --evaluate
"""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from market_discovery.gamma_client import GammaClient
from market_discovery.models import MarketInfo
from market_discovery.registry import MarketRegistry, DEFAULT_REGISTRY_PATH
from market_discovery.scanner import MarketScanner


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
        description="Polymarket 天气盘市场发现 -- 定时扫描新开的天气盘",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.add_argument("--run-once", action="store_true", help="单次扫描后退出")
    p.add_argument("--daemon", action="store_true", help="常驻扫描")
    p.add_argument(
        "--interval", type=int, default=600,
        help="扫描间隔秒数 (默认 600 = 10 分钟)",
    )
    p.add_argument("--mock", action="store_true", help="强制使用 mock 数据")
    p.add_argument(
        "--registry-path", default=DEFAULT_REGISTRY_PATH,
        help=f"注册表存储路径 (默认 {DEFAULT_REGISTRY_PATH})",
    )
    p.add_argument("--status", action="store_true", help="查看注册表状态")
    p.add_argument("--reset", action="store_true", help="清空注册表")
    p.add_argument(
        "--evaluate", action="store_true",
        help="扫描后把新盘接入策略引擎评估出信号",
    )
    p.add_argument(
        "--forecast-dir", default="./data/raw",
        help="气象预报 JSON 目录 (默认 ./data/raw)",
    )
    p.add_argument("-v", "--verbose", action="store_true", help="详细日志")
    return p.parse_args()


async def cmd_run_once(args) -> int:
    scanner = MarketScanner(
        interval_seconds=args.interval,
        registry_path=args.registry_path,
        client=GammaClient(force_mock=args.mock),
    )

    reachable = await scanner.client.is_api_reachable()
    mode = "mock" if args.mock or not reachable else "live (Gamma API)"
    print(f"\n数据源模式: {mode}\n")

    new_markets = await scanner.scan_once()

    if new_markets:
        print(f"发现 {len(new_markets)} 个新开的天气盘:")
        for m in new_markets:
            _print_market(m)
    else:
        print("本次扫描无新盘（注册表已覆盖所有扫描到的市场）")

    if args.evaluate:
        await _evaluate_new(new_markets, args)

    print_registry_summary(scanner.registry)
    return 0


async def _evaluate_new(new_markets, args) -> int:
    """把新盘接入策略引擎评估，输出信号统计。"""
    if not new_markets:
        return 0
    from market_discovery.bridge import MarketSignalBridge

    print(f"\n{'='*56}")
    print("  新盘 → 策略引擎评估")
    print(f"{'='*56}")
    bridge = MarketSignalBridge(
        forecast_dir=args.forecast_dir,
        registry_path=args.registry_path,
    )
    signals = await bridge.on_new_markets(new_markets)
    s = bridge.summary()
    print(
        f"\n  评估完成: {s['evaluated_markets']} 个市场, "
        f"{s['signals_generated']} 个信号"
    )
    if signals:
        print(f"  信号已保存到 {bridge.signal_dir}")
    print("=" * 56 + "\n")
    return 0


async def cmd_daemon(args):
    scanner = MarketScanner(
        interval_seconds=args.interval,
        registry_path=args.registry_path,
        client=GammaClient(force_mock=args.mock),
    )

    reachable = await scanner.client.is_api_reachable()
    mode = "mock" if args.mock or not reachable else "live (Gamma API)"
    print(f"数据源模式: {mode} | 间隔: {args.interval}s ({args.interval/60:.1f} 分钟)")
    print("按 Ctrl+C 停止\n")

    async def on_new(new_markets):
        print(f"\n=== {len(new_markets)} 个新盘 ===")
        for m in new_markets:
            _print_market(m)
        if args.evaluate:
            await _evaluate_new(new_markets, args)
        print()

    try:
        await scanner.start(on_new_markets=on_new)
    except KeyboardInterrupt:
        print("\n扫描器已停止")
    finally:
        print_registry_summary(scanner.registry)


def cmd_status(args):
    registry = MarketRegistry(args.registry_path)
    if not registry.count():
        print(f"注册表为空 ({args.registry_path})")
        return
    print_registry_summary(registry)


def cmd_reset(args):
    if os.path.exists(args.registry_path):
        os.remove(args.registry_path)
        print(f"已清空注册表: {args.registry_path}")
    else:
        print(f"注册表不存在: {args.registry_path}")


def _print_market(m: MarketInfo):
    from datetime import datetime
    end = (
        datetime.fromtimestamp(m.end_date_utc).strftime("%m-%d %H:%M")
        if m.end_date_utc else "?"
    )
    print(
        f"  · {m.location:<14} [{m.event_type}] 截止 {end}  "
        f"vol=${m.volume:,.0f}  liq=${m.liquidity:,.0f}"
    )
    print(f"      {m.question}")
    print(f"      condition={m.condition_id[:24]}  token={m.token_id[:16]}...")


def print_registry_summary(registry: MarketRegistry):
    s = registry.summary()
    by_et = "  ".join(f"{k}={v}" for k, v in s["by_event_type"].items()) or "(无)"
    print("\n" + "=" * 56)
    print("  市场注册表状态")
    print("=" * 56)
    print(f"  累计发现:   {s['total']} 个市场")
    print(f"  活跃中:     {s['active']} 个")
    print(f"  事件分布:   {by_et}")
    print(f"  上次扫描:   {s['last_scan_utc'] or '从未'}")
    print(f"  存储位置:   {s['path']}")
    print("=" * 56 + "\n")


def main():
    args = parse_args()
    setup_logging(args.verbose)

    if args.reset:
        cmd_reset(args)
        return 0
    if args.status:
        cmd_status(args)
        return 0
    if args.daemon:
        asyncio.run(cmd_daemon(args))
        return 0
    # 默认行为: 单次扫描
    return asyncio.run(cmd_run_once(args))


if __name__ == "__main__":
    sys.exit(main())
