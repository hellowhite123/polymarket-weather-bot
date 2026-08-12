"""市场发现模块 -- 定时扫描 Polymarket 新开的天气盘。

模块组成:
  - models.py       MarketInfo 数据模型 + 事件类型推断
  - gamma_client.py Gamma API 客户端（真实 + mock 降级）
  - registry.py     MarketRegistry 增量去重 + JSON 持久化
  - scanner.py      MarketScanner 定时扫描 + 新盘回调
  - bridge.py       MarketSignalBridge 新盘 → 策略引擎信号闭环
  - main.py         CLI 入口

用法:
    python -m market_discovery --run-once      # 单次扫描
    python -m market_discovery --daemon        # 常驻，默认 10 分钟一次
    python -m market_discovery --mock --run-once  # 强制 mock
    python -m market_discovery --daemon --evaluate  # 常驻 + 新盘自动评估信号
"""

from .models import (
    MarketInfo,
    infer_event_type,
    extract_location,
    normalize_location,
)
from .gamma_client import GammaClient
from .registry import MarketRegistry
from .scanner import MarketScanner
from .bridge import MarketSignalBridge

__version__ = "0.2.0"

__all__ = [
    "MarketInfo",
    "infer_event_type",
    "extract_location",
    "normalize_location",
    "GammaClient",
    "MarketRegistry",
    "MarketScanner",
    "MarketSignalBridge",
    "__version__",
]
