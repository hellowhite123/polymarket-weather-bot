# R5-002 实施报告（V3 定点整改）

- **状态**：IMPLEMENTED — **待独立审查**（OpenCode 不自行判 VERIFIED）
- **日期**：2026-09-18
- **基线 → 新**：`d388912` → `a2dde81`
- **关联**：`MASTER_TASKS.md` R5-002、`docs/design/R5-002_draft_v2_evidence.md`

---

## A. Diff

```bash
git diff d388912 a2dde81 --stat
# strategy_engine/config.py                        | ... (新增 2 字段 + 装配注入)
# strategy_engine/strategies/multi_source.py       | ... (_calculate_quantity 重写 + 调用点)
# test_multi_source.py                             | ... (T1–T12)
```

## B. 修改文件清单

| 文件 | 修改内容 | 为什么需要 |
|------|----------|-----------|
| `strategy_engine/config.py` | `MultiSourceStrategyParams` 新增 `base_capital`(默认1000.0)/`kelly_fraction`(默认0.25)；`StrategyConfig.from_env()` 先解析 `AggregatorParams.from_env()`，再把 `base_capital`/`kelly_fraction` **注入** multi_source | §2.1/§3/§4：策略参数承载资本，由装配层从 `AGG_*` 单一来源注入 |
| `strategy_engine/strategies/multi_source.py` | `_calculate_quantity` 重写为有界 advisory sizing；调用点取残差 σ 与 cost | §5/§9/§10：饱和 edge、USDC cap、单位转换 |
| `test_multi_source.py` | 新增 T1–T12 | §16 测试要求 |

> 未修改任何装配以外的生产逻辑；`pipeline.py` 无需改动（其使用 `self.config.multi_source`，注入已在 config 装配层完成）。

## C. 公式证明（实际调用链）

`multi_source.py:_evaluate_one`：
```python
# dev_sigma 来自 is_extreme 的第4个返回值 = sigma_level(z)，不是残差σ
dev_is_extreme, _, dev_mu, dev_sigma = self.dev_tracker.is_extreme(dev_record, ...)
...
# 残差 σ（概率量纲）——从 stats() 取
_, dev_scale_sigma, _ = self.dev_tracker.stats(event_type, location, market.token_id)
cost = market.price_for(direction)          # YES→yes_price, NO→no_price
quantity = self._calculate_quantity(market_dev, dev_scale_sigma, cost)
```
`_calculate_quantity`（`multi_source.py`）：
```
edge              = 1 - exp(-|market_dev| / σ)                 # σ = dev_scale_sigma
advisory_notional = base_capital × kelly_fraction × edge        # USDC
capped_notional   = min(advisory_notional, max_position_per_market)   # USDC cap
quantity          = capped_notional / cost                      # shares
```
**fail-closed**：σ ≤ 0/NaN/Inf、cost ≤ 0/NaN/Inf、base_capital/kelly_fraction/cap 无效 → `quantity = 0`。

**§17 关键发现**：`multi_source` 原有变量 `dev_sigma` 实为 **`sigma_level`（z 值）**（`deviation_tracker.py:148-151`），**不是残差 σ**。V3 的 σ 语义（1σ→0.6321）需要概率量纲的残差 σ，故实现改用 `DeviationTracker.stats()` 的 `std`（Welford 滚动，`window_size=100`，样本≥10 才有效，σ<1e-10 时 `is_extreme` 记 0）。**未新增 dev_scale 校准系统**（§17 要求）。

## D. 配置注入证明（单一资本权威）

```
AGG_BASE_CAPITAL / AGG_KELLY_FRACTION
        │  (AggregatorParams.from_env, aggregator.py:95-102 —— 唯一 env 读取点)
        ▼
AggregatorParams
        ├──────────────► PositionSizer          (aggregator.py:160-164)
        └──────────────► MultiSourceStrategyParams (config.py:from_env，本次新增注入)
                                │
                                ▼
                        MultiSourceStrategy        (pipeline.py:114，params=self.config.multi_source)
```
- `MultiSourceStrategy` **内部无 `os.getenv`**（T11 断言 `cfg.multi_source.* == cfg.aggregator.*`）。
- 默认值（1000/0.25）仅为 dataclass fallback，非新的生产权威。

## E. 测试

- 新增 **T1–T12**（zero dev / 1σ / 2σ / saturation / 负 dev / USDC cap / YES cost / NO cost / invalid σ / invalid cost / 资本注入 / 聚合器 final authority）。
- `pytest`：**123 passed**（原 111 + 12）。
- **mutation 验证**：把 `edge` 改为线性截断 `min(|dev|/σ,1)` → T2/T3/T4/T7/T8 共 **5 个** FAILED；还原后全绿。

## F. Scope Check（确认未修改）

- ✅ `strategy_engine/aggregator/risk_gate.py` 未修改
- ✅ `polymarket-rust/.../risk_engine.rs` 未修改
- ✅ `strategy_engine/aggregator/position_sizer.py` 未修改
- ✅ `strategy_engine/aggregator/aggregator.py`（final sizing）未修改
- ✅ Rust 执行层 / `proto/polymarket.proto` 未修改
- ✅ 未新增第二套 capital config；未引入 `dev_sigma/2`；未引入新 Kelly

```
$ git diff --name-only -- strategy_engine/aggregator/risk_gate.py \
    polymarket-rust/polymarket-rust/src/fast/risk_engine.rs \
    strategy_engine/aggregator/position_sizer.py \
    strategy_engine/aggregator/aggregator.py proto/polymarket.proto
(空)
```

## 备注

- **E4（Python/Rust `max_position_per_market` 双权威 + Python risk_gate 单位非 USDC）已按 V3 §1 正式剥离**，本任务未触碰，建议另立 P1 Finding。
- 本报告不声明 VERIFIED；请独立审查。
