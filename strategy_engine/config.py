"""策略引擎配置管理。

通过环境变量和代码两种方式设置参数，优先级: 代码覆写 > 环境变量 > 默认值。
"""

import os
from dataclasses import dataclass, field

from .aggregator.aggregator import AggregatorParams


@dataclass
class DeviationStrategyParams:
    """预报偏差策略参数"""

    # 偏差阈值（sigma 倍数），默认 2.0sigma
    deviation_threshold_sigma: float = 2.0

    # 最小绝对偏差（过滤噪声），低于此值不生成信号
    min_deviation_abs: float = 0.02

    # 滚动窗口大小（用于计算 mu/sigma）
    window_size: int = 100

    # 最小置信度阈值，低于此值不输出信号
    min_confidence: float = 0.5

    # 信号有效期（秒）
    signal_ttl_seconds: int = 300

    # 单市场最大仓位（信号数量硬限制）
    max_position_per_market: float = 1000.0

    # 是否启用偏差追踪持久化
    persist_tracker: bool = True


@dataclass
class MultiSourceStrategyParams:
    """多源验证策略参数"""

    # 源间分歧阈值（sigma 倍数），分歧超过此值才认为异常
    divergence_threshold_sigma: float = 2.0

    # 最小源间分歧（绝对值），低于此值认为源间一致
    min_divergence_abs: float = 0.05

    # 融合概率 vs 市场概率偏差阈值（sigma 倍数）
    market_div_threshold_sigma: float = 1.5

    # 最小融合偏差（绝对值），低于此值不生成信号
    min_market_div_abs: float = 0.03

    # 滚动窗口大小
    window_size: int = 100

    # 最小置信度阈值
    min_confidence: float = 0.5

    # 信号有效期（秒）
    signal_ttl_seconds: int = 300

    # 单市场最大仓位
    max_position_per_market: float = 1000.0

    # 分歧放大因子: 分歧越大, 置信度乘数越高 (0.0~1.0)
    divergence_confidence_boost: float = 0.3

    # 持久化
    persist_tracker: bool = True


@dataclass
class NowcastParams:
    """临近修正策略参数"""

    # 修正窗口（事件前 N 小时内启动修正）
    correction_window_hours: float = 6.0

    # 修正强度（0~1），控制观测数据对预报的修正幅度
    correction_strength: float = 0.5

    # 最大修正幅度（绝对值），防止过度修正
    max_correction: float = 0.3

    # 最小偏差阈值（修正后概率 vs 市场概率）
    min_deviation_abs: float = 0.03

    # 最小置信度阈值
    min_confidence: float = 0.3

    # 信号有效期（秒），临近修正信号应短命
    signal_ttl_seconds: int = 1800  # 30 分钟

    # 单市场最大仓位
    max_position_per_market: float = 500.0


@dataclass
class HistoricalParams:
    """历史统计策略参数"""

    # 预报提前时间衰减因子（小时），lead_hours / 此值 = 气候学权重增量
    lead_time_decay_hours: float = 200.0

    # 预报权重下限（气候学权重上限 = 1 - 此值）
    min_forecast_weight: float = 0.15

    # 预报权重上限（气候学权重下限 = 1 - 此值）
    max_forecast_weight: float = 0.85

    # 气候学平滑窗口（前后各 N 个月加权平均）
    smoothing_window: int = 1

    # 最小气候学样本数，低于此值不发信号
    min_climatology_samples: int = 5

    # 季节性异常置信度放大因子 (0~1)
    seasonal_confidence_boost: float = 0.3

    # 最小偏差阈值（混合概率 vs 市场概率）
    min_deviation_abs: float = 0.03

    # 最小置信度阈值
    min_confidence: float = 0.4

    # 信号有效期（秒），历史统计信号中长期有效
    signal_ttl_seconds: int = 3600  # 1 小时

    # 单市场最大仓位
    max_position_per_market: float = 800.0

    # 持久化
    persist_tracker: bool = True


@dataclass
class StrategyConfig:
    """策略引擎总配置"""

    # 预报偏差策略参数
    deviation: DeviationStrategyParams = field(default_factory=DeviationStrategyParams)

    # 多源验证策略参数
    multi_source: MultiSourceStrategyParams = field(
        default_factory=MultiSourceStrategyParams
    )

    # 临近修正策略参数
    nowcast: NowcastParams = field(default_factory=NowcastParams)

    # 历史统计策略参数
    historical: HistoricalParams = field(default_factory=HistoricalParams)

    # 偏差追踪器存储路径
    tracker_storage_path: str = "./data/tracker"

    # 信号输出路径
    signal_output_path: str = "./data/signals"

    # 聚合器参数
    aggregator: AggregatorParams = field(default_factory=AggregatorParams)

    @classmethod
    def from_env(cls) -> "StrategyConfig":
        """从环境变量加载配置。"""
        deviation = DeviationStrategyParams(
            deviation_threshold_sigma=float(
                os.getenv("DEVIATION_SIGMA", "2.0")
            ),
            min_deviation_abs=float(
                os.getenv("DEVIATION_MIN_ABS", "0.02")
            ),
            window_size=int(
                os.getenv("DEVIATION_WINDOW_SIZE", "100")
            ),
            min_confidence=float(
                os.getenv("DEVIATION_MIN_CONFIDENCE", "0.5")
            ),
            signal_ttl_seconds=int(
                os.getenv("SIGNAL_TTL_SECONDS", "300")
            ),
            max_position_per_market=float(
                os.getenv("MAX_POSITION_PER_MARKET", "1000.0")
            ),
            persist_tracker=os.getenv("PERSIST_TRACKER", "true").lower() != "false",
        )
        multi_source = MultiSourceStrategyParams(
            divergence_threshold_sigma=float(
                os.getenv("DIVERGENCE_SIGMA", "2.0")
            ),
            min_divergence_abs=float(
                os.getenv("DIVERGENCE_MIN_ABS", "0.05")
            ),
            market_div_threshold_sigma=float(
                os.getenv("MARKET_DIV_SIGMA", "1.5")
            ),
            min_market_div_abs=float(
                os.getenv("MARKET_DIV_MIN_ABS", "0.03")
            ),
            window_size=int(
                os.getenv("DIVERGENCE_WINDOW_SIZE", "100")
            ),
            min_confidence=float(
                os.getenv("MULTI_SOURCE_MIN_CONFIDENCE", "0.5")
            ),
            signal_ttl_seconds=int(
                os.getenv("MULTI_SOURCE_TTL", "300")
            ),
            max_position_per_market=float(
                os.getenv("MULTI_SOURCE_MAX_POSITION", "1000.0")
            ),
            divergence_confidence_boost=float(
                os.getenv("DIVERGENCE_CONF_BOOST", "0.3")
            ),
            persist_tracker=os.getenv("PERSIST_DIV_TRACKER", "true").lower() != "false",
        )
        return cls(
            deviation=deviation,
            multi_source=multi_source,
            nowcast=NowcastParams(
                correction_window_hours=float(
                    os.getenv("NOWCAST_WINDOW_HOURS", "6.0")
                ),
                correction_strength=float(
                    os.getenv("NOWCAST_CORRECTION_STRENGTH", "0.5")
                ),
                max_correction=float(
                    os.getenv("NOWCAST_MAX_CORRECTION", "0.3")
                ),
                min_deviation_abs=float(
                    os.getenv("NOWCAST_MIN_DEVIATION", "0.03")
                ),
                min_confidence=float(
                    os.getenv("NOWCAST_MIN_CONFIDENCE", "0.3")
                ),
                signal_ttl_seconds=int(
                    os.getenv("NOWCAST_TTL", "1800")
                ),
                max_position_per_market=float(
                    os.getenv("NOWCAST_MAX_POSITION", "500.0")
                ),
            ),
            historical=HistoricalParams(
                lead_time_decay_hours=float(
                    os.getenv("HIST_LEAD_DECAY_HOURS", "200.0")
                ),
                min_forecast_weight=float(
                    os.getenv("HIST_MIN_FORECAST_WEIGHT", "0.15")
                ),
                max_forecast_weight=float(
                    os.getenv("HIST_MAX_FORECAST_WEIGHT", "0.85")
                ),
                smoothing_window=int(
                    os.getenv("HIST_SMOOTHING_WINDOW", "1")
                ),
                min_climatology_samples=int(
                    os.getenv("HIST_MIN_CLIM_SAMPLES", "5")
                ),
                seasonal_confidence_boost=float(
                    os.getenv("HIST_SEASONAL_CONF_BOOST", "0.3")
                ),
                min_deviation_abs=float(
                    os.getenv("HIST_MIN_DEVIATION", "0.03")
                ),
                min_confidence=float(
                    os.getenv("HIST_MIN_CONFIDENCE", "0.4")
                ),
                signal_ttl_seconds=int(
                    os.getenv("HIST_TTL", "3600")
                ),
                max_position_per_market=float(
                    os.getenv("HIST_MAX_POSITION", "800.0")
                ),
                persist_tracker=os.getenv("PERSIST_CLIM_TRACKER", "true").lower() != "false",
            ),
            tracker_storage_path=os.getenv(
                "TRACKER_STORAGE_PATH", "./data/tracker"
            ),
            signal_output_path=os.getenv(
                "SIGNAL_OUTPUT_PATH", "./data/signals"
            ),
            aggregator=AggregatorParams.from_env(),
        )
