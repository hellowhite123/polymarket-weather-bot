"""策略引擎 -- 多策略信号生成与融合。

当前已实现:
  - ForecastDeviationStrategy: 预报偏差策略 (气象概率 vs 市场隐含概率)
  - MultiSourceStrategy: 多源验证策略 (多源融合 vs 市场, 检测源间分歧)
  - NowcastCorrectionStrategy: 临近修正策略 (实况观测修正预报概率)
  - HistoricalStatisticalStrategy: 历史统计策略 (气候学基线 vs 市场)
  - SignalAggregator: 信号聚合器 (冲突仲裁 -> 置信度融合 -> 风控 -> 仓位)
  - GrpcExecutionClient: gRPC 桥接客户端 (Python -> Rust 执行层)
  - BacktestEngine: 回测引擎 (历史数据 -> 策略 -> 模拟交易 -> 绩效统计)
"""

from .models import (
    TradingSignal,
    SignalDirection,
    DeviationRecord,
    DivergenceRecord,
    MarketSnapshot,
)
from .aggregator import SignalAggregator, AggregatedOrder, RiskState
from .aggregator.aggregator import AggregatorParams
from .config import (
    StrategyConfig,
    DeviationStrategyParams,
    MultiSourceStrategyParams,
    NowcastParams,
    HistoricalParams,
)
from .strategies.nowcast_correction import NowcastCorrectionStrategy
from .strategies.historical_statistical import HistoricalStatisticalStrategy

# 回测框架
from .backtest import (
    BacktestConfig,
    BacktestEngine,
    BacktestReporter,
    DataLoader as BacktestDataLoader,
    HistoricalEvent,
    EventOutcome,
)

# gRPC 桥接 (可选依赖)
try:
    from .grpc_bridge import GrpcExecutionClient, GrpcBridgeError
    _HAS_GRPC = True
except ImportError:
    _HAS_GRPC = False
    GrpcExecutionClient = None
    GrpcBridgeError = None

__version__ = "0.7.0"

__all__ = [
    "TradingSignal",
    "SignalDirection",
    "DeviationRecord",
    "DivergenceRecord",
    "MarketSnapshot",
    "StrategyConfig",
    "DeviationStrategyParams",
    "MultiSourceStrategyParams",
    "NowcastParams",
    "HistoricalParams",
    "NowcastCorrectionStrategy",
    "HistoricalStatisticalStrategy",
    "SignalAggregator",
    "AggregatorParams",
    "AggregatedOrder",
    "RiskState",
    "GrpcExecutionClient",
    "GrpcBridgeError",
    "BacktestConfig",
    "BacktestEngine",
    "BacktestReporter",
    "BacktestDataLoader",
    "HistoricalEvent",
    "EventOutcome",
    "__version__",
]
