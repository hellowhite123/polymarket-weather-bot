# CONSENSUS_DESIGN_ROUND_06.md

> **来源与诚实声明**：本文件归档的是**独立审查方（非 OpenCode）对 G1-001 Draft v6 的复审结论与 V7 整改指令**（由 Human 转交）。OpenCode 仅执行整改并归档，**不构成自证，不声称 APPROVED**。

- **被审对象**：`docs/design/settlement_pnl_risk_wiring.md` **Draft v6**（commit `511dad8`；HA-006 倾向登记 `f98d61f`）
- **复审结论**：**CHANGES_REQUESTED**
- **整改产物**：同文档 **Draft v7**（见 `git log` 最新 commit）
- **当前状态**：P0-2 UMA POC = **BLOCKED / HA-007**；HA-006 Human 倾向 **D1 SQLite**、尚未正式冻结；LIVE = **NO-GO**；Settlement → PnL production implementation = **NO-GO**

---

## 1. 逐项结论（v6）

| 项 | 结论 |
|----|------|
| Q1 durable authority | PASS |
| Q2 daily_pnl = A replay projection | PASS |
| Q3 transaction boundary | **CHANGES** |
| Q4 commit marker | PASS |
| Q5 crash 判定 | **CHANGES** |
| Q6 唯一恢复模块 | PASS |
| Q7 证明 | PASS |
| I1 / I2 / I3 | PASS |
| D1 SQLite 协议 | **CHANGES** |
| D2 WAL 协议 | PASS（保留） |
| 整体 | **CHANGES_REQUESTED** |

## 2. 核心问题：P0-3 transaction-model consistency

§5.4 的 S5/S6 + Case C 描述的「effect 已 durable、status 尚未 ACCOUNTED」状态，与 §5.6 D1 的 SQLite single transaction（`BEGIN IMMEDIATE → INSERT effect → UPDATE settlement → COMMIT`）不一致：

- D1 单事务下，**COMMIT 成功 ⇒ AccountingEffect 与 ACCOUNTED 同时 durable**；
- **COMMIT 前 crash ⇒ SQLite rollback ⇒ 两者均未提交**；
- 因此 D1 **不存在**独立的 `effect committed + status != ACCOUNTED` 状态；
- **Case C 是 D2 两阶段 / 多记录 WAL 模型的可能状态**，D1 由 atomicity 排除。

V7 必须统一 transaction model。

## 3. V7 整改指令（执行摘要）

1. **冻结 D1 SQLite 单事务模型**：S4 = `BEGIN IMMEDIATE`（验证 identity → 计算权威 pnl_delta → INSERT AccountingEffect → UPDATE SettlementRecord{ACCOUNTED, pnl_delta, accounting_effect_id}）→ `COMMIT`；COMMIT 成功 = effect 已生效唯一 durable boundary；内存 `daily_pnl` 不在事务边界内，始终 = committed effects 的 projection/cache；运行时顺序 = **durable COMMIT → 更新内存缓存**。
2. **§5.5 Case A–E 区分 D1 / D2**；D1 无 Case C（rollback → retry；COMMIT → rebuild → no-op）。
3. **§5.6 Q1–Q7 对齐 D1 单事务**；commit marker = D1 SQLite COMMIT / D2 EFFECT_COMMIT + fsync，术语不混用。
4. **补 durable conflict algorithm**（事务内 SELECT → canonical_compare → 同则 no-op、异则 ROLLBACK/reject）；`UNIQUE` 不单独替代 payload conflict semantics；effects 表支持 §6.4 二选一（方案 A 存完整 identity / 方案 B 事务内读 settlements）。
5. **§14**：14.6/14.7 按 D1 重写断言；14.11 补 I1/I2/I3 可判定断言 + journal 不 append；**新增 14.12 `test_sqlite_transaction_atomicity`**（A crash before COMMIT / B after COMMIT / C conflict）。

## 4. 禁止事项（V7 全程遵守）

- 不得修改生产 Rust/Python；不得改 Risk Model / DailyLoss 阈值 / BreachLevel；不得改 strategy / forecast / pricing；不得改 funds-control；
- 不得实现 Settlement → PnL production chain；不得假设 UMA mapping / outcome encoding 已验证；
- 不得用 HashSet / Mutex / RwLock 宣称 exactly-once；不得用 UNIQUE(settlement_id) 单独宣称 conflict semantics 已解决；
- 不得为解决 Case C 重新引入「memory daily_pnl 与 durable effect 同一 transaction」的旧模型。

## 5. 完成条件核对（V7 必须能直接回答）

| 问 | 答案 |
|----|------|
| Q1 | `SettlementJournal` 唯一 durable authority |
| Q2 | `daily_pnl` = committed effects 的 projection/cache |
| Q3 | D1：AccountingEffect + SettlementRecord ACCOUNTED 同一 SQLite transaction |
| Q4 | D1 = SQLite COMMIT；D2 = EFFECT_COMMIT + fsync |
| Q5 | D1 rollback→retry / committed→no-op；D2 无 commit→retry / 有 commit→不重复 |
| Q6 | `SettlementRecovery` 唯一 recovery owner，先于 TradingGate Ready |
| Q7 | I1 / I2 / I3 全部可机械判定 |

## 6. 后续

- v7 交付后 **等待独立 AI / Human 复审**；通过 + HA-006 冻结（D1）+ HA-007（P0-2 POC 环境）齐备后，方可进入 implementation。
- 复审结论归档：下一轮 `CONSENSUS_DESIGN_ROUND_07.md`（如适用）。
