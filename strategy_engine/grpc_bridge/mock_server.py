"""Mock gRPC 执行层服务 (用于测试)。

模拟 Rust 执行层的行为, 供 Python 端在没有 Rust server 的情况下测试 gRPC 通信。

启动:
  server = MockExecutionServer(port=50051)
  await server.start()
  # ... 运行测试 ...
  await server.stop()

行为:
  - PlaceOrder: 模拟下单, 返回成功 + 随机成交价
  - GetRiskState: 返回预设的风控状态
  - HealthCheck: 返回健康状态
  - StreamRiskStateUpdates: 每隔 N 秒推送一次风控更新
"""

import asyncio
import logging
import random
from datetime import datetime, timezone
from typing import Dict, Optional

import grpc

from strategy_engine.grpc_bridge import polymarket_pb2 as pb
from strategy_engine.grpc_bridge import polymarket_pb2_grpc as pb_grpc

logger = logging.getLogger(__name__)


class MockExecutionService(pb_grpc.ExecutionServiceServicer):
    """Mock 执行服务实现。"""

    def __init__(
        self,
        wallet_loaded: bool = True,
        api_creds_set: bool = True,
        fill_price_slippage: float = 0.001,
        fail_rate: float = 0.0,
    ):
        self.wallet_loaded = wallet_loaded
        self.api_creds_set = api_creds_set
        self.fill_price_slippage = fill_price_slippage
        self.fail_rate = fail_rate  # 模拟失败概率

        # 内部状态
        self._positions: Dict[str, float] = {}
        self._daily_pnl: float = 0.0
        self._order_count: int = 0
        self._start_time = datetime.now(timezone.utc)
        self._risk_state_update_queue: asyncio.Queue = asyncio.Queue()

    async def PlaceOrder(self, request, context):
        """模拟下单。"""
        # 随机失败
        if random.random() < self.fail_rate:
            return pb.OrderResult(
                order_id=request.order_id,
                success=False,
                error_message="模拟失败: 余额不足",
                timestamp=datetime.now(timezone.utc).isoformat(),
                filled_price=0.0,
                filled_size=0.0,
                status=pb.ExecutionStatus.REJECTED,
            )

        # 模拟成交 (目标价 +/- 滑点)
        slippage = random.uniform(-self.fill_price_slippage, self.fill_price_slippage)
        filled_price = max(0.01, min(0.99, request.target_price + slippage))
        filled_size = request.quantity

        # 更新内部持仓
        self._positions[request.token_id] = (
            self._positions.get(request.token_id, 0.0) + filled_size
        )
        self._order_count += 1

        logger.info(
            "Mock 下单: %s %s qty=%.2f @ %.4f -> filled=%.2f @ %.4f",
            request.order_id,
            pb.Direction.Name(request.direction),
            request.quantity,
            request.target_price,
            filled_size,
            filled_price,
        )

        # 推送风控状态更新
        await self._push_risk_update("order_filled")

        return pb.OrderResult(
            order_id=request.order_id,
            success=True,
            exchange_order_id=f"ex_{self._order_count:08d}",
            timestamp=datetime.now(timezone.utc).isoformat(),
            filled_price=filled_price,
            filled_size=filled_size,
            status=pb.ExecutionStatus.FILLED,
        )

    async def PlaceOrders(self, request, context):
        """批量下单。"""
        results = []
        for order in request.orders:
            result = await self.PlaceOrder(order, context)
            results.append(result)
        return pb.PlaceOrdersResponse(results=results)

    async def CancelOrder(self, request, context):
        """取消订单。"""
        return pb.CancelResult(
            success=True,
            message=f"已取消订单 {request.order_id}",
            cancelled_count=1,
        )

    async def CancelAll(self, request, context):
        """取消全部。"""
        count = len(self._positions)
        self._positions.clear()
        return pb.CancelResult(
            success=True,
            message=f"已取消 {count} 个订单",
            cancelled_count=count,
        )

    async def GetRiskState(self, request, context):
        """返回当前风控状态。"""
        return self._build_risk_state()

    async def StreamRiskStateUpdates(self, request, context):
        """流式推送风控状态更新。"""
        # 先推一次当前状态
        yield pb.RiskStateUpdate(
            risk_state=self._build_risk_state(),
            update_reason="initial",
            timestamp=datetime.now(timezone.utc).isoformat(),
        )

        # 持续监听队列
        try:
            while context.is_active():
                try:
                    update = await asyncio.wait_for(
                        self._risk_state_update_queue.get(),
                        timeout=5.0,
                    )
                    yield update
                except asyncio.TimeoutError:
                    # 超时也推一次心跳
                    yield pb.RiskStateUpdate(
                        risk_state=self._build_risk_state(),
                        update_reason="heartbeat",
                        timestamp=datetime.now(timezone.utc).isoformat(),
                    )
        except asyncio.CancelledError:
            pass

    async def HealthCheck(self, request, context):
        """健康检查。"""
        uptime_delta = datetime.now(timezone.utc) - self._start_time
        return pb.HealthCheckResponse(
            healthy=True,
            version="mock-0.1.0",
            uptime=str(uptime_delta).split(".")[0],
            wallet_loaded=self.wallet_loaded,
            api_creds_set=self.api_creds_set,
        )

    # ================================================================
    # 内部方法
    # ================================================================

    def _build_risk_state(self) -> pb.RiskState:
        positions = [
            pb.PositionEntry(token_id=tid, quantity=qty)
            for tid, qty in self._positions.items()
        ]
        return pb.RiskState(
            positions=positions,
            settlement_times=[],
            daily_pnl=self._daily_pnl,
            daily_circuit_breaker=False,
            manual_halt=False,
            last_reset_date=datetime.now(timezone.utc).strftime("%Y-%m-%d"),
        )

    async def _push_risk_update(self, reason: str):
        update = pb.RiskStateUpdate(
            risk_state=self._build_risk_state(),
            update_reason=reason,
            timestamp=datetime.now(timezone.utc).isoformat(),
        )
        await self._risk_state_update_queue.put(update)

    def set_daily_pnl(self, pnl: float):
        """设置当日 PnL (测试用)。"""
        self._daily_pnl = pnl


class MockExecutionServer:
    """Mock gRPC server 包装器。"""

    def __init__(
        self,
        port: int = 50051,
        wallet_loaded: bool = True,
        api_creds_set: bool = True,
        fill_price_slippage: float = 0.001,
        fail_rate: float = 0.0,
    ):
        self.port = port
        self.server = grpc.aio.server()
        self.service = MockExecutionService(
            wallet_loaded=wallet_loaded,
            api_creds_set=api_creds_set,
            fill_price_slippage=fill_price_slippage,
            fail_rate=fail_rate,
        )
        pb_grpc.add_ExecutionServiceServicer_to_server(
            self.service, self.server
        )
        self.server.add_insecure_port(f"[::]:{self.port}")

    async def start(self):
        await self.server.start()
        logger.info("Mock gRPC server 已启动, 监听端口 %d", self.port)

    async def stop(self):
        await self.server.stop(grace=1.0)
        logger.info("Mock gRPC server 已停止")

    async def __aenter__(self):
        await self.start()
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        await self.stop()
