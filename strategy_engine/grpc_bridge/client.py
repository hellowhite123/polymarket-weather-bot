"""gRPC 执行层客户端。

Python 策略引擎通过此客户端向 Rust 执行层发送交易指令。

架构:
  Python (client)  --gRPC-->  Rust (server)
  AggregatedOrder  -------->  PlaceOrder  -------->  OrderResult
  Empty            -------->  GetRiskState ------->  RiskState
  Empty            -------->  HealthCheck  ------->  HealthCheckResponse

传输方式:
  - UDS (Unix Domain Socket): "unix:///tmp/polymarket.sock"  (推荐, 低延迟)
  - TCP:                       "tcp://127.0.0.1:50051"       (跨机器/调试)
  - 本机 TCP:                   "127.0.0.1:50051"             (等价于 tcp://)
"""

import asyncio
import logging
from datetime import datetime, timezone
from typing import Dict, List, Optional, Union

import grpc

from strategy_engine.aggregator.models import (
    AggregatedOrder,
    RiskState,
    OrderStatus,
)
from strategy_engine.grpc_bridge import polymarket_pb2 as pb
from strategy_engine.grpc_bridge import polymarket_pb2_grpc as pb_grpc
from strategy_engine.grpc_bridge.converter import (
    order_to_proto,
    risk_state_from_proto,
    order_result_from_proto,
    fill_record_from_proto,
)

logger = logging.getLogger(__name__)


class GrpcBridgeError(Exception):
    """gRPC 桥接层错误。"""
    pass


class GrpcExecutionClient:
    """异步 gRPC 客户端, 连接 Rust 执行层。

    线程安全: gRPC channel 是线程安全的, 可在多个协程中共享。
    但不要在多个线程中共享同一个 client 实例。

    用法:
        client = GrpcExecutionClient("unix:///tmp/polymarket.sock")
        await client.connect()

        # 下单
        result = await client.place_order(aggregated_order)

        # 查询风控状态
        risk_state = await client.get_risk_state()

        await client.close()
    """

    def __init__(
        self,
        endpoint: str = "unix:///tmp/polymarket.sock",
        timeout: float = 10.0,
        max_retries: int = 3,
        retry_delay: float = 1.0,
    ):
        """
        Args:
            endpoint: gRPC 服务地址。
                      UDS: "unix:///tmp/polymarket.sock"
                      TCP: "127.0.0.1:50051"
            timeout: 单次 RPC 超时 (秒)
            max_retries: 连接失败重试次数
            retry_delay: 重试间隔 (秒)
        """
        self.endpoint = endpoint
        self.timeout = timeout
        self.max_retries = max_retries
        self.retry_delay = retry_delay

        self._channel: Optional[grpc.aio.Channel] = None
        self._stub: Optional[pb_grpc.ExecutionServiceStub] = None
        self._connected = False

    @property
    def connected(self) -> bool:
        return self._connected

    # ================================================================
    # 连接管理
    # ================================================================

    async def connect(self) -> None:
        """建立 gRPC 连接。"""
        if self._connected:
            return

        endpoint = self._normalize_endpoint(self.endpoint)

        for attempt in range(1, self.max_retries + 1):
            try:
                self._channel = grpc.aio.insecure_channel(endpoint)
                # 等待 channel 就绪
                await asyncio.wait_for(
                    self._channel.channel_ready(),
                    timeout=self.timeout,
                )
                self._stub = pb_grpc.ExecutionServiceStub(self._channel)
                self._connected = True
                logger.info("gRPC 已连接: %s (尝试 %d/%d)", endpoint, attempt, self.max_retries)
                return
            except (asyncio.TimeoutError, grpc.RpcError) as e:
                logger.warning(
                    "gRPC 连接失败 (尝试 %d/%d): %s",
                    attempt, self.max_retries, e,
                )
                if attempt < self.max_retries:
                    await asyncio.sleep(self.retry_delay * attempt)
                else:
                    raise GrpcBridgeError(
                        f"无法连接到 Rust 执行层 ({endpoint}), "
                        f"重试 {self.max_retries} 次后放弃: {e}"
                    )

    async def close(self) -> None:
        """关闭 gRPC 连接。"""
        if self._channel:
            await self._channel.close()
            self._channel = None
            self._stub = None
            self._connected = False
            logger.info("gRPC 连接已关闭")

    async def __aenter__(self):
        await self.connect()
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        await self.close()

    def _normalize_endpoint(self, endpoint: str) -> str:
        """规范化端点地址。"""
        # "tcp://host:port" -> "host:port" (gRPC 不认 tcp:// 前缀)
        if endpoint.startswith("tcp://"):
            return endpoint[6:]
        return endpoint

    def _ensure_connected(self):
        if not self._connected or self._stub is None:
            raise GrpcBridgeError(
                f"gRPC 未连接, 请先调用 connect()。端点: {self.endpoint}"
            )

    # ================================================================
    # RPC 方法
    # ================================================================

    async def place_order(self, order: AggregatedOrder, paper_trading: bool = False) -> dict:
        """下单。

        Args:
            order: 聚合后的交易指令

        Returns:
            执行结果 dict, 包含:
              - order_id: 对应的订单 ID
              - success: 是否成功
              - exchange_order_id: 交易所返回的订单 ID
              - filled_price / filled_size: 成交详情
              - status / status_name: 执行状态
        """
        self._ensure_connected()

        # 只发送可执行的指令
        if not order.is_actionable:
            logger.info(
                "跳过不可执行指令 %s (status=%s, reason=%s)",
                order.order_id, order.status.value, order.block_reason,
            )
            return {
                "order_id": order.order_id,
                "success": False,
                "exchange_order_id": "",
                "error_message": f"指令不可执行: status={order.status.value}, reason={order.block_reason}",
                "timestamp": datetime.now(timezone.utc).isoformat(),
                "filled_price": 0.0,
                "filled_size": 0.0,
                "status": 0,
                "status_name": "SKIPPED",
            }

        proto_order = order_to_proto(order, paper_trading=paper_trading)

        try:
            response: pb.OrderResult = await self._stub.PlaceOrder(
                proto_order,
                timeout=self.timeout,
            )
            result = order_result_from_proto(response)
            logger.info(
                "下单结果 %s: success=%s, filled=%.4f@%.4f, status=%s",
                result["order_id"],
                result["success"],
                result["filled_size"],
                result["filled_price"],
                result["status_name"],
            )
            return result
        except grpc.RpcError as e:
            logger.error("gRPC 下单失败: %s", e)
            raise GrpcBridgeError(f"下单 RPC 失败: {e}") from e

    async def place_orders(self, orders: List[AggregatedOrder], paper_trading: bool = False) -> List[dict]:
        """批量下单。"""
        self._ensure_connected()

        # 过滤可执行指令
        actionable = [o for o in orders if o.is_actionable]
        skipped = len(orders) - len(actionable)
        if skipped > 0:
            logger.info("批量下单: 跳过 %d 条不可执行指令", skipped)

        if not actionable:
            return []

        proto_orders = [order_to_proto(o, paper_trading=paper_trading) for o in actionable]
        request = pb.PlaceOrdersRequest(orders=proto_orders)

        try:
            response: pb.PlaceOrdersResponse = await self._stub.PlaceOrders(
                request,
                timeout=self.timeout * len(actionable),  # 按数量放大超时
            )
            results = [order_result_from_proto(r) for r in response.results]
            success_count = sum(1 for r in results if r["success"])
            logger.info(
                "批量下单: %d 条, 成功 %d, 失败 %d",
                len(results), success_count, len(results) - success_count,
            )
            return results
        except grpc.RpcError as e:
            logger.error("gRPC 批量下单失败: %s", e)
            raise GrpcBridgeError(f"批量下单 RPC 失败: {e}") from e

    async def cancel_order(self, order_id: str, wallet_address: str = "") -> dict:
        """取消指定订单。"""
        self._ensure_connected()

        request = pb.CancelOrderRequest(
            order_id=order_id,
            wallet_address=wallet_address,
        )

        try:
            response: pb.CancelResult = await self._stub.CancelOrder(
                request,
                timeout=self.timeout,
            )
            return {
                "success": response.success,
                "message": response.message,
                "cancelled_count": response.cancelled_count,
            }
        except grpc.RpcError as e:
            logger.error("gRPC 取消订单失败: %s", e)
            raise GrpcBridgeError(f"取消订单 RPC 失败: {e}") from e

    async def cancel_all(self, wallet_address: str = "") -> dict:
        """取消所有订单。"""
        self._ensure_connected()

        request = pb.CancelAllRequest(wallet_address=wallet_address)

        try:
            response: pb.CancelResult = await self._stub.CancelAll(
                request,
                timeout=self.timeout,
            )
            return {
                "success": response.success,
                "message": response.message,
                "cancelled_count": response.cancelled_count,
            }
        except grpc.RpcError as e:
            logger.error("gRPC 取消全部订单失败: %s", e)
            raise GrpcBridgeError(f"取消全部订单 RPC 失败: {e}") from e

    async def get_risk_state(self) -> RiskState:
        """查询当前风控状态。"""
        self._ensure_connected()

        try:
            response: pb.RiskState = await self._stub.GetRiskState(
                pb.Empty(),
                timeout=self.timeout,
            )
            return risk_state_from_proto(response)
        except grpc.RpcError as e:
            logger.error("gRPC 查询风控状态失败: %s", e)
            raise GrpcBridgeError(f"查询风控状态 RPC 失败: {e}") from e

    async def health_check(self) -> dict:
        """健康检查。"""
        self._ensure_connected()

        try:
            response: pb.HealthCheckResponse = await self._stub.HealthCheck(
                pb.Empty(),
                timeout=self.timeout,
            )
            return {
                "healthy": response.healthy,
                "version": response.version,
                "uptime": response.uptime,
                "wallet_loaded": response.wallet_loaded,
                "api_creds_set": response.api_creds_set,
            }
        except grpc.RpcError as e:
            logger.error("gRPC 健康检查失败: %s", e)
            raise GrpcBridgeError(f"健康检查 RPC 失败: {e}") from e

    async def get_fills(self) -> dict:
        """查询成交记录列表（完整字段：下单/成交/结算时间 + 滑点 + 手续费 + 盈亏）。

        Returns:
            dict, 包含:
              - fills: List[dict]  每笔成交记录
              - total / total_fee / total_slippage / total_pnl: 汇总统计
        """
        self._ensure_connected()

        try:
            response: pb.FillListResponse = await self._stub.GetFills(
                pb.Empty(),
                timeout=self.timeout,
            )
            return {
                "fills": [fill_record_from_proto(f) for f in response.fills],
                "total": response.total,
                "total_fee": response.total_fee,
                "total_slippage": response.total_slippage,
                "total_pnl": response.total_pnl,
            }
        except grpc.RpcError as e:
            logger.error("gRPC 查询成交记录失败: %s", e)
            raise GrpcBridgeError(f"查询成交记录 RPC 失败: {e}") from e

    async def stream_risk_state_updates(self, callback=None):
        """流式订阅风控状态更新。

        Args:
            callback: 可选的回调函数, 收到更新时调用。
                      若不提供, 则作为异步生成器使用。

        Yields:
            RiskStateUpdate proto 消息 (如果不用 callback 模式)
        """
        self._ensure_connected()

        try:
            call = self._stub.StreamRiskStateUpdates(pb.Empty())
            async for update in call:
                if callback:
                    await callback(update)
                else:
                    yield update
        except grpc.RpcError as e:
            if e.code() == grpc.StatusCode.CANCELLED:
                logger.info("风控状态流已取消")
            else:
                logger.error("gRPC 风控状态流错误: %s", e)
                raise GrpcBridgeError(f"风控状态流 RPC 失败: {e}") from e
