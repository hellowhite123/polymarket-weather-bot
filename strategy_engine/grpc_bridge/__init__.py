"""gRPC 桥接层 -- Python 策略大脑 -> Rust 执行手脚。

包结构:
  polymarket_pb2.py      -- proto 生成的消息定义
  polymarket_pb2_grpc.py -- proto 生成的 gRPC 桩
  client.py              -- GrpcExecutionClient 封装
  converter.py           -- dataclass <-> proto 消息转换

使用方式:
  from strategy_engine.grpc_bridge import GrpcExecutionClient

  client = GrpcExecutionClient(endpoint="unix:///tmp/polymarket.sock")
  result = client.place_order(aggregated_order)
  risk_state = client.get_risk_state()
"""

from .client import GrpcExecutionClient, GrpcBridgeError
from .converter import (
    order_to_proto,
    order_from_proto,
    risk_state_to_proto,
    risk_state_from_proto,
    order_result_from_proto,
)

__all__ = [
    "GrpcExecutionClient",
    "GrpcBridgeError",
    "order_to_proto",
    "order_from_proto",
    "risk_state_to_proto",
    "risk_state_from_proto",
    "order_result_from_proto",
]
