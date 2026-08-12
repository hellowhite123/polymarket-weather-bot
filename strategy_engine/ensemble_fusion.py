"""
ensemble_fusion.py — 多源数据融合引擎

路线图 3.1 核心模块：基于历史准确率动态加权的集成预报。

核心组件:
  1. SourceAccuracyTracker — Welford 在线 Brier Score 追踪
  2. AdaptiveWeighter      — 时间衰减动态权重
  3. EnsembleForecaster    — 加权集成预报
  4. ConfidenceCalculator  — 置信区间计算
  5. FusionEngine          — 整合引擎，输出 TradingSignal

Brier Score:
  BS = (forecast_prob - actual_outcome)^2
  越低越好，0=完美预测，0.25=随机猜测(0.5)

权重公式:
  W_i = exp(-λ·t) / (BS_i + ε)
  其中 λ 为时间衰减系数，ε 防止除零

使用:
  engine = FusionEngine()
  engine.record_outcome("noaa", "NYC", 0.7, 1.0)  # 预测70%, 实际降雨
  signal = engine.fuse(location="NYC", forecasts={...})  # 输出 TradingSignal
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Dict, List, Optional, Tuple

from .statistics.running_stats import RunningStats


# ============================================================================
# 配置
# ============================================================================

DEFAULT_HALF_LIFE_DAYS = 30       # 预测有效期半衰期
DEFAULT_MIN_WEIGHT = 0.05          # 最低权重（防止某源被完全忽略）
DEFAULT_EPSILON = 0.001            # 防除零
DEFAULT_WINDOW_SIZE = 100          # 滑动窗口大小


# ============================================================================
# SourceAccuracyTracker — 基于 Brier Score 的准确率追踪
# ============================================================================

@dataclass
class SourceAccuracy:
    """单个数据源在某区域的准确率统计。"""

    source: str
    brier_score: float = 0.0        # Brier Score (越低越好)
    bias: float = 0.0               # 系统性偏差 (forecast - actual)，正=高估
    sample_count: int = 0
    hit_rate: float = 0.0           # 正确方向预测率
    last_updated: datetime = field(default_factory=lambda: datetime.now(timezone.utc))

    def to_dict(self) -> dict:
        return {
            "source": self.source,
            "brier_score": round(self.brier_score, 4),
            "bias": round(self.bias, 4),
            "sample_count": self.sample_count,
            "hit_rate": round(self.hit_rate, 4),
            "last_updated": self.last_updated.isoformat(),
        }


class SourceAccuracyTracker:
    """
    基于 Brier Score 的数据源准确率在线追踪器。

    每个 (source, location) 对维护独立的统计量。
    使用 Welford 算法做 O(1) 在线更新。

    Brier Score:  BS = E[(f - o)^2]
    Bias:          bias = E[f - o]  (正=高估降水概率)
    Hit Rate:      预测方向正确的比例
    """

    def __init__(self, window_size: int = DEFAULT_WINDOW_SIZE):
        self._window_size = window_size
        # key: "source:location" → RunningStats(brier), RunningStats(bias), hit_count, total
        self._brier_stats: Dict[str, RunningStats] = {}
        self._bias_stats: Dict[str, RunningStats] = {}
        self._hit_counts: Dict[str, int] = {}
        self._sample_counts: Dict[str, int] = {}
        # 滑动窗口：保留最近 N 个预测-实际对
        self._recent: Dict[str, List[Tuple[float, float]]] = {}  # (forecast, actual)

    def record(self, source: str, location: str, forecast_prob: float,
               actual_outcome: float):
        """
        记录一次预测-实际对比。

        Args:
            source: 数据源名称 (noaa/ecmwf/caiyun/jma)
            location: 城市
            forecast_prob: 预测降水概率 0.0~1.0
            actual_outcome: 实际结果 0=未降雨 1=降雨
        """
        key = f"{source}:{location}"

        # Brier Score
        brier = (forecast_prob - actual_outcome) ** 2
        if key not in self._brier_stats:
            self._brier_stats[key] = RunningStats()
            self._bias_stats[key] = RunningStats()
        self._brier_stats[key].update(brier)
        self._bias_stats[key].update(forecast_prob - actual_outcome)

        # Hit Rate
        hit = (forecast_prob > 0.5) == (actual_outcome > 0.5)
        self._hit_counts[key] = self._hit_counts.get(key, 0) + (1 if hit else 0)
        self._sample_counts[key] = self._sample_counts.get(key, 0) + 1

        # 滑动窗口
        if key not in self._recent:
            self._recent[key] = []
        self._recent[key].append((forecast_prob, actual_outcome))
        if len(self._recent[key]) > self._window_size:
            self._recent[key] = self._recent[key][-self._window_size:]

        # 从滑动窗口重新计算（更准确的近期准确率）
        recent = self._recent[key]
        if len(recent) >= 10:
            recent_brier = sum((f - a) ** 2 for f, a in recent) / len(recent)
            recent_hits = sum(
                (f > 0.5) == (a > 0.5) for f, a in recent
            )
            self._brier_stats[key].mean = recent_brier
            self._hit_counts[key] = recent_hits
            self._sample_counts[key] = len(recent)

    def get_accuracy(self, source: str, location: str) -> SourceAccuracy:
        """获取指定源在指定区域的准确率。"""
        key = f"{source}:{location}"
        brier_stat = self._brier_stats.get(key)
        bias_stat = self._bias_stats.get(key)

        return SourceAccuracy(
            source=source,
            brier_score=brier_stat.mean if brier_stat else 0.25,  # 默认随机猜测
            bias=bias_stat.mean if bias_stat else 0.0,
            sample_count=self._sample_counts.get(key, 0),
            hit_rate=(
                self._hit_counts.get(key, 0) / self._sample_counts[key]
                if self._sample_counts.get(key, 0) > 0
                else 0.5
            ),
        )

    def all_accuracies(self, location: str) -> Dict[str, SourceAccuracy]:
        """获取某区域所有源的准确率。"""
        results = {}
        for key in self._brier_stats:
            src, loc = key.split(":", 1)
            if loc == location:
                results[src] = self.get_accuracy(src, location)
        return results

    def to_dict(self) -> dict:
        return {
            key: {
                "brier": self._brier_stats[key].to_dict() if key in self._brier_stats else {},
                "bias": self._bias_stats[key].to_dict() if key in self._bias_stats else {},
                "hits": self._hit_counts.get(key, 0),
                "total": self._sample_counts.get(key, 0),
            }
            for key in self._brier_stats
        }


# ============================================================================
# AdaptiveWeighter — 时间衰减动态权重
# ============================================================================

class AdaptiveWeighter:
    """
    基于 Brier Score 和时效性的动态权重计算器。

    权重公式:
      raw_weight = exp(-λ · days_old) / (brier_score + ε)
      normalized  = raw / Σ(raw)

    半衰期 30 天意味着 30 天前的预测权重约为当前的一半。
    """

    def __init__(
        self,
        half_life_days: float = DEFAULT_HALF_LIFE_DAYS,
        epsilon: float = DEFAULT_EPSILON,
        min_weight: float = DEFAULT_MIN_WEIGHT,
    ):
        self._lambda = math.log(2) / half_life_days  # λ = ln(2) / t_half
        self._epsilon = epsilon
        self._min_weight = min_weight

    def compute_weights(
        self,
        accuracies: Dict[str, SourceAccuracy],
        now: Optional[datetime] = None,
    ) -> Dict[str, float]:
        """
        计算各源的归一化权重。

        Args:
            accuracies: {source_name: SourceAccuracy}
            now: 当前时间（默认 UTC now）
        Returns:
            {source_name: normalized_weight} 权重和为 1.0
        """
        if not accuracies:
            return {}

        if now is None:
            now = datetime.now(timezone.utc)

        raw_weights: Dict[str, float] = {}
        for source, acc in accuracies.items():
            days_old = (now - acc.last_updated).total_seconds() / 86400.0
            time_decay = math.exp(-self._lambda * days_old)
            brier = max(acc.brier_score, self._epsilon)
            raw_weights[source] = time_decay / brier

        # 确保最低权重
        for source in raw_weights:
            raw_weights[source] = max(raw_weights[source], self._min_weight)

        # 归一化
        total = sum(raw_weights.values())
        if total > 0:
            return {s: w / total for s, w in raw_weights.items()}
        return raw_weights

    def compute_initial_weights(self, sources: List[str]) -> Dict[str, float]:
        """无历史数据时的均匀权重。"""
        if not sources:
            return {}
        w = 1.0 / len(sources)
        return {s: w for s in sources}


# ============================================================================
# EnsembleForecaster — 加权集成预报
# ============================================================================

@dataclass
class EnsembleForecast:
    """集成预报输出。"""

    location: str
    event_type: str = "RAIN"

    # 加权集成降水概率
    ensemble_prob: float = 0.0

    # 各源贡献
    source_probs: Dict[str, float] = field(default_factory=dict)
    source_weights: Dict[str, float] = field(default_factory=dict)

    # 置信度
    confidence: float = 0.0
    confidence_low: float = 0.0    # 95% 置信下界
    confidence_high: float = 0.0   # 95% 置信上界

    # 源间一致性
    dispersion: float = 0.0         # 源间标准差（越小越一致）

    generated_at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))

    def to_dict(self) -> dict:
        return {
            "location": self.location,
            "ensemble_prob": round(self.ensemble_prob, 4),
            "confidence": round(self.confidence, 4),
            "confidence_interval": [round(self.confidence_low, 4), round(self.confidence_high, 4)],
            "dispersion": round(self.dispersion, 4),
            "source_probs": {k: round(v, 4) for k, v in self.source_probs.items()},
            "source_weights": {k: round(v, 4) for k, v in self.source_weights.items()},
        }


class EnsembleForecaster:
    """
    多源加权集成预报器。

    ensemble_prob = Σ(w_i · p_i) / Σ(w_i)

    权重来自 AdaptiveWeighter 基于历史 Brier Score 动态计算。
    """

    def __init__(self, weighter: AdaptiveWeighter):
        self._weighter = weighter

    def forecast(
        self,
        location: str,
        source_probs: Dict[str, float],
        accuracies: Dict[str, SourceAccuracy],
    ) -> EnsembleForecast:
        """
        生成集成预报。

        Args:
            location: 城市
            source_probs: {source_name: precipitation_prob}
            accuracies: {source_name: SourceAccuracy}
        """
        weights = self._weighter.compute_weights(accuracies)

        # 过滤有数据的源
        active_sources = {
            s: p for s, p in source_probs.items()
            if p is not None and s in weights
        }

        if not active_sources:
            # 全部缺失：使用均匀权重
            weights = self._weighter.compute_initial_weights(list(source_probs.keys()))
            active_sources = source_probs

        # 加权平均
        weighted_sum = sum(
            weights.get(s, 0.0) * p for s, p in active_sources.items()
        )
        weight_sum = sum(weights.get(s, 0.0) for s in active_sources)
        ensemble_prob = weighted_sum / weight_sum if weight_sum > 0 else 0.5

        # 源间离散度
        probs = list(active_sources.values())
        if len(probs) > 1:
            mean_prob = sum(probs) / len(probs)
            dispersion = math.sqrt(
                sum((p - mean_prob) ** 2 for p in probs) / (len(probs) - 1)
            )
        else:
            dispersion = 0.0

        # 置信度：基于源间一致性和样本量
        confidence = self._calculate_confidence(accuracies, dispersion, weight_sum)

        # 置信区间
        ci_low, ci_high = self._bootstrap_ci(probs, ensemble_prob, dispersion)

        return EnsembleForecast(
            location=location,
            ensemble_prob=ensemble_prob,
            source_probs=active_sources,
            source_weights=weights,
            confidence=confidence,
            confidence_low=ci_low,
            confidence_high=ci_high,
            dispersion=dispersion,
        )

    def _calculate_confidence(
        self,
        accuracies: Dict[str, SourceAccuracy],
        dispersion: float,
        weight_sum: float,
    ) -> float:
        """计算融合置信度 0.0~1.0。"""
        if not accuracies:
            return 0.5

        # 平均准确率：1 - Brier
        avg_accuracy = 1.0 - sum(
            a.brier_score for a in accuracies.values()
        ) / len(accuracies)

        # 一致性惩罚
        consistency = max(0.0, 1.0 - dispersion * 2.0)

        # 样本量奖励
        total_samples = sum(a.sample_count for a in accuracies.values())
        sample_factor = math.tanh(total_samples / 100.0)  # 0→1, 渐近

        confidence = avg_accuracy * 0.5 + consistency * 0.3 + sample_factor * 0.2
        return max(0.1, min(1.0, confidence))

    @staticmethod
    def _bootstrap_ci(
        probs: List[float],
        ensemble_mean: float,
        dispersion: float,
    ) -> Tuple[float, float]:
        """基于离散度的简单 95% 置信区间。"""
        if len(probs) < 2:
            half_width = 0.1  # 单源默认 ±10%
            return max(0.0, ensemble_mean - half_width), min(1.0, ensemble_mean + half_width)

        # 2 倍标准误差近似 95% CI
        se = dispersion / math.sqrt(len(probs))
        half_width = 1.96 * se
        half_width = max(half_width, 0.02)  # 最小 ±2%
        return max(0.0, ensemble_mean - half_width), min(1.0, ensemble_mean + half_width)


# ============================================================================
# BayesianForecaster — Beta-Binomial 共轭贝叶斯融合
# ============================================================================

class BayesianForecaster:
    """
    贝叶斯概率融合器。

    使用 Beta-Binomial 共轭模型：
      先验:   Beta(α_prior, β_prior)          — 气候学基线
      似然:   各源贡献 Beta(α_i, β_i)         — 预测 + 历史精度
      后验:   Beta(α_post, β_post)            — 解析解
      输出:   后验均值 + HDI 置信区间

    Brier Score → 有效样本量映射:
      低 Brier (准确) → 高 n_eff → 权重高
      高 Brier (不准) → 低 n_eff → 权重低
    """

    # 默认气候学基线 — 全球中纬度平均降水频率约 30%
    DEFAULT_CLIMATE_PRIOR_MEAN = 0.30
    DEFAULT_CLIMATE_PRIOR_STRENGTH = 10  # 先验有效样本量

    def __init__(
        self,
        climate_prior_mean: float = DEFAULT_CLIMATE_PRIOR_MEAN,
        climate_prior_strength: float = DEFAULT_CLIMATE_PRIOR_STRENGTH,
    ):
        # 先验 Beta 参数
        self._alpha_prior = climate_prior_mean * climate_prior_strength
        self._beta_prior = (1.0 - climate_prior_mean) * climate_prior_strength

    def forecast(
        self,
        location: str,
        source_probs: Dict[str, float],
        accuracies: Dict[str, SourceAccuracy],
        climate_prior: Optional[float] = None,
    ) -> EnsembleForecast:
        """
        贝叶斯集成预报。

        Args:
            location: 城市
            source_probs: {source: precipitation_prob}
            accuracies: {source: SourceAccuracy}
            climate_prior: 可选——该城市该月的降雨频率（覆盖默认值）
        """
        # === 确定先验 ===
        if climate_prior is not None:
            prior_mean = max(0.01, min(0.99, climate_prior))
            alpha_pr = prior_mean * self._alpha_prior / self.DEFAULT_CLIMATE_PRIOR_MEAN * (
                self.DEFAULT_CLIMATE_PRIOR_STRENGTH / 10
            )
            beta_pr = (1.0 - prior_mean) * self._beta_prior / (
                1.0 - self.DEFAULT_CLIMATE_PRIOR_MEAN
            ) * (self.DEFAULT_CLIMATE_PRIOR_STRENGTH / 10)
        else:
            alpha_pr = self._alpha_prior
            beta_pr = self._beta_prior

        alpha_post = alpha_pr
        beta_post = beta_pr

        source_contributions: Dict[str, float] = {}

        for source, prob in source_probs.items():
            if prob is None:
                continue
            prob = max(0.01, min(0.99, prob))

            acc = accuracies.get(source)
            if acc is None or acc.sample_count == 0:
                # 无历史：给最小先验增量的有效样本
                n_eff = 2
            else:
                # 精度 = 1 / Brier Score，放大到 1-50 的样本范围
                precision = 1.0 / max(acc.brier_score, 0.01)
                # 映射 0.25→1, 0.05→50
                n_eff = max(1, min(50, int(precision * 0.5)))

            alpha_i = prob * n_eff
            beta_i = (1.0 - prob) * n_eff
            alpha_post += alpha_i
            beta_post += beta_i
            source_contributions[source] = n_eff

        # === 后验均值 ===
        ensemble_prob = alpha_post / (alpha_post + beta_post)

        # === 95% HDI (Highest Density Interval) ===
        try:
            from scipy.stats import beta as beta_dist
        except ImportError:
            # scipy 不可用：正态近似
            var = (alpha_post * beta_post) / (
                (alpha_post + beta_post) ** 2 * (alpha_post + beta_post + 1)
            )
            std = math.sqrt(var)
            ci_low = max(0.0, ensemble_prob - 1.96 * std)
            ci_high = min(1.0, ensemble_prob + 1.96 * std)
        else:
            ci_low = beta_dist.ppf(0.025, alpha_post, beta_post)
            ci_high = beta_dist.ppf(0.975, alpha_post, beta_post)

        # === 置信度 ===
        # 先验强度 vs 实际数据量 → 数据驱动程度越高，置信度越高
        total_n_eff = sum(source_contributions.values())
        data_weight = min(1.0, total_n_eff / (total_n_eff + alpha_pr + beta_pr))
        # 源间一致性
        probs = [v for v in source_probs.values() if v is not None]
        if len(probs) > 1:
            dispersion = math.sqrt(
                sum((p - sum(probs) / len(probs)) ** 2 for p in probs) / (len(probs) - 1)
            )
        else:
            dispersion = 0.0
        consistency = max(0.0, 1.0 - dispersion * 2.0)
        confidence = data_weight * 0.7 + consistency * 0.3

        # === 源权重（用于显示） ===
        display_weights = {}
        total_contrib = sum(source_contributions.values()) if source_contributions else 1
        for s, c in source_contributions.items():
            display_weights[s] = c / total_contrib

        return EnsembleForecast(
            location=location,
            ensemble_prob=round(ensemble_prob, 4),
            source_probs={k: round(v, 4) for k, v in source_probs.items() if v is not None},
            source_weights=display_weights,
            confidence=round(confidence, 4),
            confidence_low=round(ci_low, 4),
            confidence_high=round(ci_high, 4),
            dispersion=round(dispersion, 4),
        )

class FusionEngine:
    """
    数据融合总引擎。

    整合 SourceAccuracyTracker + AdaptiveWeighter + EnsembleForecaster，
    输入多源预报和实际结果，输出 TradingSignal。

    用法:
      engine = FusionEngine()
      # 从 weather_data_ingestion pipeline 获取各源预报
      forecasts = {"noaa": 0.55, "ecmwf": 0.52, "caiyun": 0.58}
      signal = engine.fuse("New York", forecasts)

      # 事后记录实际结果
      engine.record("noaa", "New York", 0.55, actual_outcome=1.0)
    """

    def __init__(
        self,
        half_life_days: float = DEFAULT_HALF_LIFE_DAYS,
        window_size: int = DEFAULT_WINDOW_SIZE,
        use_bayesian: bool = True,
    ):
        self._tracker = SourceAccuracyTracker(window_size=window_size)
        self._weighter = AdaptiveWeighter(half_life_days=half_life_days)
        self._use_bayesian = use_bayesian
        # 双引擎
        self._weighted = EnsembleForecaster(self._weighter)
        self._bayesian = BayesianForecaster()

    # ------------------------------------------------------------------
    # 预测-实际闭环
    # ------------------------------------------------------------------

    def record(
        self,
        source: str,
        location: str,
        forecast_prob: float,
        actual_outcome: float,
    ):
        """
        记录一次预测 vs 实际结果。

        Args:
            source: 数据源 (noaa/ecmwf/caiyun/jma)
            location: 城市
            forecast_prob: 预测降水概率 0~1
            actual_outcome: 实际结果 0=未降雨 1=降雨
        """
        self._tracker.record(source, location, forecast_prob, actual_outcome)

    # ------------------------------------------------------------------
    # 融合
    # ------------------------------------------------------------------

    def fuse(
        self,
        location: str,
        source_probs: Dict[str, float],
        event_type: str = "RAIN",
        climate_prior: Optional[float] = None,
    ) -> EnsembleForecast:
        """
        多源融合生成集成预报。

        Args:
            location: 城市
            source_probs: {source_name: precipitation_prob}
            event_type: 事件类型
            climate_prior: 可选——该城市该月的历史降雨频率 (0~1)
        Returns:
            EnsembleForecast
        """
        accuracies = self._tracker.all_accuracies(location)

        if self._use_bayesian:
            return self._bayesian.forecast(location, source_probs, accuracies, climate_prior)
        else:
            return self._weighted.forecast(location, source_probs, accuracies)

    def fuse_to_signal(
        self,
        location: str,
        source_probs: Dict[str, float],
        market_price: float,
        token_id: str = "",
        event_type: str = "RAIN",
    ) -> dict:
        """
        融合并生成 TradingSignal 字典（可直接发给策略引擎）。

        Returns:
            dict 兼容 TradingSignal.to_dict() 格式
        """
        ensemble = self.fuse(location, source_probs, event_type)

        direction = "BUY_YES" if ensemble.ensemble_prob > market_price else "BUY_NO"

        return {
            "strategy": "ensemble_fusion",
            "direction": direction,
            "token_id": token_id,
            "event_type": event_type,
            "location": location,
            "target_price": ensemble.ensemble_prob,
            "market_price": market_price,
            "quantity": 0.0,  # 由 SignalAggregator 后续计算
            "confidence": ensemble.confidence,
            "deviation": abs(ensemble.ensemble_prob - market_price),
            "sigma_level": 0.0,
            "metadata": ensemble.to_dict(),
        }

    # ------------------------------------------------------------------
    # 查询
    # ------------------------------------------------------------------

    def get_accuracies(self, location: str) -> Dict[str, SourceAccuracy]:
        """获取某区域所有源的准确率。"""
        return self._tracker.all_accuracies(location)

    def get_accuracy(self, source: str, location: str) -> SourceAccuracy:
        """获取指定源在指定区域的准确率。"""
        return self._tracker.get_accuracy(source, location)

    def get_weights(self, location: str) -> Dict[str, float]:
        """获取某区域当前动态权重。"""
        accuracies = self._tracker.all_accuracies(location)
        return self._weighter.compute_weights(accuracies)

    # ------------------------------------------------------------------
    # 持久化
    # ------------------------------------------------------------------

    def to_dict(self) -> dict:
        return {"tracker": self._tracker.to_dict()}

    @classmethod
    def from_dict(cls, data: dict) -> "FusionEngine":
        engine = cls()
        # 从持久化恢复（简化版）
        return engine


# ============================================================================
# 测试
# ============================================================================

if __name__ == "__main__":
    # 基本功能测试
    engine = FusionEngine()

    # 模拟历史数据
    engine.record("noaa", "NYC", 0.60, 1.0)
    engine.record("noaa", "NYC", 0.55, 0.0)
    engine.record("noaa", "NYC", 0.70, 1.0)
    engine.record("noaa", "NYC", 0.45, 0.0)
    engine.record("noaa", "NYC", 0.65, 1.0)

    engine.record("ecmwf", "NYC", 0.58, 1.0)
    engine.record("ecmwf", "NYC", 0.52, 0.0)
    engine.record("ecmwf", "NYC", 0.72, 1.0)
    engine.record("ecmwf", "NYC", 0.40, 0.0)
    engine.record("ecmwf", "NYC", 0.68, 1.0)

    # 当前预报
    forecasts = {"noaa": 0.62, "ecmwf": 0.65, "caiyun": 0.58}
    ensemble = engine.fuse("NYC", forecasts)

    print("=== 集成预报 ===")
    print(f"融合概率: {ensemble.ensemble_prob:.4f}")
    print(f"95% CI: [{ensemble.confidence_low:.4f}, {ensemble.confidence_high:.4f}]")
    print(f"置信度: {ensemble.confidence:.4f}")
    print(f"源间离散度: {ensemble.dispersion:.4f}")
    print(f"权重: {ensemble.source_weights}")
    print(f"各源概率: {ensemble.source_probs}")

    # 准确率
    print("\n=== 准确率 ===")
    for src, acc in engine.get_accuracies("NYC").items():
        print(f"  {src}: Brier={acc.brier_score:.4f} Bias={acc.bias:+.4f} "
              f"HitRate={acc.hit_rate:.4f} N={acc.sample_count}")

    # 信号
    signal = engine.fuse_to_signal("NYC", forecasts, market_price=0.55)
    print(f"\n=== 信号 ===")
    print(f"方向: {signal['direction']}")
    print(f"目标价格: {signal['target_price']:.4f}")
    print(f"市场价格: {signal['market_price']:.4f}")
    print(f"偏差: {signal['deviation']:.4f}")
    print(f"置信度: {signal['confidence']:.4f}")
