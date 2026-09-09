# PROBABILITY_MODEL.md

**架构真源** · 版本 v0.1 · 2026-08-25 · 待随审计完善

## 1. 气象概率链路（协议 §24）

```text
Polymarket Market
      │
      v
SettlementTarget  （精确站点 + 官方源）
      │
      v
Forecast Archive  （历史预报 vintage）
      │
      v
Forecast/Observation Pair
      │
      v
Bias Correction  （偏差修正）
      │
      v
EMOS  （Ensemble Model Output Statistics）
      │
      v
Probability Calibration  （概率校准）
      │
      v
P_model + Uncertainty  （预测概率 + 不确定性）
      │
      v
Market Probability
      │
      v
Edge / No-Trade  （是否有边际优势）
      │
      v
OrderIntent
```

## 2. 三条禁止红线

```text
Grid Temperature            ≠  Settlement Temperature
Raw Ensemble Frequency      ≠  Calibrated Probability
Final Forecast              ≠  Historical Forecast Vintage
```

（避免数据穿越 / Lookahead Bias / 未校准概率。）

## 3. 校准与评分

- **Brier Score**：FusionEngine 追踪，用于评估概率校准质量。
- **FusionEngine**：衰减加权 + 95% CI，融合多源。
- 统计模块：`DeviationTracker` / `DivergenceTracker` / `ClimateTracker`（Welford 在线算法）。

## 4. 策略信号（strategy_engine）

| 策略 | 核心 | 信号有效期 |
|------|------|-----------|
| ForecastDeviation | 预报偏差 | 5 分钟 |
| MultiSource | 多源验证 | — |
| NowcastCorrection | 临近修正（时间权重 `1 - h/window`） | 30 分钟 |
| HistoricalStatistical | 历史统计（混合权重 `max(0.15, min(0.85, 1 - lead_hours/200))`） | 1 小时 |

## 5. 待补充（v0.1 → v1.0）

- [ ] Forecast Vintage 的落库与配对规则（P0 审计项）
- [ ] EMOS / BMA 的具体实现与参数
- [ ] 校准诊断（Reliability Diagram / PIT / CRPS）落地情况
