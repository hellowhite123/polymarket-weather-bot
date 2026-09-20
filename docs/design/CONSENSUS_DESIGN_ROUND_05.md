# CONSENSUS_DESIGN_ROUND_05.md

> **来源与诚实声明**：本文件由 **OpenCode（工程实施者）依 Human 指令执行的自审**，针对 `settlement_pnl_risk_wiring.md` **v5**（commit `0ba8a78`）的 **v4→v5 五处定点修改**。
> **这不是独立审查批准，也不构成 VERIFIED**；独立裁定的最终结论应由非实施者给出。本文件仅为可追溯的自审记录。

## 0. 复审输入

| 项 | 值 |
|----|----|
| 工作区 HEAD | `8263b37` |
| 被审文档提交 | `0ba8a78`（`settlement_pnl_risk_wiring.md` v5，+49/-6） |
| 判据 | Human 指令：**先查 §5.6；若仅写“SQLite/WAL 可以做到”而无真正 transaction/recovery invariant，则必改 1 = CHANGES_REQUESTED，其余四项不再给 APPROVED** |

## 1. 结论

| # | 项 | 结论 |
|---|----|------|
| **1** | §5.6 Durable Transaction Boundary | **CHANGES_REQUESTED** |
| 2 | §4.5 settlement_id 前置依赖 | 不评（因 1 未过） |
| 3 | §6.4 effect identity 比较 | 不评（因 1 未过） |
| 4 | §10 late-settlement gate | 不评（因 1 未过） |
| 5 | §14.6/14.7 Test A/B 断言 | 不评（因 1 未过） |

## 2. 取证（commit `0ba8a78` 实际代码/测试）

- 代码中**不存在** `SettlementRecord` / `AccountingEffect` / `accounting_effect_id` / `apply_pnl_effect` / `settle_by_token_with_pnl`（grep 为空）。
- `report_pnl` 现状为 `pub fn report_pnl(&self, pnl: f64) -> BreachLevel`（`risk_engine.rs:555`），**无 `settlement_id`**；v5 描述的接口尚未实现。
- `FillLedger` **无持久化原语**（无 File/sqlite/WAL/journal；仅内存 `RwLock<HashMap>`）。
- `DailyLossLimit.daily_pnl: RwLock<f64>`（`risk_engine.rs:515`）—— **纯进程内内存**。
- 结论：v5 §5.6 依赖的持久层协议在 `0ba8a78` **无可执行依据**（设计阶段属预期，但缺 invariant 定义）。

## 3. 必改 1 详析：§5.6 未定义 transaction/recovery invariant

§5.6 只做了：列“三者一起生效”的愿望、给出 D1/D2 选项名、声明 “`daily_pnl` 必须可持久恢复”。**缺可验证 invariant 与协议**：

1. **核心矛盾未解**：第 3 项 `RiskEngine.daily_pnl` 是进程内 `RwLock`；**durable 提交无法与内存变更绑成原子**，§5.6 对此未作处理（即“SQLite/WAL 可以做到”式断言）。
2. **权威归属二义**：`daily_pnl` 以持久表为权威，还是由 committed effects 重放派生？“加载或按 effect 重放”不是 invariant。
3. **无 invariant 清单**：缺 `I1 可见 daily_pnl ⇔ 已提交 effect 集合`、`I2 每 settlement_id 至多一条 committed effect`、`I3 重放幂等且对 pnl_delta 确定性`。
4. **WAL 协议不完整**：无记录 framing/schema/version、无 fsync（durability point）、无 replay cursor、无尾部半写/损坏检测（长度/校验和）、无重放确定性判定。
5. **SQLite 选项不完整**：仅 UNIQUE 声称幂等；未定义包含 RiskEngine effect 的**事务边界**、`daily_pnl` 落点/派生、冲突检测事务语义。
6. **与 §5.5 未闭环**：Case A–E 未与持久层协议绑定为**恢复算法**（含每步崩溃的判定条件）。
7. **无对应代码/测试**：见 §2。

→ 依判据，**必改 1 = CHANGES_REQUESTED**。

### 3.1 必改 1 必须补齐（最小集）

- **I1–I3 invariant**（可见 PnL ⇔ committed effects；每 id ≤1 effect；重放幂等/确定性）。
- **权威归属二选一冻结**：
  - (a) 持久 store 为唯一权威，`daily_pnl` 于启动时**重放 committed effects** 重建（内存仅缓存）；或
  - (b) `daily_pnl` 持久化并与 effect **同事务**，内存仅缓存、启动必重载。
  - **不得**含糊。
- **D2 WAL**：记录 framing/schema/version、fsync 点、replay cursor、半写/尾损处理、校验和、确定性证明。
- **D1 SQLite**：表结构/约束、事务边界（含 effect 与 `daily_pnl` 落点）、冲突检测语句。
- **恢复算法**：§5.5 A–E 与持久层判定绑定；**每个 commit point 的故障注入测试**。

## 4. 其余四项（因 1 未过，依判据不给 APPROVED）

§4.5、§6.4、§10、§14.6/14.7 文本存在，但**与必改 1 未解耦**：§6.4 的幂等、§14.6/14.7 的“持久化 effect 存在”直接依赖 §5.6 的持久层协议。故**一并挂起**，待 §5.6 重做后再审。

## 5. 下一步

1. OpenCode 重做 §5.6（补齐 invariant + 权威归属 + D1/D2 完整协议 + 恢复算法），产出 v6。
2. 独立审查复审 v6；通过后 Human 批准（HA-006 持久化选型等）。
3. **P0-2 UMA POC 仍 BLOCKED**（HA-007），生产实现继续禁止；**LIVE = NO-GO**。
