"""test_ensemble_fusion.py — P1-009 集合校准（Ensemble Calibration）验证。

验证 FusionEngine 的校准性质：
1. Brier Score 正确追踪预测准确率
2. 动态权重：准确源权重 > 不准确源权重
3. 集成概率在 [0,1] 且置信区间覆盖集成概率
4. 贝叶斯融合在历史数据充足时收敛到数据驱动值
"""

from strategy_engine.ensemble_fusion import (
    AdaptiveWeighter,
    FusionEngine,
    SourceAccuracyTracker,
)


def test_brier_score_tracks_accuracy():
    tracker = SourceAccuracyTracker()
    # 完美预测：预报 0.9，实际降雨
    for _ in range(20):
        tracker.record("good", "NYC", 0.9, 1.0)
    # 差预测：预报 0.9，实际未降雨
    for _ in range(20):
        tracker.record("bad", "NYC", 0.9, 0.0)

    good = tracker.get_accuracy("good", "NYC")
    bad = tracker.get_accuracy("bad", "NYC")

    # 完美预测 Brier ≈ 0.01，差预测 Brier ≈ 0.81
    assert good.brier_score < bad.brier_score
    assert good.hit_rate > bad.hit_rate


def test_accurate_source_gets_higher_weight():
    engine = FusionEngine(use_bayesian=False)
    # 记录历史：noaa 准确（预报对），ecmwf 不准（预报错）
    for _ in range(30):
        engine.record("noaa", "NYC", 0.9, 1.0)
        engine.record("ecmwf", "NYC", 0.9, 0.0)

    weights = engine.get_weights("NYC")
    assert "noaa" in weights and "ecmwf" in weights
    assert weights["noaa"] > weights["ecmwf"], f"准确源应获更高权重: {weights}"


def test_fusion_ensemble_prob_in_range_and_ci_covers():
    engine = FusionEngine()
    forecast = engine.fuse("NYC", {"noaa": 0.55, "ecmwf": 0.62})
    assert 0.0 <= forecast.ensemble_prob <= 1.0
    assert forecast.confidence_low <= forecast.ensemble_prob <= forecast.confidence_high


def test_bayesian_fusion_converges_to_data():
    engine = FusionEngine(use_bayesian=True)
    # 两个源都预报高概率，且历史都准确 → 后验应偏向高概率
    for _ in range(30):
        engine.record("noaa", "NYC", 0.9, 1.0)
        engine.record("ecmwf", "NYC", 0.8, 1.0)

    forecast = engine.fuse("NYC", {"noaa": 0.9, "ecmwf": 0.8})
    # 数据驱动后验应显著高于默认气候先验 0.30
    assert forecast.ensemble_prob > 0.5, f"贝叶斯融合应收敛到高概率: {forecast.ensemble_prob}"


def test_adaptive_weighter_uniform_without_history():
    w = AdaptiveWeighter()
    weights = w.compute_initial_weights(["noaa", "ecmwf", "caiyun"])
    assert abs(sum(weights.values()) - 1.0) < 1e-9
    assert abs(weights["noaa"] - 1.0 / 3.0) < 1e-9
