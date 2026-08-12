"""临近修正策略。

核心逻辑:
  1. 在事件截止前 6 小时内启动（临近修正窗口）
  2. 用实时观测数据（RadarObservation）计算实况信号
  3. 对比实况信号 vs 预报概率，计算修正值
  4. 修正后概率 vs 市场隐含概率 -> 偏差 -> 信号

与其他策略的区别:
  - 预报偏差策略：气象模型概率 vs 市场隐含概率（统计偏差）
  - 多源验证策略：多源融合概率 vs 市场隐含概率（源间分歧）
  - 临近修正策略：实况观测修正后的概率 vs 市场隐含概率（预报准确性偏差）

时间权重设计:
  - 事件前 6 小时：time_weight = 0.0（几乎不修正）
  - 事件前 3 小时：time_weight = 0.5
  - 事件前 1 小时：time_weight = 0.83
  - 事件前 0 小时：time_weight = 1.0（最大修正）
  - 越临近截止，实况数据越有代表性

修正方向:
  - 实况比预报"更湿"（已在下雨但预报说不会）-> 上修降水概率 -> BUY_YES
  - 实况比预报"更干"（没下雨且能见度好，但预报说会下）-> 下修降水概率 -> BUY_NO
"""

import logging
import math
from datetime import datetime, timedelta, timezone
from typing import List, Optional

from ..models import (
    EventType,
    MarketSnapshot,
    SignalDirection,
    TradingSignal,
)
from .base import BaseStrategy
from .forecast_utils import extract_probability, get_location

logger = logging.getLogger(__name__)


class NowcastCorrectionStrategy(BaseStrategy):
    """临近修正策略。

    在事件截止前 correction_window_hours 小时内，用实时观测数据
    修正预报概率。修正后概率与市场隐含概率的偏差超过阈值时生成信号。

    用法:
        strategy = NowcastCorrectionStrategy(params)
        signal = strategy.evaluate(
            forecast=forecast,
            observation=observation,
            market=market,
            event_type=EventType.RAIN,
            event_deadline=deadline,
        )
    """

    def __init__(self, params=None):
        """初始化。

        Args:
            params: NowcastParams 配置。为 None 时使用默认值。
        """
        super().__init__(name="nowcast_correction")
        # 延迟导入避免循环依赖
        if params is None:
            from ..config import NowcastParams
            params = NowcastParams()
        self.params = params

    # ================================================================
    # 公共 API
    # ================================================================

    async def evaluate(self) -> List[TradingSignal]:
        """抽象方法实现（单次调用请用 evaluate / evaluate_batch）。"""
        return []

    def evaluate(
        self,
        forecast,
        observation,
        market: MarketSnapshot,
        event_type: EventType,
        event_deadline: datetime,
        target_hours_ahead: int = 24,
    ) -> Optional[TradingSignal]:
        """评估单个 (预报, 实况, 市场) 三元组。

        Args:
            forecast: WeatherForecast 预报数据
            observation: RadarObservation 实况观测数据
            market: MarketSnapshot 市场快照
            event_type: 事件类型（目前支持 RAIN / SNOW）
            event_deadline: 事件截止时间（UTC）
            target_hours_ahead: 预报目标时间窗口（小时）

        Returns:
            TradingSignal 或 None（未触发阈值）
        """
        now = datetime.now(timezone.utc)

        # 确保 event_deadline 有时区信息
        if event_deadline.tzinfo is None:
            event_deadline = event_deadline.replace(tzinfo=timezone.utc)

        # Step 1: 检查时间窗口
        hours_to_deadline = (event_deadline - now).total_seconds() / 3600.0
        if hours_to_deadline < 0:
            logger.debug("事件已过期: %s", event_deadline.isoformat())
            return None
        if hours_to_deadline >= self.params.correction_window_hours:
            logger.debug(
                "距截止 %.1fh >= 修正窗口 %.1fh，跳过",
                hours_to_deadline,
                self.params.correction_window_hours,
            )
            return None

        time_weight = 1.0 - (hours_to_deadline / self.params.correction_window_hours)

        # Step 2: 从预报提取目标概率
        forecast_prob = extract_probability(
            forecast, event_type.value, target_hours_ahead
        )
        if forecast_prob is None:
            logger.debug(
                "无法从预报提取概率: %s, event=%s",
                forecast.location_name,
                event_type.value,
            )
            return None

        # Step 3: 从观测数据计算实况信号
        obs_signal = self._compute_observation_signal(observation, event_type)
        if obs_signal is None:
            logger.debug("无法从观测数据提取信号: %s", observation.station_id)
            return None

        # Step 4: 计算修正值
        correction = (obs_signal - forecast_prob) * time_weight * self.params.correction_strength
        # 限制最大修正幅度
        correction = max(
            -self.params.max_correction,
            min(self.params.max_correction, correction),
        )

        # Step 5: 修正后概率
        corrected_prob = max(0.0, min(1.0, forecast_prob + correction))

        # Step 6: 与市场比较
        market_prob = market.implied_probability
        deviation = corrected_prob - market_prob

        if abs(deviation) < self.params.min_deviation_abs:
            logger.debug(
                "修正后偏差 %.4f < 最小阈值 %.4f，跳过",
                deviation,
                self.params.min_deviation_abs,
            )
            return None

        # Step 7: 生成信号
        direction = SignalDirection.BUY_YES if deviation > 0 else SignalDirection.BUY_NO
        confidence = self._calculate_confidence(
            correction, time_weight, hours_to_deadline
        )
        quantity = self._calculate_quantity(deviation, time_weight)

        if confidence < self.params.min_confidence:
            logger.debug(
                "置信度 %.3f < 最小阈值 %.3f，丢弃",
                confidence,
                self.params.min_confidence,
            )
            return None

        ttl = timedelta(seconds=self.params.signal_ttl_seconds)
        location = get_location(forecast)

        signal = TradingSignal(
            strategy=self.name,
            direction=direction,
            token_id=market.token_id,
            event_type=event_type,
            location=location,
            target_price=round(corrected_prob, 4),
            market_price=round(market_prob, 4),
            quantity=quantity,
            confidence=confidence,
            deviation=round(deviation, 4),
            sigma_level=0.0,  # 临近修正不用 sigma，用 time_weight 代替
            generated_at=now,
            expires_at=now + ttl,
            metadata={
                "forecast_prob": round(forecast_prob, 4),
                "corrected_prob": round(corrected_prob, 4),
                "obs_signal": round(obs_signal, 4),
                "correction": round(correction, 4),
                "time_weight": round(time_weight, 4),
                "hours_to_deadline": round(hours_to_deadline, 2),
                "station_id": observation.station_id,
                "precip_mm": observation.precip_last_hour_mm,
                "temp_c": observation.temperature_c,
                "target_hours": target_hours_ahead,
            },
        )

        logger.info(
            "临近修正信号: %s %s token=%s dev=%.4f corr=%.4f conf=%.3f",
            direction.value,
            event_type.value,
            market.token_id[:12],
            deviation,
            correction,
            confidence,
        )

        self._increment_counter()
        return signal

    def evaluate_batch(
        self,
        forecasts: list,
        observations: list,
        markets: list[MarketSnapshot],
        event_types: list[EventType],
        event_deadlines: list[datetime],
        target_hours_ahead: int = 24,
    ) -> List[TradingSignal]:
        """批量评估多个三元组。

        Args:
            forecasts: WeatherForecast 列表
            observations: RadarObservation 列表
            markets: MarketSnapshot 列表
            event_types: EventType 列表
            event_deadlines: datetime 列表（事件截止时间，UTC）
            target_hours_ahead: 预报目标时间窗口

        Returns:
            TradingSignal 列表
        """
        n = len(forecasts)
        if not (len(observations) == len(markets) == len(event_types) == len(event_deadlines) == n):
            raise ValueError(
                f"输入长度不一致: forecasts={n}, observations={len(observations)}, "
                f"markets={len(markets)}, event_types={len(event_types)}, "
                f"event_deadlines={len(event_deadlines)}"
            )

        signals: List[TradingSignal] = []
        for i in range(n):
            sig = self.evaluate(
                forecasts[i],
                observations[i],
                markets[i],
                event_types[i],
                event_deadlines[i],
                target_hours_ahead,
            )
            if sig is not None:
                signals.append(sig)

        logger.info(
            "临近修正策略评估完成: %d 个输入, %d 个信号",
            n,
            len(signals),
        )
        return signals

    # ================================================================
    # 实况信号计算
    # ================================================================

    def _compute_observation_signal(
        self,
        observation,
        event_type: EventType,
    ) -> Optional[float]:
        """从观测数据提取实况信号（0.0~1.0）。

        降水事件:
          - 已在下雨: signal = 0.5 + min(precip_mm / 10, 0.5)
          - 无降水: signal = 0.1 * (1 - visibility_factor)
          - 无数据: 返回 None（不修正）

        Args:
            observation: RadarObservation
            event_type: 事件类型

        Returns:
            实况信号 0.0~1.0，或 None（无数据）
        """
        if event_type in (EventType.RAIN, EventType.SNOW):
            return self._precip_signal(observation, event_type)
        elif event_type == EventType.TEMPERATURE:
            return self._temp_signal(observation)
        elif event_type == EventType.WIND:
            return self._wind_signal(observation)
        else:
            return None

    @staticmethod
    def _precip_signal(observation, event_type: EventType) -> Optional[float]:
        """降水实况信号。

        逻辑:
          - 有降水记录 (precip_mm > 0):
            基础信号 0.5 + 强度增量（最多 +0.5）
          - 明确无降水 (precip_mm == 0):
            低信号，但受能见度影响（能见度低可能有毛毛雨）
          - 无数据: 返回 None
        """
        precip_mm = observation.precip_last_hour_mm

        if precip_mm is not None:
            if precip_mm > 0:
                # 有降水：强度越高信号越强
                # 0mm -> 0.5, 5mm -> 0.75, 10mm+ -> 1.0
                return min(1.0, 0.5 + precip_mm / 10.0)
            else:
                # 明确无降水
                # 能见度好 -> 信号低（不太可能下雨）
                # 能见度差 -> 信号略高（可能有雾/毛毛雨未被记录）
                vis_factor = 1.0
                if observation.visibility_km is not None:
                    vis_factor = max(0.0, min(1.0, observation.visibility_km / 10.0))
                # 无降水 + 能见度好 -> 0.02
                # 无降水 + 能见度差 -> 0.10
                return round(0.10 * (1.0 - vis_factor) + 0.02 * vis_factor, 4)

        # 降水数据缺失，尝试用雷达反射率
        if observation.radar_reflectivity_dbz is not None:
            dbz = observation.radar_reflectivity_dbz
            if dbz >= 40:
                return 0.9  # 强回波
            elif dbz >= 20:
                return 0.5 + (dbz - 20) / 40.0  # 0.5~0.9
            else:
                return 0.2  # 弱回波或无回波

        # 完全无降水相关数据
        return None

    @staticmethod
    def _temp_signal(observation) -> Optional[float]:
        """温度实况信号（0.0~1.0）。

        将观测温度映射为概率信号：
          - 温度 < 0°C: 0.9（容易降雪/结冰）
          - 温度 0~10°C: 0.5~0.7
          - 温度 10~25°C: 0.3
          - 温度 > 25°C: 0.1
        """
        if observation.temperature_c is None:
            return None
        t = observation.temperature_c
        if t < 0:
            return 0.9
        elif t < 10:
            return 0.5 + (10 - t) / 10.0 * 0.2  # 0.5~0.7
        elif t < 25:
            return 0.3
        else:
            return 0.1

    @staticmethod
    def _wind_signal(observation) -> Optional[float]:
        """风速实况信号（0.0~1.0）。

        将观测风速映射为概率信号：
          - 风速 > 60 km/h: 1.0（大风事件几乎确定）
          - 风速 30~60 km/h: 0.5~1.0
          - 风速 < 30 km/h: 0.0~0.5
        """
        if observation.wind_speed_kph is None:
            return None
        w = observation.wind_speed_kph
        if w >= 60:
            return 1.0
        elif w >= 30:
            return 0.5 + (w - 30) / 30.0 * 0.5  # 0.5~1.0
        else:
            return w / 30.0 * 0.5  # 0.0~0.5

    # ================================================================
    # 置信度与仓位
    # ================================================================

    def _calculate_confidence(
        self,
        correction: float,
        time_weight: float,
        hours_to_deadline: float,
    ) -> float:
        """计算信号置信度。

        置信度因子:
          1. 修正幅度：修正越大，置信度越高
          2. 时间权重：越临近截止，观测越有代表性
          3. 临近衰减：距截止 < 0.5 小时时降低（信号有效期太短）

        公式:
          base = |correction| / max_correction  (0~1)
          confidence = base * time_weight * decay
        """
        # 修正幅度因子
        correction_ratio = abs(correction) / self.params.max_correction

        # 临近衰减：距截止 < 0.5h 时快速衰减
        if hours_to_deadline < 0.5:
            decay = hours_to_deadline / 0.5  # 0~1
        else:
            decay = 1.0

        raw = correction_ratio * time_weight * decay
        # 平滑映射到 0.3~0.95
        confidence = 0.3 + 0.65 * min(1.0, raw)
        return round(max(0.0, min(1.0, confidence)), 4)

    def _calculate_quantity(self, deviation: float, time_weight: float) -> float:
        """计算建议交易数量。

        临近修正策略的仓位应该比预报偏差策略更保守，
        因为修正窗口短、不确定性高。
        """
        # 基础仓位 = 偏差幅度 * 50 * 时间权重
        base = abs(deviation) * 50.0 * (0.5 + 0.5 * time_weight)
        return round(min(base, self.params.max_position_per_market), 2)
