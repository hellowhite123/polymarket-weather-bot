# 设计提案 v2 独立复审投喂清单（Round 02）

> 供**第二轮独立审查 AI / Human** 使用。对象为 G1-001 / G1-002 的 **Draft v2**。
> 依据 `AGENTS.md`、`docs/CODE_REVIEW_STANDARD.md`、`docs/design/CONSENSUS_DESIGN_ROUND_01.md`（首轮 CHANGES_REQUESTED）。
> **OpenCode 为工程实施者，本清单不构成批准；结论不得由实施者自证。**

## 0. 复审输入（固定）

| 项 | 值 |
|----|----|
| 代码/文档快照 commit | `8f22902`（本清单产出时） |
| 被审文档 | `docs/design/update_settlement_times_rpc.md`（v2）、`docs/design/settlement_pnl_risk_wiring.md`（v2） |
| 首轮结论 | `docs/design/CONSENSUS_DESIGN_ROUND_01.md`（两份 CHANGES_REQUESTED） |
| 通用核对项 | `docs/design/REVIEW_CHECKLIST_DESIGN.md` |
| 产出 | 在 `docs/design/` 追加二审结论（建议 `CONSENSUS_DESIGN_ROUND_02.md`） |

**复审重点**：
1. 首轮 9 + 15 项 Required Changes 是否**逐条满足**（本文 §1/§2 对照）；
2. v2 中新增/变更的**设计决策是否自洽**、是否与红线冲突；
3. 文中的 `file:line` 证据声明是否属实（**首轮明确未将设计文档行号视为已确认证据，本轮请逐条核验**，见 §3）。

---

## 1. G1-002 `update_settlement_times_rpc.md` v2 — 逐条对照

| 首轮 Required Change | v2 位置 | v2 如何处理 | 复审动作 | 残余风险/待确认 |
|---|---|---|---|---|
| 1. 冻结 Unknown Settlement Time（不得 fail-open） | §6 | 基线=**fail-closed 拒绝新开仓**（Sell 放行）；废弃 v1 的折中 c；列为 Human 必批 | 确认基线是否满足 fail-closed；是否有隐藏 fail-open 分支 | Q1：从未监控的 token 是否也一致拒绝 |
| 2. `replace_all` 必须 atomic snapshot replacement | §7 | 禁止 clear+insert；构造新 `HashMap`→校验→单次 `Arc` 交换；读者只见旧/新完整快照 | 核对是否覆盖「增量 upsert」与「全量替换」两种模式 | 是否需为原子替换放开 `risk_engine.rs` 最小 Allowed |
| 3. 冻结 `success/accepted/rejected` 语义 | §3.1 | `success`=请求级；`accepted/rejected`=条目级；`success=false` 不得有任何变更 | 确认语义是否写入 proto/Acceptance | — |
| 4. 增加 registry freshness | §8 | 新增 A/B/C/D 四态 + `last_success_at`/`snapshot_taken_at`/连续失败计数 | 确认四态与交易闸门映射是否完整 | Q5：`freshness_window` 数值 |
| 5. RPC 连续失败/过期行为 | §8 | 连续失败超阈值 → 降级停新单（基线）；过期 → 已注册仍判定但告警 | 确认是否与 G2 降级通道一致 | Q3：是否降级 TradingGate |
| 6. token 生命周期/cleanup | §9 | ACTIVE→SETTLEMENT_WINDOW→EXPECTED_PASSED→ACTUALLY_SETTLED→CLEANUP_ELIGIBLE；仅末态可删 | 确认「预计过 ≠ 已结算」是否守住 | 依赖 G1-001 的 actual settlement 信号 |
| 7. concurrent atomic replace 测试 | §10 | 新增 `test_settlement_snapshot_atomic_replace`（读者 ∈ {old,new}） | 确认测试可证伪（不会观察到空/部分集） | — |
| 8. `risk_engine.rs` 最小 Allowed | §11 | 仅 snapshot getter + atomic replacement + 直接相关最小辅助 | 核对是否越界 | Q2 批准 |
| 9. 禁止修改项 | §11 | settlement window / Risk Model threshold / strategy / position limit / funds-control 全部 Forbidden | 核对无遗漏 | — |

---

## 2. G1-001 `settlement_pnl_risk_wiring.md` v2 — 逐条对照

| 首轮 Required Change | v2 位置 | v2 如何处理 | 复审动作 | 残余风险/待确认 |
|---|---|---|---|---|
| 1. 不得默认 UMA 映射成立 | §5.1 | 改为 **POC 前置门禁**，未证明不实施 | 确认门禁是否强制 | — |
| 2. 提供 identifier→token_id 完整数据链 | §5.1 / §11 | 新增 POC 任务（G1-001A）与产出清单 | 确认产出要求是否可验证 | 映射算法尚不存在 |
| 3. 核实 UMA outcome encoding | §5.2 | 未验证前**禁止** outcome 转换 | 确认「禁止」是否明确 | 编码结论待 POC |
| 4. realized PnL 是否含 fee | §7.1 | 基线=gross（不含 fee，C-001 默认 0），Human 必批 | 确认基线 + 待批状态 | Q2 批准 |
| 5. UTC daily reset | §7.2 | 复用现有 `DailyLossLimit` UTC 日切，不另造 | 确认无第二套 reset | — |
| 6. 跨 UTC 日归属 | §7.2 / §10 | 归**实际结算发生日**；新增 cross-day 测试 | 确认测试覆盖 | — |
| 7. Rust 唯一 Risk Authority（REQUIRED） | §6 | 明确 Rust 权威；Python 为 projection | 确认无第二权威 | — |
| 8. `RiskStateInternal` = projection | §6 / §8.2 | 只从 `RiskEngine` 权威快照填充，不自行累计 | 确认单一来源 | — |
| 9. 唯一 PnL delta 来源 | §8.1 | `settle_by_token`→`pnl_delta`→`report_pnl` 单点 | 确认无其它 `report_pnl` 路径 | Q5 放开 `fill_records.rs` |
| 10. 并发幂等（atomic claim） | §9 | 禁止 `contains()+insert()`；冲突 outcome 拒绝 | 确认幂等门与 `report_pnl` 无让渡窗口 | — |
| 11. concurrent duplicate settlement 测试 | §10 | `test_concurrent_duplicate_settlement` | 确认「恰好一次」可证伪 | — |
| 12. conflicting outcome 测试 | §10 | `test_conflicting_duplicate_outcome` | 确认无二次/反向 PnL | — |
| 13. cross-day settlement 测试 | §10 | `test_cross_utc_day_settlement` | 确认归属日的断言方式 | — |
| 14. `risk_engine.rs`/`fill_records.rs` 最小 Allowed | §12 | 仅转发/只读快照/聚合，不改口径 | 核对是否越界 | Q5 批准 |
| 15. 禁止修改项 | §12 | DailyLoss/BreachLevel threshold、Risk Model、strategy、trading semantics、funds-control 全部 Forbidden | 核对无遗漏 | — |

---

## 3. 需独立核验的代码证据（首轮未采信，本轮请落实）

> 请以 commit `8f22902` 为准，逐条确认；若与行号不符，请给出实际位置。

**P0-RISK-002（G1-002）**
- [ ] `fast/risk_engine.rs`：`set_settlement` / `set_settlements` 无生产调用（仅定义 + 测试）
- [ ] `grpc/service.rs`：`RiskStateInternal::to_proto` 中 `settlement_times: Vec::new()`（约 `:224`）
- [ ] `strategy_engine/aggregator/risk_gate.py`：`_check_settlement_window` 依赖 `state.settlement_times`（约 `:80-105`）
- [ ] `strategy_engine/grpc_bridge/converter.py`：`risk_state_from_proto` 从 Rust 回传填充 settlement_times（约 `:156-165`）
- [ ] `proto/polymarket.proto`：仅 `GetRiskState`/`StreamRiskStateUpdates`，无反向推送（约 `:243-272`）
- [ ] `market_discovery/models.py`：`end_date_utc` 存在且未结束盘强制（约 `:76,92-94`）

**P0-RISK-001（G1-001）**
- [ ] `fast/risk_engine.rs`：`report_pnl` 仅被 `post_check` 调用；`post_check` 生产零调用（约 `:555,1049,1063`）
- [ ] `fast/fill_records.rs`：`settle` / `settle_by_token` 生产零调用（约 `:374,401`）
- [ ] `fast/market_events.rs`：`mark_settled` 无生产调用者；`consume_trade_events` 仅消费 Trade
- [ ] `grpc/service.rs`：`RiskStateInternal.daily_pnl` 无写入路径（约 `:194-235`）
- [ ] `strategy_engine/aggregator/risk_gate.py`：`_check_daily_loss` 依赖 `state.daily_pnl`（约 `:107-114`）

---

## 4. 跨文档一致性 / 红线

- [ ] G1-002 与 G1-001 的**语义分离**是否守住：预计结算时间（禁开仓）≠ 实际结算（PnL）——不得合并成单一状态（两文档均 §边界）。
- [ ] 两文档的 Allowed 交集是否自洽；`risk_engine.rs` / `fill_records.rs` 的最小范围是否有重复/冲突。
- [ ] 是否触及 `AGENTS.md` 红线（交易语义 / Risk Model / Strategy / 资金控制 / 重大架构）；若有，是否已显式申请授权。
- [ ] 是否与 `DECISIONS.md` C-001（fee 默认 0）、C-007（两阶段风控）、C-008（结算窗口/熔断）冲突。

---

## 5. 待 Human 拍板项（请从设计层给出建议，最终由 Human 批准）

**G1-002**：Q1 未监控 token 是否拒绝；Q2 是否放开 `risk_engine.rs` 原子替换；Q3 连续失败/过期是否降级；Q4 时间格式；Q5 freshness/失败阈值/cleanup 宽限数值；Q6 registry 归属（TimeWindowGuard 持 Arc vs Service 同步写入）；Q7 回传范围。

**G1-001**：Q1 settlement authority（POC 后定 A/B）；Q2 fee 口径；Q3 跨日归属（基线已定，确认）；Q4 Rust 唯一 authority（确认）；Q5 `risk_engine.rs`/`fill_records.rs` 最小 Allowed；Q6 Restricted=仅平仓（确认语义）。

---

## 6. 复审结论模板（写入 `CONSENSUS_DESIGN_ROUND_02.md`）

```markdown
| Doc | 结论 | 首轮 Required Changes 满足度 | 新增阻塞 | 需 Human 拍板 |
|-----|------|------------------------------|----------|---------------|
| update_settlement_times_rpc.md | APPROVED / CHANGES_REQUESTED / REJECTED | 9/9? | ... | Q1..Q7 |
| settlement_pnl_risk_wiring.md   | APPROVED / CHANGES_REQUESTED / REJECTED | 15/15? | ... | Q1..Q6 |

证据核验：P0-RISK-002 __/6；P0-RISK-001 __/5（列出不符项）
理由与修改要求：
- ...
批准后的下一步：拆分为 MASTER_TASKS（Allowed / Forbidden / Required Tests / Acceptance）
```

> ⚠️ 第二轮通过 + Human 批准前，G1-001/G1-002 **不进入实施**；LIVE 维持 **NO-GO**。
