"""Python 策略大脑侧延迟基准。

测量「信号 → 聚合 → 序列化 → gRPC 往返」各环节耗时：

  1. aggregate          —— 信号聚合器（冲突仲裁 + 融合 + 风控 + 仓位）
  2. serialize_order    —— dataclass -> proto -> bytes
  3. deserialize_result —— bytes -> proto -> dict
  4. grpc_health_rt     —— gRPC HealthCheck 本地往返（纯框架开销）
  5. grpc_placeorder_rt —— gRPC PlaceOrder 本地往返（含序列化+反序列化）

gRPC 往返使用 mock server（本地 loopback TCP），在无外网环境下
量化 Python<->执行层 之间的进程间通信延迟。

用法：
  python latency_bench_py.py

输出：`name<TAB>mean_ms<TAB>p50_ms<TAB>p95_ms<TAB>p99_ms<TAB>max_ms<TAB>n`
"""

import asyncio
import math
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from strategy_engine.aggregator.aggregator import AggregatorParams, SignalAggregator
from strategy_engine.aggregator.models import RiskState
from strategy_engine.grpc_bridge import polymarket_pb2 as pb
from strategy_engine.grpc_bridge.client import GrpcExecutionClient
from strategy_engine.grpc_bridge.converter import order_result_from_proto, order_to_proto
from strategy_engine.grpc_bridge.mock_server import MockExecutionServer
from strategy_engine.models import EventType, SignalDirection, TradingSignal


def make_signal(strategy, direction, token_id, confidence, target_price, market_price):
    return TradingSignal(
        strategy=strategy,
        direction=direction,
        token_id=token_id,
        event_type=EventType("rain"),
        location="NewYork",
        target_price=target_price,
        market_price=market_price,
        confidence=confidence,
    )


def pct(samples):
    samples = sorted(samples)
    n = len(samples)
    def pick(q):
        idx = min(n - 1, int(math.ceil(q * n)) - 1)
        return samples[idx]
    return {
        "mean": sum(samples) / n,
        "p50": pick(0.50),
        "p95": pick(0.95),
        "p99": pick(0.99),
        "max": samples[-1],
        "n": n,
    }


def emit(name, s):
    print(
        f"{name}\t{s['mean']*1e3:.6f}\t{s['p50']*1e3:.6f}\t{s['p95']*1e3:.6f}"
        f"\t{s['p99']*1e3:.6f}\t{s['max']*1e3:.6f}\t{s['n']}"
    )


def bench_sync(n, f):
    samples = []
    for _ in range(n):
        t = time.perf_counter()
        f()
        samples.append(time.perf_counter() - t)
    return pct(samples)


async def bench_async(n, f):
    samples = []
    for _ in range(n):
        t = time.perf_counter()
        await f()
        samples.append(time.perf_counter() - t)
    return pct(samples)


async def main():
    print("Python 策略大脑侧延迟基准 (单位 ms)\n")

    # ---------- 准备信号 + 聚合器 ----------
    signals = [
        make_signal("forecast_deviation", SignalDirection.BUY_YES, "tok_A", 0.80, 0.85, 0.70),
        make_signal("multi_source", SignalDirection.BUY_YES, "tok_A", 0.90, 0.88, 0.70),
        make_signal("forecast_deviation", SignalDirection.BUY_NO, "tok_B", 0.75, 0.30, 0.45),
    ]
    agg = SignalAggregator(AggregatorParams())

    # 1. 聚合器
    orders = agg.aggregate(signals)
    actionable = [o for o in orders if o.is_actionable]
    s = bench_sync(5_000, lambda: agg.aggregate(signals, RiskState()))
    emit("aggregate", s)

    if not actionable:
        print("!! 无可执行订单，聚合结果异常，退出")
        return

    order = actionable[0]

    # 2. 序列化 (dataclass -> proto -> bytes)
    proto = order_to_proto(order)
    s = bench_sync(20_000, lambda: order_to_proto(order).SerializeToString())
    emit("serialize_order", s)

    # 3. 反序列化 (bytes -> proto -> dict)
    raw = order_to_proto(order).SerializeToString()
    s = bench_sync(20_000, lambda: order_result_from_proto(pb.OrderResult.FromString(raw)))
    emit("deserialize_result", s)

    # ---------- 4/5. gRPC 本地往返 ----------
    server = MockExecutionServer(port=50061)
    await server.start()
    client = GrpcExecutionClient("127.0.0.1:50061", timeout=10.0)
    await client.connect()

    # 4. HealthCheck 纯框架往返
    s = await bench_async(2_000, lambda: client.health_check())
    emit("grpc_health_rt", s)

    # 5. PlaceOrder 往返（含序列化 + mock 处理 + 反序列化）
    s = await bench_async(2_000, lambda: client.place_order(order))
    emit("grpc_placeorder_rt", s)

    await client.close()
    await server.stop()

    print("\n完成。")


if __name__ == "__main__":
    asyncio.run(main())
