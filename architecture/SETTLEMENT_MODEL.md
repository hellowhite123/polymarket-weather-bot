# SETTLEMENT_MODEL.md

**架构真源** · 版本 v0.1 · 2026-08-25 · 待随审计完善

## 1. 结算目标（SettlementTarget）

每个天气市场有唯一的结算目标，必须精确对齐：

```text
Polymarket Market
      │
      v
SettlementTarget  （精确站点 + 官方数据源 + 结算规则）
```

- 结算目标错误 = 直接导致错误 token 定价 → P0。
- 禁止：`Grid Temperature = Settlement Temperature`（协议 §24）。

## 2. 结算与争议（UMA OO）

- 结算依赖 UMA Optimistic Oracle（OO）。
- `uma_monitor` 监听 UMA OO 预言机事件：提案 / 争议 / 结算。
- 争议中市场禁止交易（RiskEngine 拦截）。

## 3. 成交与盈亏结算

- 成交记录账簿 `FillLedger`：完整字段（下单/成交/结算时间 + 滑点 + 手续费 + 盈亏）。
- 盈亏仅在结算时产生（`FillLedger.settle` 接口回填）。
- 滑点定义：`|成交价 - 下单价| / 下单价`（始终 ≥0）。

## 4. 关键约束

- 结算目标、BUY/NO Token 路由属于 Human 关键审查清单（协议 §8）。
- 结算前不得将未结算仓位记为已实现盈亏。

## 5. 待补充（v0.1 → v1.0）

- [ ] 各天气市场的官方结算源清单（站点 ID、数据源、结算时间）
- [ ] UMA 争议期间的仓位处理策略
