# CONSENSUS_DESIGN_ROUND_01.md

> **来源与诚实声明**：本文件由 OpenCode（实施者）根据**独立审查方在 2026-09-17 给出的审查结论**转录/归档。
> 审查对象：`docs/design/update_settlement_times_rpc.md`（v1）、`docs/design/settlement_pnl_risk_wiring.md`（v1）。
> 如获得审查方原件，请以原件为准并覆盖本文件。

## 结论

| Doc | 结论 | 允许实施 |
|-----|------|----------|
| `update_settlement_times_rpc.md` | **CHANGES_REQUESTED** | ❌ 暂不允许 |
| `settlement_pnl_risk_wiring.md` | **CHANGES_REQUESTED** | ❌ 暂不允许 |

- 不判定 REJECT；要求改为 **Draft v2** 后再做第二轮独立审查。
- 独立审查**未**将设计文档中的 `file:line` 视为已确认代码证据。
- 总体状态：**LIVE = NO-GO**（关键风控语义/authority/幂等/fail-closed 未冻结）。

## G1-002 Required Changes（v1 → v2）

1. 冻结 Unknown Settlement Time 的最终处置（不得 fail-open）。
2. `replace_all` 必须为 atomic snapshot replacement（禁止 clear+insert）。
3. 冻结 `success / accepted / rejected` 语义。
4. 增加 settlement registry freshness。
5. 明确 RPC 连续失败与 snapshot 过期后的行为。
6. 明确 expired/settled token 生命周期与 cleanup 条件。
7. 增加 concurrent atomic replace 测试。
8. 明确 `fast/risk_engine.rs` 最小 Allowed 范围。
9. 明确禁止修改：settlement window / Risk Model threshold / strategy / position limit / funds-control semantics。

## G1-001 Required Changes（v1 → v2）

1. 不得默认 UMA identifier→token_id 映射成立。
2. 提供 identifier→token_id 完整可验证数据链。
3. 核实 UMA outcome encoding。
4. 明确 realized PnL 是否包含 fee。
5. 明确 UTC daily reset。
6. 明确跨 UTC 日 settlement 归属。
7. 将 Rust 定义为唯一 Risk Authority（REQUIRED）。
8. `RiskStateInternal` 仅为 projection。
9. 明确 settlement accounting 的唯一 PnL delta 来源。
10. settlement 幂等必须支持并发（atomic claim）。
11. 增加 concurrent duplicate settlement 测试。
12. 增加 conflicting outcome 测试。
13. 增加 cross-day settlement 测试。
14. 明确 `risk_engine.rs` / `fill_records.rs` 最小 Allowed 边界。
15. 明确禁止修改：DailyLoss/BreachLevel threshold、Risk Model、strategy、trading semantics、funds-control semantics。

## 实施者响应（OpenCode）

- 两份文档已升级为 **Draft v2**，并在正文 §0 建立「审查意见 → 修改」对照：
  - `update_settlement_times_rpc.md` §0（9 项）
  - `settlement_pnl_risk_wiring.md` §0（15 项）
- 未修改任何生产代码（遵循「只改设计」要求）。
- 关键设计基线（待 Human 批准）：
  - G1-002：未知结算时间 **fail-closed 拒绝新开仓**；atomic snapshot replacement。
  - G1-001：先做 **identifier→token_id / outcome encoding 的 POC**；POC 通过前不实施；Rust 为唯一 Risk Authority。

## 下一步

1. 独立审查 #2：核对证据引用 + Draft v2 是否满足上述全部 Required Changes。
2. Human 批准 Draft v2 与其中标记的开放问题。
3. 通过后拆分 `MASTER_TASKS.md`（含 Allowed / Forbidden / Required Tests / Acceptance）。
4. 在此之前：**不实施、不修改生产代码**。
