# 设计提案 V3 统一复审投喂清单（Round 03）

> 供**独立审查 AI / Human** 使用。覆盖 `docs/design/` 下全部设计提案的**当前版本**。
> 依据 `AGENTS.md`、`docs/CODE_REVIEW_STANDARD.md`、`docs/design/CONSENSUS_DESIGN_ROUND_01.md`、`docs/design/REVIEW_CHECKLIST_DESIGN_V2.md`。
> **OpenCode 为工程实施者，本清单不构成批准；结论不得由实施者自证。**

## 0. 复审输入（固定）

| 项 | 值 |
|----|----|
| 代码/文档快照 commit | `11fb2bc` |
| 产出 | `docs/design/CONSENSUS_DESIGN_ROUND_03.md`（建议） |
| 通用核对项 | `docs/design/REVIEW_CHECKLIST_DESIGN.md` |

### 总览

| 文档 | 关联 | 版本 | 状态 | 是否可实施 |
|------|------|------|------|-----------|
| `update_settlement_times_rpc.md` | G1-002 / P0-RISK-002 | **v3** | 待复审 | 复审通过 + Human 批准前 ❌ |
| `settlement_pnl_risk_wiring.md` | G1-001 / P0-RISK-001 | **v3** | 待复审；**P0-2 POC 未完成** | ❌（POC 阻断） |
| `reserve_release_accounting.md` | G4 / P1-RISK-008 | v1 | 待审查 | ❌ |
| `indoubt_order_reconciliation.md` | G4 / P1-EXEC-009 | v1 | 待审查 | ❌ |

---

## 1. G1-002 `update_settlement_times_rpc.md`（v3）复审项

> 第二轮整改要求 P0-DESIGN-001~004 + P1，逐条核对 V3 是否满足。

| 整改要求 | V3 位置 | 复审动作 | 残余/待确认 |
|----------|---------|----------|-------------|
| P0-DESIGN-001：Unknown fail-closed 的 production gate 冲突 | §2（call chain）、§3、§11 | 核对 gate 是否**唯一**且位于 `place_order`（`TimeWindowGuard` 之外）；`TimeWindowGuard` 窗口计算是否确实未改；`replace_settlements` 是否仅“数据替换” | Q：`replace_settlements` 是否纳入 Allowed |
| P0-DESIGN-002：STALE 已知 token 的 Buy 语义 | §4 | 核对是否冻结为 **STALE⇒全停 Buy / Sell 允许** | 例外保留需 Human |
| P0-DESIGN-003：双 freshness | §5 | 核对 transport_age + snapshot_age；缺失/未来/clock skew 是否 fail-closed | 数值待 Human |
| P0-DESIGN-004：replace_all 语义冲突 | §6、§7 | 核对 true=原子全量替换、false=原子 upsert、禁 clear+insert | — |
| P1：Human Acceptance 冻结 | §13 | 核对 12 项是否列全并标注“待 Human” | Human 冻结 |
| P1：registry 增长保护 | §10 | 核对“不删未确认 token + 上限 + 冷归档” | — |

**必须的真实路径测试（§12）**：`test_unknown_settlement_buy_rejected_on_order_path`、`..._sell_allowed_...`、`test_stale_registry_rejects_known_token_buy`、`test_registered_token_preserves_existing_window_behavior`、`test_update_settlement_times_partial_reject`、`test_settlement_snapshot_atomic_replace`、`test_single_decision_uses_single_snapshot`。

**需核验代码证据**（commit `11fb2bc`）：
- [ ] `risk_engine.rs`：`set_settlement(s)` 无生产调用；`TimeWindowGuard::pre_check` 对未注册返回 `Ok`
- [ ] `service.rs:224`：`settlement_times: Vec::new()`
- [ ] `risk_gate.py:80-105`、`converter.py:156-165`：Python 侧依赖回传
- [ ] `market_discovery/models.py:76,92-94`：`end_date_utc`

---

## 2. G1-001 `settlement_pnl_risk_wiring.md`（v3）复审项

> 第三轮整改要求 P0-1 / P0-2 / P0-3 + P1。

| 整改要求 | V3 位置 | 复审动作 | 残余/待确认 |
|----------|---------|----------|-------------|
| P0-1：PnL 权威语义 | §3 | 核对是否以**现有** `compute_pnl`/`settle_by_token` 为权威；multi/partial/unsettled/settled/剩余 是否逐项冻结 | **V2 错误已更正为 net（`− fee`）**，请确认 |
| P0-1：gross/net | §3.3 | 核对 net 定义与现有代码一致；fee 不新增 | HA-002 |
| P0-2：settlement identity | §4 | 核对 `settlement_id` 幂等键语义 + **POC-1/2/3 门禁** | **POC 未完成 → 阻断**；HA-007 |
| P0-3：crash-safe | §5 | 核对持久化 `SettlementRecord` 状态机 + case A–E | 持久化=架构变更，HA-006 |
| P0-4：report_pnl 幂等 | §6 | 核对 `report_pnl(settlement_id, pnl_delta, settlement_ts)` 幂等 | — |
| P1：模块边界 | §7 | FillLedger=authority / RiskEngine=consumer / RiskStateInternal=projection / Python=consumer | — |
| P1：settlement timestamp | §8 | 核对以权威 settlement UTC 时间入 `settled_at` 与分桶（现状 `now_secs()` 错误） | — |
| P1：UTC DailyLoss | §9 | 复用现有机制 + 迟到结算处理 | HA-003/005 |
| P1：restart recovery | §10 | 核对 PENDING/ACCOUNTING 重放、ACCOUNTED 跳过 | — |
| P1：测试规格 | §12 | 核对 12.1–12.10（并发/冲突/多 fill/partial/crash×2/restart/集成/projection） | — |
| 完整调用链 / Allowed / HA | §13/§14/§15 | 核对 13 环 + Allowed/Forbidden + HA-001~007 | — |

**须重点确认（诚实性）**：V3 §16 自评 **P0-2 ❌ 未完成**（缺链上原始事件/外网）。若审查方据此判 CHANGES_REQUESTED/阻断，属预期；推进需 **HA-007**。

**需核验代码证据**：
- [ ] `fill_records.rs:291-297`（compute_pnl net）、`:401-422`（settle_by_token；返回 count 非 delta）、`:151-155/251-255`（`now_secs()`）
- [ ] 测试 `test_compute_pnl_with_fee`（`:715`）、`test_ledger_multi_fill_settle_by_token`（`:832`）→ 证明 net + multi-fill 权威
- [ ] `risk_engine.rs:555-594`（report_pnl）、`:889`（私有 daily_loss）、`:986-1005`（check_gates）
- [ ] `grpc/service.rs:334-338`（未调用包装）、`:347-368`（on_market_trade）、`:194-235`（RiskStateInternal 无写入）
- [ ] `uma_monitor.rs:119`（无 token 映射）、`:261-310`（identifier 解出）
- [ ] FillLedger/OrderStore **无持久化**（grep 无 persist/load/save）

---

## 3. `reserve_release_accounting.md`（v1）复审项（P1-RISK-008）

| 复审点 | 文档位置 | 核对 |
|--------|----------|------|
| 根因：预留按限价入池、按成交价出池 | §1 | 对照 `risk_engine.rs:332-338` |
| Option A（释放额=成交增量×订单限价）守恒论证 | §3.2 | 核对全成交/部分成交+取消是否恰好释放 |
| Option B（per-order 台账）取舍 | §3.3 | 是否同意推荐 A |
| 限价来源（OrderStore.limit_price） | §3.2 | 对照 `order_state.rs` |
| 过期路径是否释放 | §4/§8 Q3 | 需 Human |
| 记账接口签名变更授权 | §6/§8 Q2 | **Risk Model 记账红线，需授权** |
| 测试 + 鉴别力 | §5 | — |
| 开放问题 Q1–Q5 | §8 | 需结论 |

**需核验代码证据**：`risk_engine.rs:204-209`（PositionEntry 池化）、`:272-306`（按限价预留）、`:322-344`（按成交价冲抵）、`:1088-1097`（on_fill）。

---

## 4. `indoubt_order_reconciliation.md`（v1）复审项（P1-EXEC-009）

| 复审点 | 文档位置 | 核对 |
|--------|----------|------|
| 根因：Received 被排除对对账输入集 | §1 | 对照 `order_state.rs:352-369`、`service.rs:273-275` |
| 未知结果分支保持 Received + 预留 | §1 | 对照 `service.rs` fast path Err arm（`is_unknown_outcome`） |
| Option A（可见性 + fail-closed 停新单） | §3.1 | 是否接受为第一步 |
| Option B（远端匹配收敛） | §3.2 | 匹配键/唯一性/TTL |
| Option C（client order id） | §3.3 | 平台能力待核 |
| 停新单范围（全局 vs 仅该 token） | §8 Q4 | 需 Human |
| 是否改 proto 观测字段 | §8 Q6 | 接口变更 |
| 测试 + 鉴别力 | §5 | — |
| 开放问题 Q1–Q6 | §8 | 需结论 |

**需核验代码证据**：`order_state.rs:332-348`（pending_order_ids 含 Received，未用）、`:352-369`（pending_exchange_order_ids 排除 Received）、`service.rs:273-275`、`state_sync.rs:695-738`（`full_reconcile_with_recovery` 输入）、`bin/server.rs:225-297`（后台对账循环）。

---

## 5. 跨文档一致性 / 红线

- [ ] G1-002（预计结算时间）与 G1-001（实际结算/PnL）**语义分离**（两份 v3 均声明，核对无交叉污染）。
- [ ] G4 两份 v1 与 G1 的 Allowed 是否冲突；`risk_engine.rs` 的最小 Allowed 是否重复/可合并。
- [ ] 是否触及 `AGENTS.md` 红线（交易语义 / Risk Model / Strategy / 资金控制 / 重大架构 / 持久化）；涉及者是否显式申请授权（HA-006 等）。
- [ ] 与 `DECISIONS.md` C-001（fee=0）、C-007、C-008 是否冲突。

---

## 6. 统一 Human Acceptance 汇总（供一次拍板）

**G1-002（§13）**：Unknown Buy / Unknown Sell / STALE Known Buy / STALE Sell / NEVER_SYNCED Buy / `transport_freshness_window` / `snapshot_freshness_window` / `max_consecutive_failures` / `cleanup_grace` / clock skew / `max_registry_entries` / partial-reject 是否降级 / STALE 是否全停 Buy。

**G1-001（§15）**：HA-001 authority；HA-002 gross/net；HA-003 cross-day；HA-004 Rust 唯一；HA-005 迟到结算；HA-006 持久化方案；**HA-007 提供 UMA 原始事件 fixture / RPC 环境（POC 前置）**。

**P1-RISK-008（§8）**：Q1 Option A/B；Q2 授权改记账接口；Q3 过期释放；Q4 限价未知回退；Q5 不变量校验。

**P1-EXEC-009（§8）**：Q1 Option A；Q2 匹配键/asset_id 映射；Q3 client order id；Q4 停新单范围 + 人工处置入口；Q5 TTL；Q6 proto 字段。

---

## 7. 复审结论模板（`CONSENSUS_DESIGN_ROUND_03.md`）

```markdown
| Doc | Version | 结论 | 关键阻断 | 需 Human |
|-----|---------|------|----------|----------|
| update_settlement_times_rpc.md | v3 | APPROVED / CHANGES_REQUESTED / REJECTED | ... | 13~ |
| settlement_pnl_risk_wiring.md   | v3 | ... | P0-2 POC | HA-001~007 |
| reserve_release_accounting.md   | v1 | ... | 记账授权 | Q1~Q5 |
| indoubt_order_reconciliation.md | v1 | ... | 对账语义/proto | Q1~Q6 |

证据核验：G1-002 __/4；G1-001 __/7；P1-008 __；P1-009 __
理由与修改要求：...
下一步：拆分 MASTER_TASKS（Allowed/Forbidden/Required Tests/Acceptance）
```

---

## 8. 流程与停止条件

```
Draft v3 / v1（当前）
   │
   ▼
独立复审（本文）──► CONSENSUS_DESIGN_ROUND_03
   │  ├─ APPROVED ──► Human 批准（含 HA 冻结）──► 拆分 MASTER_TASKS ──► Implementation
   │  ├─ CHANGES_REQUESTED ──► 修订 design → v4
   │  └─ REJECTED ──► 重做
   ▼
POC（G1-001 P0-2）需 HA-007 提供数据/环境
```

> **当前 LIVE = NO-GO**。四份设计提案未经独立复审 + Human 批准前，**一律不得进入生产实现**；G1-001 另受 **P0-2 POC 未完成** 阻断。
