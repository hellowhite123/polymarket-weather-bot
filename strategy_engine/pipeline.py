"""策略管线编排。

从气象预报 + 市场数据到交易信号的完整链路:
  1. 加载 WeatherForecast (JSON)
  2. 获取/生成 MarketSnapshot (真实 API 或 Mock)
  3. 运行策略评估 (预报偏差 / 多源验证)
  4. 输出 TradingSignal
  5. 保存信号和统计状态
"""

import asyncio
import glob
import json
import logging
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Dict, List, Optional, Tuple

from .config import StrategyConfig
from .models import (
    DeviationRecord,
    DivergenceRecord,
    EventType,
    MarketSnapshot,
    TradingSignal,
)
from .statistics.deviation_tracker import DeviationTracker
from .statistics.divergence_tracker import DivergenceTracker
from .statistics.climate_tracker import ClimateTracker
from .strategies.forecast_deviation import ForecastDeviationStrategy
from .strategies.forecast_utils import extract_probability, get_location
from .strategies.mock_market import MockMarketProvider
from .strategies.multi_source import MultiSourceStrategy
from .strategies.nowcast_correction import NowcastCorrectionStrategy
from .strategies.historical_statistical import HistoricalStatisticalStrategy

# gRPC 桥接 (延迟导入, 避免未安装 grpcio 时报错)
try:
    from .grpc_bridge import GrpcExecutionClient, GrpcBridgeError
    HAS_GRPC = True
except ImportError:
    HAS_GRPC = False
    GrpcExecutionClient = None
    GrpcBridgeError = Exception

logger = logging.getLogger(__name__)


def __normalize_location_safe(location: str) -> str:
    """城市名归一化用于跨模块匹配（market_discovery 不可用时本地兜底）。"""
    try:
        from market_discovery.models import normalize_location
        return normalize_location(location)
    except ImportError:
        return "".join(c for c in location.lower() if c.isalnum())


class StrategyPipeline:
    """策略管线编排器。

    用法:
        config = StrategyConfig.from_env()
        pipeline = StrategyPipeline(config)

        # 预报偏差策略
        signals = await pipeline.run(forecast_files=[...], market_mode="mock")

        # 多源验证策略
        signals = await pipeline.run_multi_source(
            multi_forecast_files=[[noaa_file, caiyun_file], ...],
            market_mode="mock",
        )
    """

    def __init__(self, config: Optional[StrategyConfig] = None):
        self.config = config or StrategyConfig()

        # 偏差追踪器 (model vs market)
        tracker_path = os.path.join(
            self.config.tracker_storage_path, "deviation_tracker.json"
        )
        tracker = DeviationTracker.load(tracker_path)
        if tracker is None:
            tracker = DeviationTracker(
                window_size=self.config.deviation.window_size
            )
            logger.info("创建新的偏差追踪器 (window_size=%d)", tracker.window_size)

        self.tracker = tracker
        self.tracker_path = tracker_path

        # 分歧追踪器 (source vs source)
        div_tracker_path = os.path.join(
            self.config.tracker_storage_path, "divergence_tracker.json"
        )
        div_tracker = DivergenceTracker.load(div_tracker_path)
        if div_tracker is None:
            div_tracker = DivergenceTracker(
                window_size=self.config.multi_source.window_size
            )
            logger.info(
                "创建新的分歧追踪器 (window_size=%d)", div_tracker.window_size
            )

        self.div_tracker = div_tracker
        self.div_tracker_path = div_tracker_path

        # 策略实例
        self.deviation_strategy = ForecastDeviationStrategy(
            tracker=self.tracker,
            params=self.config.deviation,
        )
        self.multi_source_strategy = MultiSourceStrategy(
            divergence_tracker=self.div_tracker,
            deviation_tracker=self.tracker,
            params=self.config.multi_source,
        )

        # 临近修正策略
        self.nowcast_strategy = NowcastCorrectionStrategy(
            params=self.config.nowcast,
        )

        # 历史统计策略
        clim_tracker_path = os.path.join(
            self.config.tracker_storage_path, "climate_tracker.json"
        )
        clim_tracker = ClimateTracker.load(clim_tracker_path)
        if clim_tracker is None:
            clim_tracker = ClimateTracker()
            logger.info("创建新的气候追踪器")

        self.clim_tracker = clim_tracker
        self.clim_tracker_path = clim_tracker_path

        self.historical_strategy = HistoricalStatisticalStrategy(
            tracker=self.clim_tracker,
            params=self.config.historical,
        )

        # Mock 市场提供者
        self.mock_provider: Optional[MockMarketProvider] = None

        # 信号聚合器
        from .aggregator.aggregator import SignalAggregator
        self.aggregator = SignalAggregator(self.config.aggregator)

    # ================================================================
    # 核心管线 -- 预报偏差策略
    # ================================================================

    async def run(
        self,
        forecast_files: List[str],
        market_mode: str = "mock",
        event_types: Optional[List[str]] = None,
        target_hours: int = 24,
        mock_bias: float = 0.0,
        mock_noise: float = 0.05,
        output_signals: bool = True,
        registry_path: str = "./data/market_registry.json",
    ) -> List[TradingSignal]:
        """执行预报偏差策略管线。

        market_mode:
          - "mock": 生成模拟市场（默认）
          - "registry": 从 market_discovery 注册表加载真实市场
        """
        # Step 1: 加载预报数据
        forecasts = self._load_forecasts(forecast_files)
        if not forecasts:
            logger.warning("无有效预报数据")
            return []
        logger.info("已加载 %d 个预报", len(forecasts))

        # Step 2: 解析事件类型
        if event_types is None:
            event_types = ["rain"] * len(forecasts)
        parsed_types = [EventType(et) for et in event_types]
        while len(parsed_types) < len(forecasts):
            parsed_types.append(parsed_types[-1])
        parsed_types = parsed_types[:len(forecasts)]

        # Step 3: 获取市场数据
        if market_mode == "mock":
            markets = self._generate_mock_markets(
                forecasts, parsed_types, target_hours,
                bias=mock_bias, noise=mock_noise,
            )
        elif market_mode == "registry":
            markets = self._load_registry_markets(
                forecasts, parsed_types, registry_path,
            )
        else:
            raise NotImplementedError(
                f"未支持的市场模式: '{market_mode}' (可选: mock / registry)"
            )

        # Step 4: 运行策略评估
        signals = await self.deviation_strategy.evaluate_batch(
            forecasts=forecasts,
            markets=markets,
            event_types=parsed_types,
            target_hours_ahead=target_hours,
            record_deviations=True,
        )

        # Step 5: 保存
        if output_signals and signals:
            self._save_signals(signals)
        if self.config.deviation.persist_tracker:
            self.tracker.save(self.tracker_path)

        return signals

    # ================================================================
    # 核心管线 -- 多源验证策略
    # ================================================================

    async def run_multi_source(
        self,
        multi_forecast_files: List[List[str]],
        market_mode: str = "mock",
        event_types: Optional[List[str]] = None,
        target_hours: int = 24,
        mock_bias: float = 0.0,
        mock_noise: float = 0.05,
        output_signals: bool = True,
        registry_path: str = "./data/market_registry.json",
    ) -> List[TradingSignal]:
        """执行多源验证策略管线。

        Args:
            multi_forecast_files: 每个元素是一组多源预报文件路径,
                                  例如 [[noaa_nyc.json, caiyun_nyc.json], ...]
            event_types: 事件类型列表, 与外层一一对应
            target_hours: 目标时间窗口
            market_mode: "mock" (默认) 或 "registry" (market_discovery 注册表)
            mock_bias/noise: Mock 市场参数
        """
        # Step 1: 加载多源预报
        multi_forecasts = []
        for source_files in multi_forecast_files:
            forecasts = self._load_forecasts(source_files)
            if len(forecasts) >= 2:
                multi_forecasts.append(forecasts)
            else:
                logger.warning(
                    "跳过: 需要 >=2 个源的预报, 实际 %d", len(forecasts)
                )

        if not multi_forecasts:
            logger.warning("无有效的多源预报组")
            return []
        logger.info("已加载 %d 组多源预报", len(multi_forecasts))

        # Step 2: 解析事件类型
        n_groups = len(multi_forecasts)
        if event_types is None:
            event_types = ["rain"] * n_groups
        parsed_types = [EventType(et) for et in event_types]
        while len(parsed_types) < n_groups:
            parsed_types.append(parsed_types[-1])
        parsed_types = parsed_types[:n_groups]

        # Step 3: 获取市场数据
        # 用每组第一个预报生成 mock 市场 (基于融合概率的近似)
        if market_mode == "mock":
            markets = self._generate_mock_markets_multi(
                multi_forecasts, parsed_types, target_hours,
                bias=mock_bias, noise=mock_noise,
            )
        elif market_mode == "registry":
            markets = self._load_registry_markets_multi(
                multi_forecasts, parsed_types, registry_path,
            )
        else:
            raise NotImplementedError(
                f"未支持的市场模式: '{market_mode}' (可选: mock / registry)"
            )

        # Step 4: 运行多源策略评估
        signals = await self.multi_source_strategy.evaluate_batch(
            multi_forecasts=multi_forecasts,
            markets=markets,
            event_types=parsed_types,
            target_hours_ahead=target_hours,
            record_data=True,
        )

        # Step 5: 保存
        if output_signals and signals:
            self._save_signals(signals)
        if self.config.multi_source.persist_tracker:
            self.div_tracker.save(self.div_tracker_path)
        if self.config.deviation.persist_tracker:
            self.tracker.save(self.tracker_path)

        return signals

    # ================================================================
    # 核心管线 -- 临近修正策略
    # ================================================================

    async def run_nowcast(
        self,
        forecast_files: List[str],
        observation_files: Optional[List[str]] = None,
        market_mode: str = "mock",
        event_types: Optional[List[str]] = None,
        target_hours: int = 24,
        event_deadline_hours: float = 6.0,
        mock_bias: float = 0.0,
        mock_noise: float = 0.05,
        mock_precip_deviation: float = 0.0,
        output_signals: bool = True,
        registry_path: str = "./data/market_registry.json",
    ) -> List[TradingSignal]:
        """执行临近修正策略管线。

        Args:
            forecast_files: 预报 JSON 文件列表
            observation_files: 观测 JSON 文件列表（可选，为 None 时用 Mock）
            event_types: 事件类型列表
            target_hours: 预报目标时间窗口
            event_deadline_hours: 事件截止时间（从现在起 N 小时后）
            mock_bias/noise: Mock 市场参数
            mock_precip_deviation: Mock 观测偏差（正值=比预报更湿，负值=更干）
        """
        # Step 1: 加载预报数据
        forecasts = self._load_forecasts(forecast_files)
        if not forecasts:
            logger.warning("无有效预报数据")
            return []
        logger.info("已加载 %d 个预报", len(forecasts))

        # Step 2: 解析事件类型
        if event_types is None:
            event_types = ["rain"] * len(forecasts)
        parsed_types = [EventType(et) for et in event_types]
        while len(parsed_types) < len(forecasts):
            parsed_types.append(parsed_types[-1])
        parsed_types = parsed_types[:len(forecasts)]

        # Step 3: 获取观测数据
        if observation_files:
            observations = self._load_observations(observation_files)
        else:
            observations = self._generate_mock_observations(
                forecasts, parsed_types, target_hours,
                precip_deviation=mock_precip_deviation,
            )
        logger.info("已加载 %d 个观测", len(observations))

        # Step 4: 获取市场数据
        if market_mode == "mock":
            markets = self._generate_mock_markets(
                forecasts, parsed_types, target_hours,
                bias=mock_bias, noise=mock_noise,
            )
        elif market_mode == "registry":
            markets = self._load_registry_markets(
                forecasts, parsed_types, registry_path,
            )
        else:
            raise NotImplementedError(
                f"未支持的市场模式: '{market_mode}' (可选: mock / registry)"
            )

        # Step 5: 计算事件截止时间
        now = datetime.now(timezone.utc)
        from datetime import timedelta
        event_deadlines = [now + timedelta(hours=event_deadline_hours)] * len(forecasts)

        # Step 6: 运行临近修正策略
        signals = self.nowcast_strategy.evaluate_batch(
            forecasts=forecasts,
            observations=observations,
            markets=markets,
            event_types=parsed_types,
            event_deadlines=event_deadlines,
            target_hours_ahead=target_hours,
        )

        # Step 7: 保存
        if output_signals and signals:
            self._save_signals(signals)

        return signals

    # ================================================================
    # 核心管线 -- 历史统计策略
    # ================================================================

    async def run_historical(
        self,
        forecast_files: List[str],
        historical_data_file: Optional[str] = None,
        market_mode: str = "mock",
        event_types: Optional[List[str]] = None,
        target_hours: int = 24,
        mock_bias: float = 0.0,
        mock_noise: float = 0.05,
        output_signals: bool = True,
        registry_path: str = "./data/market_registry.json",
    ) -> List[TradingSignal]:
        """执行历史统计策略管线。

        Args:
            forecast_files: 预报 JSON 文件列表
            historical_data_file: 历史气候观测 JSON 文件（可选，为 None 时用 Mock）
            event_types: 事件类型列表
            target_hours: 预报目标时间窗口
            mock_bias/noise: Mock 市场参数
        """
        # Step 1: 加载预报数据
        forecasts = self._load_forecasts(forecast_files)
        if not forecasts:
            logger.warning("无有效预报数据")
            return []
        logger.info("已加载 %d 个预报", len(forecasts))

        # Step 2: 解析事件类型
        if event_types is None:
            event_types = ["rain"] * len(forecasts)
        parsed_types = [EventType(et) for et in event_types]
        while len(parsed_types) < len(forecasts):
            parsed_types.append(parsed_types[-1])
        parsed_types = parsed_types[:len(forecasts)]

        # Step 3: 加载历史气候数据
        if historical_data_file:
            extra_tracker = ClimateTracker.from_historical_json(historical_data_file)
            # 合并到主追踪器
            for key_info in extra_tracker.list_keys():
                loc = key_info["location"]
                evt = EventType(key_info["event_type"])
                month = key_info["month"]
                s = extra_tracker.get_stats(loc, evt, month)
                if s:
                    for v in s._values:
                        self.clim_tracker.add_observation(loc, evt, month, v)
            logger.info("从 %s 导入历史数据并合并", historical_data_file)
        else:
            # 生成 Mock 历史数据
            self._generate_mock_climatology(forecasts, parsed_types)
            logger.info("已生成 Mock 历史气候数据")

        # Step 4: 获取市场数据
        if market_mode == "mock":
            markets = self._generate_mock_markets(
                forecasts, parsed_types, target_hours,
                bias=mock_bias, noise=mock_noise,
            )
        elif market_mode == "registry":
            markets = self._load_registry_markets(
                forecasts, parsed_types, registry_path,
            )
        else:
            raise NotImplementedError(
                f"未支持的市场模式: '{market_mode}' (可选: mock / registry)"
            )

        # Step 5: 运行策略评估
        signals = await self.historical_strategy.evaluate_batch(
            forecasts=forecasts,
            markets=markets,
            event_types=parsed_types,
            target_hours_ahead=target_hours,
            record_data=True,
        )

        # Step 6: 保存
        if output_signals and signals:
            self._save_signals(signals)
        if self.config.historical.persist_tracker:
            self.clim_tracker.save(self.clim_tracker_path)

        return signals

    # ================================================================
    # 信号聚合
    # ================================================================

    def aggregate_signals(
        self,
        signals: List[TradingSignal],
        risk_state=None,
        total_portfolio_value: Optional[float] = None,
    ) -> list:
        """聚合多策略信号为最终交易指令。

        Args:
            signals: 来自所有策略的 TradingSignal 列表
            risk_state: 风控状态 (默认空, 允许全部交易)
            total_portfolio_value: 总资产 (USD), 用于下单金额上限分档;
                                   None = 不设限 (回测/测试)
        """
        # 动态更新下单金额上限
        if total_portfolio_value is not None:
            from .aggregator.order_size_limits import OrderSizeLimits
            self.aggregator.size_limits = OrderSizeLimits.from_portfolio(
                total_portfolio_value
            )
        return self.aggregator.aggregate(signals, risk_state)

    async def run_aggregated(
        self,
        forecast_files: Optional[List[str]] = None,
        multi_forecast_files: Optional[List[List[str]]] = None,
        nowcast_forecast_files: Optional[List[str]] = None,
        nowcast_observation_files: Optional[List[str]] = None,
        event_deadline_hours: float = 6.0,
        mock_precip_deviation: float = 0.0,
        historical_forecast_files: Optional[List[str]] = None,
        historical_data_file: Optional[str] = None,
        event_types: Optional[List[str]] = None,
        target_hours: int = 24,
        market_mode: str = "mock",
        mock_bias: float = 0.0,
        mock_noise: float = 0.05,
        risk_state=None,
        output_orders: bool = True,
        registry_path: str = "./data/market_registry.json",
        total_portfolio_value: Optional[float] = None,
    ) -> list:
        """运行策略 + 聚合信号 -> 最终交易指令。

        同时运行预报偏差、多源验证、临近修正和/或历史统计策略,
        收集全部信号后聚合。

        Args:
            forecast_files: 单源预报文件 (预报偏差策略)
            multi_forecast_files: 多源预报组 (多源验证策略)
            nowcast_forecast_files: 预报文件 (临近修正策略)
            nowcast_observation_files: 观测文件 (临近修正策略, None 则 Mock)
            event_deadline_hours: 事件截止时间 (从现在起 N 小时后)
            mock_precip_deviation: Mock 观测降水偏差
            historical_forecast_files: 预报文件 (历史统计策略)
            historical_data_file: 历史气候数据 JSON (可选)
            event_types: 事件类型列表
            risk_state: 风控状态
        """
        all_signals: List[TradingSignal] = []

        # 运行预报偏差策略
        if forecast_files:
            dev_signals = await self.run(
                forecast_files=forecast_files,
                event_types=event_types,
                target_hours=target_hours,
                market_mode=market_mode,
                mock_bias=mock_bias,
                mock_noise=mock_noise,
                registry_path=registry_path,
                output_signals=False,
            )
            all_signals.extend(dev_signals)
            logger.info("预报偏差策略: %d 个信号", len(dev_signals))

        # 运行多源验证策略
        if multi_forecast_files:
            ms_signals = await self.run_multi_source(
                multi_forecast_files=multi_forecast_files,
                event_types=event_types,
                target_hours=target_hours,
                market_mode=market_mode,
                mock_bias=mock_bias,
                mock_noise=mock_noise,
                registry_path=registry_path,
                output_signals=False,
            )
            all_signals.extend(ms_signals)
            logger.info("多源验证策略: %d 个信号", len(ms_signals))

        # 运行临近修正策略
        if nowcast_forecast_files:
            nc_signals = await self.run_nowcast(
                forecast_files=nowcast_forecast_files,
                observation_files=nowcast_observation_files,
                event_types=event_types,
                target_hours=target_hours,
                event_deadline_hours=event_deadline_hours,
                market_mode=market_mode,
                mock_bias=mock_bias,
                mock_noise=mock_noise,
                mock_precip_deviation=mock_precip_deviation,
                registry_path=registry_path,
                output_signals=False,
            )
            all_signals.extend(nc_signals)
            logger.info("临近修正策略: %d 个信号", len(nc_signals))

        # 运行历史统计策略
        if historical_forecast_files:
            hist_signals = await self.run_historical(
                forecast_files=historical_forecast_files,
                historical_data_file=historical_data_file,
                event_types=event_types,
                target_hours=target_hours,
                market_mode=market_mode,
                mock_bias=mock_bias,
                mock_noise=mock_noise,
                registry_path=registry_path,
                output_signals=False,
            )
            all_signals.extend(hist_signals)
            logger.info("历史统计策略: %d 个信号", len(hist_signals))

        # 聚合
        orders = self.aggregate_signals(all_signals, risk_state, total_portfolio_value)

        # 保存
        if output_orders and orders:
            self._save_orders(orders)

        return orders

    def _save_orders(self, orders: list) -> None:
        """保存聚合指令到磁盘。"""
        out_dir = Path(self.config.signal_output_path) / "orders"
        out_dir.mkdir(parents=True, exist_ok=True)

        timestamp = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
        for i, order in enumerate(orders):
            filename = f"{timestamp}_order_{i:03d}.json"
            filepath = out_dir / filename
            with open(filepath, "w", encoding="utf-8") as f:
                json.dump(order.to_dict(), f, indent=2, ensure_ascii=False)

        logger.info("已保存 %d 个聚合指令到 %s", len(orders), out_dir)

    # ================================================================
    # gRPC 发送
    # ================================================================

    async def send_to_rust(
        self,
        orders: list,
        endpoint: str = "127.0.0.1:50051",
        timeout: float = 10.0,
    ) -> list:
        """通过 gRPC 将聚合指令发送到 Rust 执行层。

        Args:
            orders: AggregatedOrder 列表
            endpoint: Rust gRPC 服务地址
                      TCP: "127.0.0.1:50051"
                      UDS: "unix:///tmp/polymarket.sock"
            timeout: RPC 超时 (秒)

        Returns:
            执行结果列表 [{order_id, success, filled_price, ...}]
        """
        if not HAS_GRPC:
            raise RuntimeError(
                "gRPC 桥接层未安装, 请运行: "
                "pip install grpcio grpcio-tools"
            )

        if not orders:
            logger.info("无指令需要发送")
            return []

        actionable = [o for o in orders if o.is_actionable]
        logger.info(
            "发送 %d 条指令到 Rust (%s), 其中可执行 %d 条",
            len(orders), endpoint, len(actionable),
        )

        async with GrpcExecutionClient(
            endpoint=endpoint,
            timeout=timeout,
        ) as client:
            # 先做健康检查
            try:
                health = await client.health_check()
                logger.info(
                    "Rust 执行层: healthy=%s, version=%s, wallet=%s",
                    health["healthy"], health["version"], health["wallet_loaded"],
                )
            except GrpcBridgeError as e:
                logger.error("健康检查失败: %s", e)
                raise

            # 批量下单
            results = await client.place_orders(orders)

            success_count = sum(1 for r in results if r["success"])
            logger.info(
                "执行结果: %d 条, 成功 %d, 失败 %d",
                len(results), success_count, len(results) - success_count,
            )

            return results

    # ================================================================
    # 查询方法
    # ================================================================

    def get_tracker_summary(self) -> dict:
        """获取追踪器统计摘要。"""
        return {
            "deviation_tracker": {
                "total_records": self.tracker.total_records(),
                "tracked_keys": self.tracker.key_count(),
                "keys": self.tracker.list_keys(),
                "signal_count": self.deviation_strategy.signal_count,
            },
            "divergence_tracker": {
                "total_records": self.div_tracker.total_records(),
                "tracked_keys": self.div_tracker.key_count(),
                "keys": self.div_tracker.list_keys(),
                "signal_count": self.multi_source_strategy.signal_count,
            },
            "nowcast_correction": {
                "signal_count": self.nowcast_strategy.signal_count,
            },
            "historical_statistical": {
                "total_observations": self.clim_tracker.total_observations(),
                "tracked_keys": self.clim_tracker.key_count(),
                "keys": self.clim_tracker.list_keys(),
                "signal_count": self.historical_strategy.signal_count,
            },
        }

    def get_recent_signals(self, limit: int = 20) -> List[TradingSignal]:
        """获取最近的信号（从磁盘加载）。"""
        signal_dir = Path(self.config.signal_output_path)
        if not signal_dir.exists():
            return []

        signals: List[TradingSignal] = []
        json_files = sorted(signal_dir.glob("*.json"), key=os.path.getmtime, reverse=True)

        for f in json_files[:limit]:
            try:
                with open(f, "r", encoding="utf-8") as fp:
                    data = json.load(fp)
                signals.append(TradingSignal.from_dict(data))
            except Exception:
                continue

        return signals

    # ================================================================
    # 内部方法
    # ================================================================

    def _load_forecasts(self, files: List[str]) -> list:
        """加载 Forecast JSON 文件, 返回 WeatherForecast 对象列表。"""
        forecasts = []
        try:
            from weather_data_ingestion.models import WeatherForecast
        except ImportError:
            logger.error("无法导入 weather_data_ingestion")
            return []

        # 展开 glob 模式
        expanded_files = []
        for fpath in files:
            if any(c in fpath for c in "*?[]"):
                expanded_files.extend(glob.glob(fpath))
            else:
                expanded_files.append(fpath)

        for fpath in expanded_files:
            if not os.path.exists(fpath):
                logger.warning("文件不存在: %s", fpath)
                continue
            try:
                with open(fpath, "r", encoding="utf-8") as f:
                    data = json.load(f)
                if isinstance(data, list):
                    for item in data:
                        forecasts.append(WeatherForecast.from_dict(item))
                else:
                    forecasts.append(WeatherForecast.from_dict(data))
            except Exception as e:
                logger.error("加载预报失败 %s: %s", fpath, e)

        return forecasts

    def _generate_mock_markets(
        self,
        forecasts: list,
        event_types: List[EventType],
        target_hours: int,
        bias: float = 0.0,
        noise: float = 0.05,
    ) -> List[MarketSnapshot]:
        """用 Mock 数据源生成市场快照 (单源模式)。"""
        if self.mock_provider is None:
            self.mock_provider = MockMarketProvider(
                base_noise=noise, bias=bias, seed=42
            )

        markets: List[MarketSnapshot] = []
        for i, forecast in enumerate(forecasts):
            model_prob = extract_probability(
                forecast, event_types[i].value, target_hours
            )
            if model_prob is None:
                model_prob = 0.5

            location = get_location(forecast)
            token_id = f"mock_{location.replace(',', '_').replace(' ', '_')}_{event_types[i].value}"

            market = self.mock_provider.generate(
                token_id=token_id,
                model_prob=model_prob,
            )
            markets.append(market)

        return markets

    def _generate_mock_markets_multi(
        self,
        multi_forecasts: List[list],
        event_types: List[EventType],
        target_hours: int,
        bias: float = 0.0,
        noise: float = 0.05,
    ) -> List[MarketSnapshot]:
        """用 Mock 数据源生成市场快照 (多源模式)。

        用各源概率的简单平均作为 mock 市场的 base probability,
        然后加 bias 和 noise 偏移模拟市场定价偏差。
        """
        if self.mock_provider is None:
            self.mock_provider = MockMarketProvider(
                base_noise=noise, bias=bias, seed=42
            )

        markets: List[MarketSnapshot] = []
        for i, forecasts in enumerate(multi_forecasts):
            # 提取各源概率, 取平均作为市场 base
            probs = []
            for fc in forecasts:
                p = extract_probability(fc, event_types[i].value, target_hours)
                if p is not None:
                    probs.append(p)

            avg_prob = sum(probs) / len(probs) if probs else 0.5

            location = get_location(forecasts[0])
            token_id = f"mock_ms_{location.replace(',', '_').replace(' ', '_')}_{event_types[i].value}"

            market = self.mock_provider.generate(
                token_id=token_id,
                model_prob=avg_prob,
            )
            markets.append(market)

        return markets

    # ================================================================
    # 真实市场模式 -- market_discovery 注册表
    # ================================================================

    def _build_registry_index(self, registry_path: str) -> dict:
        """从 market_discovery 注册表构建 (location, event_type) -> [MarketInfo] 索引。

        返回空 dict 表示注册表不可用或为空。
        """
        try:
            from market_discovery.models import MarketInfo, normalize_location
            from market_discovery.registry import MarketRegistry
        except ImportError:
            logger.warning("market_discovery 未安装，registry 模式不可用")
            return {}

        try:
            reg = MarketRegistry(registry_path)
            records = reg.all_markets()
        except Exception as e:
            logger.warning("注册表加载失败 (%s): %s", registry_path, e)
            return {}

        index: dict = {}
        for rec in records:
            try:
                m = MarketInfo.from_dict(rec)
            except (ValueError, TypeError) as e:
                logger.debug("跳过无效注册记录: %s", e)
                continue
            key = (normalize_location(m.location), m.event_type)
            index.setdefault(key, []).append(m)

        if not index:
            logger.warning("注册表为空 (%s)，registry 模式将使用占位市场", registry_path)
        return index

    def _registry_market_for(
        self, forecast, event_type: EventType, index: dict
    ) -> Optional[MarketSnapshot]:
        """为单个预报匹配注册表市场，返回 MarketSnapshot。

        匹配键: (归一化城市名, event_type)；多个候选时取流动性最高者。
        """
        if not index:
            return None
        try:
            from market_discovery.models import normalize_location
        except ImportError:
            return None

        loc = normalize_location(get_location(forecast))
        candidates = index.get((loc, event_type.value))
        if not candidates:
            return None
        best = max(candidates, key=lambda m: m.liquidity)
        return MarketSnapshot(**best.to_market_snapshot())

    def _load_registry_markets(
        self,
        forecasts: list,
        event_types: List[EventType],
        registry_path: str,
    ) -> List[MarketSnapshot]:
        """从 market_discovery 注册表加载市场快照（与 forecasts 一一对应）。

        匹配不到真实市场的预报使用占位快照 (yes_price=0.5)，
        保证与策略 evaluate_batch 的 zip 对齐。
        """
        index = self._build_registry_index(registry_path)
        markets: List[MarketSnapshot] = []
        for i, forecast in enumerate(forecasts):
            market = self._registry_market_for(forecast, event_types[i], index)
            if market is None:
                loc = _normalize_location_safe(get_location(forecast))
                logger.warning(
                    "注册表无匹配市场: %s/%s，使用占位 0.5",
                    get_location(forecast), event_types[i].value,
                )
                market = MarketSnapshot(
                    token_id=f"unmatched_{loc}",
                    yes_price=0.5, no_price=0.5,
                )
            markets.append(market)
        return markets

    def _load_registry_markets_multi(
        self,
        multi_forecasts: List[list],
        event_types: List[EventType],
        registry_path: str,
    ) -> List[MarketSnapshot]:
        """多源模式: 用每组第一个预报匹配注册表市场。"""
        index = self._build_registry_index(registry_path)
        markets: List[MarketSnapshot] = []
        for i, forecasts in enumerate(multi_forecasts):
            market = self._registry_market_for(forecasts[0], event_types[i], index)
            if market is None:
                loc = _normalize_location_safe(get_location(forecasts[0]))
                logger.warning(
                    "注册表无匹配市场: %s/%s，使用占位 0.5",
                    get_location(forecasts[0]), event_types[i].value,
                )
                market = MarketSnapshot(
                    token_id=f"unmatched_{loc}",
                    yes_price=0.5, no_price=0.5,
                )
            markets.append(market)
        return markets

    def _load_observations(self, files: List[str]) -> list:
        """加载观测 JSON 文件, 返回 RadarObservation 对象列表。"""
        observations = []
        try:
            from weather_data_ingestion.models import RadarObservation
        except ImportError:
            logger.error("无法导入 weather_data_ingestion.models.RadarObservation")
            return []

        for fpath in files:
            if not os.path.exists(fpath):
                logger.warning("文件不存在: %s", fpath)
                continue
            try:
                with open(fpath, "r", encoding="utf-8") as f:
                    data = json.load(f)
                if isinstance(data, list):
                    for item in data:
                        observations.append(RadarObservation.from_dict(item))
                else:
                    observations.append(RadarObservation.from_dict(data))
            except Exception as e:
                logger.error("加载观测失败 %s: %s", fpath, e)

        return observations

    def _generate_mock_observations(
        self,
        forecasts: list,
        event_types: List[EventType],
        target_hours: int = 24,
        precip_deviation: float = 0.0,
    ) -> list:
        """生成 Mock 观测数据。

        基于预报数据构造观测，通过 precip_deviation 控制偏离程度。
        正值 = 比预报更湿（有降水），负值 = 比预报更干（无降水）。

        Args:
            forecasts: WeatherForecast 列表
            event_types: 事件类型列表
            target_hours: 目标时间窗口
            precip_deviation: 降水偏差 (-1.0~1.0)
        """
        import random
        from weather_data_ingestion.models import RadarObservation

        observations = []
        for i, forecast in enumerate(forecasts):
            # 从预报提取当前概率
            forecast_prob = extract_probability(
                forecast, event_types[i].value, target_hours
            )
            if forecast_prob is None:
                forecast_prob = 0.5

            # 调整后的概率
            adjusted_prob = max(0.0, min(1.0, forecast_prob + precip_deviation))

            # 根据调整后的概率决定是否有降水
            has_precip = random.random() < adjusted_prob

            if has_precip:
                precip_mm = round(max(0.1, adjusted_prob * 10 + random.gauss(0, 1)), 2)
            else:
                precip_mm = 0.0

            # 温度：从预报提取或用默认值
            temp_c = None
            for period in forecast.periods:
                if period.temperature.value_c is not None:
                    temp_c = period.temperature.value_c
                    break
            if temp_c is None:
                temp_c = 20.0

            # 降水类型
            if not has_precip:
                precip_type = "none"
            elif temp_c < -2:
                precip_type = "snow"
            elif temp_c < 2:
                precip_type = "sleet"
            else:
                precip_type = "rain"

            # 能见度
            if precip_mm > 0:
                visibility_km = round(max(1.0, 10.0 - precip_mm), 2)
            else:
                visibility_km = 10.0

            # 湿度
            humidity = round(max(0.3, min(1.0, 0.5 + adjusted_prob * 0.4)), 4)

            obs = RadarObservation(
                station_id=f"mock_{forecast.location_name}",
                station_name=f"Mock Station {forecast.location_name}",
                lat=forecast.lat,
                lon=forecast.lon,
                timestamp=datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                temperature_c=round(temp_c + random.gauss(0, 1), 2),
                precip_last_hour_mm=precip_mm,
                precipitation_type=precip_type,
                wind_speed_kph=round(max(0, 15 + random.gauss(0, 5)), 2),
                wind_dir_deg=round(random.uniform(0, 360), 1),
                visibility_km=visibility_km,
                humidity=humidity,
            )
            observations.append(obs)

        return observations

    def _generate_mock_climatology(
        self,
        forecasts: list,
        event_types: List[EventType],
        years: int = 5,
    ) -> None:
        """生成 Mock 历史气候数据并注入追踪器。

        基于当前预报概率，模拟过去 N 年同期的历史观测。
        每年每月生成 3~5 条观测，概率围绕当前预报概率波动，
        加入季节性变化（夏季降水多/冬季少等）。

        Args:
            forecasts: WeatherForecast 列表
            event_types: 事件类型列表
            years: 模拟年数
        """
        import random

        for i, forecast in enumerate(forecasts):
            location = get_location(forecast)
            event_type = event_types[i]

            # 从预报提取当前概率作为基线
            base_prob = extract_probability(
                forecast, event_type.value, 24
            )
            if base_prob is None:
                base_prob = 0.5

            # 为每个月生成历史数据
            for month in range(1, 13):
                # 季节性调整：基于事件类型
                seasonal_adj = self._seasonal_adjustment(event_type, month, base_prob)
                month_base = max(0.02, min(0.98, base_prob + seasonal_adj))

                # 每月生成 3~5 条观测
                n_obs = random.randint(3, 5) * years
                for _ in range(n_obs):
                    # 在月基线上加随机波动
                    noise = random.gauss(0, 0.08)
                    prob = max(0.0, min(1.0, month_base + noise))
                    self.clim_tracker.add_observation(
                        location, event_type, month, round(prob, 4)
                    )

    @staticmethod
    def _seasonal_adjustment(
        event_type: EventType,
        month: int,
        base_prob: float,
    ) -> float:
        """计算季节性调整值。

        北半球季节性规律（简化模型）:
          - 降水: 夏季(6-8)略高于冬季
          - 温度: 夏季(6-8)高温概率高
          - 雪: 冬季(12-2)降雪概率高
          - 风: 春秋季(3-5, 9-11)风力较大
        """
        if event_type == EventType.RAIN:
            # 夏季多雨，冬季少雨
            if month in (6, 7, 8):
                return 0.10
            elif month in (12, 1, 2):
                return -0.08
            else:
                return 0.0
        elif event_type == EventType.TEMPERATURE:
            # 夏季高温概率高
            if month in (6, 7, 8):
                return 0.15
            elif month in (12, 1, 2):
                return -0.15
            else:
                return 0.0
        elif event_type == EventType.SNOW:
            # 仅冬季有降雪
            if month in (12, 1, 2):
                return 0.30
            elif month in (11, 3):
                return 0.10
            else:
                return -0.15
        elif event_type == EventType.WIND:
            # 春秋多大风
            if month in (3, 4, 5, 10, 11):
                return 0.08
            else:
                return -0.03
        else:
            return 0.0

    def _save_signals(self, signals: List[TradingSignal]):
        """保存信号到磁盘。"""
        out_dir = Path(self.config.signal_output_path)
        out_dir.mkdir(parents=True, exist_ok=True)

        timestamp = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
        for i, signal in enumerate(signals):
            filename = f"{timestamp}_{signal.strategy}_{i:03d}.json"
            filepath = out_dir / filename
            with open(filepath, "w", encoding="utf-8") as f:
                json.dump(signal.to_dict(), f, indent=2, ensure_ascii=False)

        logger.info("已保存 %d 个信号到 %s", len(signals), out_dir)
