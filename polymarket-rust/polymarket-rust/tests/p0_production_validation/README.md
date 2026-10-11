# P0 生产验证集成测试

上线前必须全部通过的验收测试。每个测试对应 P0 清单中的一项，从 crate 公共 API 验证关键安全行为。

## 运行

```bash
# 仅本目录
cargo test --test p0_production_validation

# 全量（单元 + 集成）
cargo test
```

## 覆盖清单

| 文件 | P0 项 | 验证内容 |
|------|-------|---------|
| `trading_gate.rs` | P0-007 / P0-011 | TradingGate 状态机 + 启动对账进 READY（非 READY 拒单、对账失败保持 Reconciling、熔断阻断） |
| `order_dedup.rs` | P0-004 | Order Dedup 并发原子性（16 线程并发 claim 只 1 次成功） |
| `risk_engine.rs` | P0-003 | RiskEngine 超仓拒单（单市场超限、全局超限、平仓放行） |

## 说明

- **StateSync 对账**（P0-007/09）：`reconcile_positions` / `reconcile_orders` 纯逻辑由 `src/fast/state_sync.rs` 单元测试覆盖；启动时的「对账结果 → TradingGate 状态」决策，通过 `trading_gate.rs` 的 `reconcile_complete` 测试覆盖（`StateSync::new` 依赖钱包与 API 凭据，集成测试不重复构造）。
- 与 `src/**` 内联单元测试互补：单元测试覆盖内部实现细节，本目录从外部验证公共契约，防止「内部改了但公共行为退化」。
