# CONSENSUS_ROUND_04.md

> **来源与诚实声明**：本文件是 OpenCode（实施者）根据 **Human 在 2026-09-17 会话中转述的独立共识审查结论**所做的**转录**。
> 独立审查方产出的 `CONSENSUS_ROUND_04.md` **原件未在本仓库/本机找到**（已在 `C:\Users\32389` 与 `D:\` 全盘检索）。
> 如已获得原件，请**直接用原件覆盖本文件**，以原件为准。
>
> 审查输入：`audits/round_04/audit_opencode_round_04.md`（OpenCode，commit `21fdcf8`）+ `audits/round_04/REVIEW_CHECKLIST_ROUND_04.md`。
> 独立性：OpenCode 为本项目**工程实施者**，未参与 VERIFIED 判定；本转录不改变该事实。

---

## 一、裁决结果（转述）

| Finding | 独立裁决 | Final Severity | 后续动作 |
|---------|----------|----------------|----------|
| P0-RISK-001 日亏熔断未接线 | CONFIRMED | P0 | 进入 `MASTER_TASKS.md` G1 |
| P0-RISK-002 结算窗口未接线 | CONFIRMED | P0 | 进入 `MASTER_TASKS.md` G1 |
| P0-RISK-003 Gas 保护被硬编码 0 绕过 | CONFIRMED | P0 | 进入 `MASTER_TASKS.md` G1 |
| P0-EXEC-004 实时成交链不生效 | CONFIRMED | P0 | 进入 `MASTER_TASKS.md` G2 |
| P0-EXEC-005 Maker 方向未归一化 | CONFIRMED | P0 | 进入 `MASTER_TASKS.md` G3 |

## 二、任务授权（转述）

- **G1 / G2 / G3**：授权进入 `MASTER_TASKS.md`，`Status → TODO`。
- **ENV-001**：授权。

## 三、P1-035 状态（转述）

- **不得升级 VERIFIED**，维持 **FIXED / Verification Pending**。
- 理由（审查原则）：严格区分
  `代码存在 ≠ 生产调用 ≠ 测试通过 ≠ 鉴别力测试通过 ≠ VERIFIED`。

## 四、外部事实核验（转述，支持定级）

1. Polymarket 当前官方 SDK 的 **Trade schema** 同时存在
   `side` / `trader_side` / `maker_address` / `maker_orders` / `taker_order_id`，
   因此 **P0-EXEC-005 不能仅凭顶层 `trade.side` 完成本方成交方向归属**。
2. `/data/trades` 的 **L2 认证要求**有外部文档证据支持，
   故 **P0-EXEC-004 的认证部分不是单纯代码推测**。

## 五、总体状态（转述）

- **LIVE = NO-GO**（任何 P0 未闭环前禁止实盘）。

---

## 六、实施进展（OpenCode 附加，非共识原件内容）

> 供追溯，不作为共识裁决的一部分。

| 项 | 状态 | commit |
|----|------|--------|
| G3 P0-EXEC-005 实时/恢复方向归一化 | 🔧 FIXED（待独立判 VERIFIED） | `e02d26f` |
| G2 P0-EXEC-004 实时成交链（L2+token+fail-closed） | 🔧 FIXED（待独立判 VERIFIED） | `97adf69` |
| ENV-001 Python 测试门禁 | 🔧 FIXED / 已验证（`pytest -q` 94 passed） | `f6791e5` |
| G1 P0-RISK-001/002/003 | TODO（G1-002 需先出 `UpdateSettlementTimes` 接口方案待批；G1-001 依赖结算/PnL 链） | — |
