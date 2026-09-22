# 设计方案：结算 / 已实现盈亏 → 日亏熔断接线（P0-RISK-001）— Draft v7

- **状态**：**Draft v7** — 按「G1-001 v6 独立复审（CHANGES_REQUESTED，P0-3 transaction-model consistency）」定点整改：**统一 D1 SQLite single-transaction semantics，消除 Case C 与 D1 transaction boundary 冲突**。仅改 §5.4/§5.5/§5.6/§6/§6.4/§14.6/§14.7/§14.11 + 新增 §14.12（§11 为必要联动），**待独立复审**。
- **作者**：OpenCode（工程实施者）
- **日期**：2026-09-22
- **关联**：`MASTER_TASKS.md` G1-001、`BUG_TRACKER.md` P0-RISK-001、`docs/design/CONSENSUS_DESIGN_ROUND_06.md`（本轮复审结论）、`docs/design/REVIEW_CHECKLIST_DESIGN_V6.md`
- **证据基线**：commit `bf68cad`（`file:line` 未经独立核验）。
- **本阶段仅设计 / POC**；提交后 **STOP**，不进入生产实现。

### v6 → v7 修改摘要（本轮定点整改，仅设计）

**复审结论（CHANGES_REQUESTED，归档于 `CONSENSUS_DESIGN_ROUND_06.md`）**：v6 已正确补齐 Q1/Q2/Q4/Q6/Q7、I1–I3、`SettlementRecovery` 唯一 owner、D1/D2 基本协议、commit marker、§14 I1–I3 与 14.11；但存在必须修正的 **P0-3 transaction-model consistency 问题**：

> §5.4 的 S5/S6 + Case C 描述的「effect 已 durable、status 尚未 ACCOUNTED」状态，与 §5.6 D1 的 SQLite single transaction（`BEGIN IMMEDIATE → INSERT effect → UPDATE settlement → COMMIT`）不一致。D1 采用单事务时：**COMMIT 成功 ⇒ AccountingEffect 与 ACCOUNTED 同时 durable**；**COMMIT 前 crash ⇒ SQLite rollback ⇒ 两者均未提交**。因此 D1 下不存在独立的 `effect committed + status != ACCOUNTED` 状态。V7 必须统一 transaction model。

| 项 | v6 | v7 |
|----|----|----|
| §5.4 | S5「改 daily_pnl + 写 effect_id 同一 durable 原子提交」+ S6 ACCOUNTED | **S4 = D1 单事务**（`BEGIN IMMEDIATE` → 验证 identity → `INSERT AccountingEffect` → `UPDATE SettlementRecord{ACCOUNTED, pnl_delta, accounting_effect_id}` → `COMMIT`）；S5 = COMMIT 后更新内存投影；S6 = recovery 验证 |
| §5.5 | Case A–E 单一表，含 Case C（effect 已提交、未 ACCOUNTED） | **区分 D1 / D2**；**D1 无 Case C**（由单事务 atomicity 排除）；Case C 仅为 D2 多记录 WAL 模型的可能状态 |
| §5.6 | D1「INSERT effect + UPDATE status 同一事务」简版；conflict 仅靠 UNIQUE | Q1–Q7 对齐 D1 单事务；新增 **durable conflict algorithm**（事务内 SELECT → canonical_compare → INSERT/UPDATE → COMMIT，或 ROLLBACK/reject）；effects 表须支持 §6.4（方案 A / B 二选一） |
| §6 / §6.4 | 语义已冻结 | 语义不变；commit marker 明确 = D1 `COMMIT` / D2 `EFFECT_COMMIT + fsync`；`UNIQUE` 不单独构成 conflict semantics |
| §14.6 / 14.7 | 通用断言 | 按 D1 语义重写断言（rollback → retry → 恰一笔；COMMIT → rebuild → no-op） |
| §14.11 | 重放幂等 | 补 I1/I2/I3 可判定断言 + journal 不 append |
| §14.12 | 无 | **新增** `test_sqlite_transaction_atomicity`（A 前 COMMIT / B 后 COMMIT / C conflict） |

**未变**：P0-2 UMA POC 仍 **BLOCKED（HA-007）**；HA-006 Human 倾向 **D1 SQLite** 但**未正式冻结**；LIVE = **NO-GO**；未改任何生产代码。

> **诚实声明（关键）**：P0-2 的 UMA POC（`identifier→condition_id→token_id` 映射、outcome encoding、settlement identity）在本环境 **POC BLOCKED**（无链上 RPC / 原始事件 fixture / 外网）。**P0-2 未关闭 ⇒ 禁止生产 settlement 实现**。V4 冻结其余可冻结语义；POC 需 Human 提供环境（HA-007）。**不得用“显然合理 / 应为 0/1 / token 天然唯一”替代证据。**

---

## 0. V3 → V4 修改摘要（对应定点整改意见）

| 整改项 | V3 缺口 | V4 处理 |
|--------|---------|---------|
| **P0-2** UMA POC | 仅描述门禁 | 保持门禁 + **明确 POC BLOCKED** + 精确产出清单与测试名（§4） |
| **P0-3** Crash-safe | 状态机未绑定“PnL effect 是否已应用” | 新增 **Accounting Effect Identity** + **Commit Point 表** + Crash Case A–E 状态转移表（§5） |
| **P0-4** report_pnl 幂等 | 语义粗略 | 冻结完整契约（同请求/冲突/跨重启），依赖持久化 effect（§6） |
| **约束** Lock≠ExactlyOnce | 未显式声明 | 显式写入设计约束（§7） |
| **P1-1** Net PnL 措辞 | 写成 Human 二选一 | 改为**冻结“本任务不得改变现有 net 口径”**，Human 仅确认（§3.3 / HA-002） |
| **P1-2** settlement timestamp | 仅提及 | 明确 `settlement_ts` 唯一来源；`received_at`/`processed_at` 仅观测（§9） |
| **P1-3** late settlement | 仅提 alert | 冻结归属日 + 历史 bucket 授权要求（§10） |
| **P1-4** Restart Recovery | 与幂等分离 | 统一为“持久化状态 + effect 身份 + 幂等 effect + 启动恢复”四件套（§11） |
| **测试** | 缺 crash 两例 | 新增 Test A（effect 后 crash）/ Test B（effect 前 crash），须自动化（§14） |
| **§16** Allowed/Forbidden | 截断 | 完整列出（§15） |

---

## 1. Root Cause（现状与证据）

1. **日亏熔断未接通**：`DailyLossLimit::report_pnl`（`risk_engine.rs:555-594`）仅经 `RiskEngine::post_check`（`:1049-1063`）可达，而 `post_check` 生产零调用；`on_fill`（`:1088-1097`）不更新 PnL；`RiskEngine.daily_loss` 私有（`:889`），无对外 `report_pnl`/`daily_pnl`。
2. **PnL 来源未接线**：`FillLedger::settle`（`fill_records.rs:374`）/ `settle_by_token`（`:401-422`）生产零调用；`service.rs:334-338` 的 `settle_token` 包装未被调用；`market_events::mark_settled` 无调用者；`consume_trade_events` 丢弃 `Settlement`。
3. **状态失真**：`RiskStateInternal.daily_pnl`（`service.rs:194-235`）无写入；`get_risk_state` 恒 0/false。
4. **UMA 映射缺失**：`uma_monitor.rs` 解出 `identifier`（`:266,295`）但注释承认「需外部映射 token_id」（`:119`）；`PriceSettledEvent.price` 原始字符串，无 outcome 解码。
5. **无持久化**：`FillLedger` / `OrderStore` 纯内存（grep 无 persist/load/save），重启即失忆 → **直接威胁 P0-3**。

---

## 2. 目标 / 非目标

**目标**：在既有正确设计上，关闭 P0-2/3/4 与 P1；形成 **Exactly-Once Settlement Accounting**（含重启）。
**非目标**：不重建 PnL 算法；不改 Risk Model/阈值/strategy/funds-control；不合并 G1-002。

---

## 3. P0-1：Realized PnL 权威语义（保留 v3，措辞冻结）

### 3.1 Accounting Authority（不变）
> 权威 = 现有 `FillLedger`：`compute_pnl`（`fill_records.rs:291-297`）+ `settle_by_token`（`:401-422`）。G1-001 **只消费**权威 `pnl_delta`。

### 3.2 冻结的数学定义（不变）
| 项 | 定义 | 依据 |
|----|------|------|
| BUY | `size × (settlement − filled_price) − fee` | `:293` |
| SELL | `size × (filled_price − settlement) − fee` | `:294` |
| multi-fill | 逐笔 `compute_pnl` 求和；订单级=各 fill 之和 | `:405-420`；测试 `test_ledger_multi_fill_settle_by_token`(`:832`) |
| partial fill | 仅 `status==Filled` 的 fill 参与；未成交不产生 PnL | `:406` |
| settled | 结算后 `Settled`，二次调用跳过（token 内不重复） | `:406,151` |
| YES/NO | 以该 token 自身 outcome 表达（DEC-0001/0002 路由不变） | `compute_pnl` |

### 3.3 Net PnL 措辞（**P1-1 修正**）
- 现有权威公式已含 `− fee`，即 **NET realized PnL（现有 `compute_pnl` 语义）**。
- **冻结**：`G1-001 MUST NOT change this accounting semantics.`（不引入新 fee 逻辑、不改为 gross）。
- **Human Acceptance（HA-002）不再是“选 gross/net”**，而是**确认“本任务不改变现有 net 口径”**。

### 3.4 唯一 `pnl_delta` 来源（不变）
- 禁止三套计算并存。`settle_by_token` 现返回 count；实现期需在同一临界区返回**权威 delta**（如 `settle_by_token_with_pnl(...) -> (usize, f64)`），其它模块只消费。

---

## 4. P0-2：UMA POC（**POC BLOCKED**）

### 4.1 现状
- 无 `identifier → condition_id → token_id` 映射（`uma_monitor.rs:119`）；`price` 未解码。
- **本环境无链上 RPC / 原始事件 fixture / 外网 ⇒ POC 无法执行。**

### 4.2 状态
```
POC-1 Mapping            : BLOCKED（缺原始事件/RPC）
POC-2 Outcome encoding   : BLOCKED
POC-3 Settlement identity: BLOCKED
```

### 4.3 必须产出（完成条件，禁止伪造）
| POC | 产出 |
|-----|------|
| POC-1 | 原始 `PriceSettledEvent` fixture；ABI/event 解码；`identifier→condition_id→token_id` 确定性映射算法 + 自动化测试 |
| POC-2 | `raw_outcome → decoded_outcome → application_outcome` 映射 + 已结算市场的期望结果 + 测试 |
| POC-3 | 稳定 `settlement_id`（restart/duplicate/replay 不变）+ 唯一性论证 + 测试 |

测试名（建议）：`test_uma_price_settled_event_mapping`、`test_uma_outcome_decoding`、`test_settlement_identity_derivation`。

### 4.4 分界（冻结）
- **允许**：POC 代码 / fixture / decoder / mapping test / 本设计修订。
- **禁止**：任何生产 `Settlement → PnL` 实现。

> **需 Human 提供 RPC / fixture / 链上数据环境（HA-007）。**

### 4.5 前置依赖链（必改2，冻结）
```
P0-2 POC 未完成
   → settlement_id 未经 POC-3 验证（唯一性/稳定性未证）
   → P0-4（report_pnl 幂等）只能“设计冻结”，不可实现
   → 禁止进入 production implementation
```
即：`settlement_id` 未经验证前，任何依赖它的幂等/恢复实现**一律不得落地**。

---

## 5. P0-3：Crash-Safe Settlement Accounting（**核心整改**）

### 5.1 为什么锁不够
`Mutex/RwLock/AtomicHashSet` 只解决 **in-process 并发**；`FillLedger` 无持久化 ⇒ `claim→crash` 丢 PnL、`apply→crash` 可能重 PnL。**必须持久化 + 可判定 effect 是否已应用。**

### 5.2 持久化模型（冻结语义，技术选型待 HA-006）
```
SettlementRecord {
    settlement_id: String,          // 幂等键（§4.3）
    token_id: String,
    outcome: f64,
    settlement_ts: u64,             // 权威结算时间（§9）
    pnl_delta: Option<f64>,
    status: SettlementStatus,
    accounting_effect_id: Option<String>,  // ★ 证明 RiskEngine effect 已应用
    received_at: u64, processed_at: Option<u64>,   // 仅观测
    updated_at: u64,
}
enum SettlementStatus { PENDING, ACCOUNTING, ACCOUNTED, RETRYABLE, FAILED }
```
- `accounting_effect_id`：**权威“PnL effect 已生效”证明**。与 effect 应用**原子持久化**（同一 durable 事务/追加记录）。
- 存储：append-only WAL(JSONL) 或 SQLite（HA-006 授权）。

### 5.3 状态机（冻结，状态数受控）
| 状态 | 含义 |
|------|------|
| PENDING | 已接收，未开始 accounting |
| ACCOUNTING | 已确定 identity / `pnl_delta`，正在执行 effect |
| ACCOUNTED | **effect 已确认，不允许再产生 effect** |
| RETRYABLE | 当前处理失败，可安全重放 |
| FAILED | 不可自动恢复，需人工（终态） |

### 5.4 Commit Points（★ 与 D1 单事务一致，冻结）

```
S1  durable insert SettlementRecord(status=PENDING)                [持久化]
S2  status = ACCOUNTING                                            [持久化]
S3  compute authoritative pnl_delta                                [内存]
S4  durable accounting transaction（D1：单一 SQLite 事务）          [持久化 ★]
        BEGIN IMMEDIATE;
          1. validate settlement identity（settlement_id/token_id/outcome 与事件一致）
          2. 使用 authoritative pnl_delta（S3，§3 权威口径）
          3. INSERT AccountingEffect
          4. UPDATE SettlementRecord:
                 pnl_delta            = authoritative delta
                 accounting_effect_id = effect identity
                 status               = ACCOUNTED
        COMMIT;     ★ COMMIT 是 effect 已生效的唯一 durable boundary
S5  after COMMIT: update in-memory daily_pnl projection           [内存]
S6  recovery / reconciliation verification:
        committed effect exists && SettlementRecord == ACCOUNTED   [读]
```

> **关键**：`daily_pnl` **不在 durable transaction 中**，由 committed effects 派生（§5.6 Q2-A）。COMMIT 成功 = AccountingEffect committed = settlement accounting committed = **effect 已生效**；COMMIT 前 crash = rollback，两者皆未提交（§5.5 D1 Case A/B）。D1 下**不存在**「effect 已 durable、status 尚未 ACCOUNTED」的中间态。

### 5.5 Crash Case A–E（区分 D1 / D2，冻结）

| Case | D1：SQLite single transaction | D2：WAL / multi-record |
|------|-------------------------------|-------------------------|
| **A** | transaction 尚未开始 / 未提交 → rollback → **retry** | pending（无 EFFECT_COMMIT）→ **retry** |
| **B** | transaction 中途 crash → SQLite rollback → **retry** | pnl 已准备但无 EFFECT_COMMIT → **retry** |
| **C** | **不存在于 D1 single transaction**（由 atomicity 排除） | EFFECT_COMMIT 已 durable、业务状态 marker 尚未完成 → **不重复 effect，只补状态** |
| **D** | committed + duplicate → **no-op** | committed + duplicate → **no-op** |
| **E** | same settlement_id + different canonical payload → **conflict / no mutation** | 同左 |

> **Case C 是 D2 两阶段 / 多记录 WAL 模型的可能状态；D1 single SQLite transaction 中由 atomicity 排除**——D1 下 COMMIT 成功 ⇒ effect 与 ACCOUNTED 同 durable；COMMIT 前 crash ⇒ rollback ⇒ 两者皆未提交。
> 恢复判定与唯一恢复模块见 **§5.6（Q5/Q6）**。

### 5.6 Durable Transaction Boundary（v7：7 问 + I1–I3 + D1 单事务 / D2）

**Q1｜谁是 durable accounting authority？**
唯一 **`SettlementJournal`**（持久 store：D1 SQLite 表 / D2 WAL 文件）。`RiskEngine` / `daily_pnl` **不是** authority。

**Q2｜daily_pnl 是 A 还是 B？**
**选 A：effect replay projection**。`daily_pnl(day) = Σ pnl_delta(committed effects, 其 UTC 日)`；内存 `DailyLossLimit.daily_pnl` 仅为 **cache**，启动由 journal 重建。→ 消除「把内存变更与磁盘提交绑原子」的不可实现矛盾。

**Q3｜transaction boundary 是什么？**
- **D1**：`AccountingEffect` + `SettlementRecord.status=ACCOUNTED` 在**同一个** `BEGIN IMMEDIATE … COMMIT` 中完成。**内存 daily_pnl 不在 boundary 内**。
- **D2**：多记录模型，boundary = `EFFECT_COMMIT + fsync`。
- **术语不混用**：D1 的 durable boundary = **SQLite COMMIT**；D2 的 durable boundary = **EFFECT_COMMIT + fsync**。

**Q4｜什么标记定义「effect 已生效」？**
- **D1**：**SQLite COMMIT 成功** = authoritative accounting commit（effects 行 + settlements 行同事务生效）。
- **D2**：**EFFECT_COMMIT + fsync** = durable accounting boundary。

**Q5｜crash 后如何判定？**
| 模型 | 观测 | 判定 | 动作 |
|------|------|------|------|
| D1 | 无 committed effect（未 COMMIT / rollback） | effect **未生效** | retry accounting（§5.5 A/B） |
| D1 | 有 committed effect（COMMIT 成功） | effect **已生效**（status 必已 ACCOUNTED） | rebuild projection → **no-op** |
| D2 | 无 EFFECT_COMMIT | 未生效 | retry |
| D2 | 有 EFFECT_COMMIT | 已生效 | **不重复 effect**；若业务 marker 未完成仅补状态（§5.5 C） |

**Q6｜restart recovery 由哪个唯一模块执行？**
唯一模块 **`SettlementRecovery`**，启动时执行一次、**先于 `TradingGate` Ready**：读 journal → 建 committed 集合 C → 重建 daily_pnl 投影 → 续处理未提交项 → 设置 Gate。**RiskEngine 不自恢复。**

**Q7｜如何证明（I1–I3）？**
- **I1 PnL consistency**：任意静止点 `daily_pnl(day) == Σ_{committed e, UTCday(e)==day} e.pnl_delta`；内存缓存必须等于该投影（恢复后强制成立）。
- **I2 settlement_id uniqueness**：∀ `settlement_id`，committed effect 数 **≤ 1**（D1 `UNIQUE(settlement_id)` / D2 append-once）。
- **I3 replay determinism**：`rebuild` 是 committed 集合的**纯函数**；`rebuild(journal)` 多次 == 一次；**重放只读、不产生新 effect、journal 不发生 append**。

**D1｜SQLite 单事务协议（本轮冻结为 single transaction）**
```
-- 表（effects 保存完整 identity，方案 A，见下）
settlements(settlement_id TEXT PRIMARY KEY, token_id, outcome,
            settlement_ts, pnl_delta, status, accounting_effect_id)
effects    (settlement_id TEXT PRIMARY KEY, token_id, outcome,
            pnl_delta, settlement_ts, applied_at)

-- S4 durable accounting transaction（★ effect 已生效的唯一 durable boundary）
BEGIN IMMEDIATE;
  1. 验证 settlement identity（settlement_id / token_id / outcome 与事件一致）
  2. 计算 authoritative pnl_delta（§3，内存）
  3. INSERT INTO effects(settlement_id, token_id, outcome, pnl_delta, settlement_ts, applied_at)
     VALUES(:id, :token, :outcome, :delta, :ts, :now);
  4. UPDATE settlements
     SET status='ACCOUNTED',
         pnl_delta=:delta,
         accounting_effect_id=:effect_id
     WHERE settlement_id=:id;
COMMIT;
-- COMMIT 成功 = AccountingEffect committed = settlement accounting committed = effect 已生效

-- 重建投影（I1/I3，只读）
SELECT utc_day(settlement_ts), SUM(pnl_delta) FROM effects GROUP BY utc_day(settlement_ts);
```

**D1｜durable conflict algorithm（§6.4 的持久落地，冻结）**
```
BEGIN IMMEDIATE;
  existing = SELECT settlement_id, token_id, outcome, pnl_delta, settlement_ts
             FROM effects WHERE settlement_id = :id;
  IF existing == NONE:
      validate canonical payload（§6.4）
      INSERT effects / UPDATE settlements → ACCOUNTED
      COMMIT;
  ELSE:
      canonical_compare(settlement_id, token_id, outcome, pnl_delta, settlement_ts)
      IF identical: COMMIT;  -- no-op
      ELSE: ROLLBACK; reject conflict;  -- 不累加、不覆盖、无第二笔 effect
```
> `PRIMARY KEY(settlement_id)` / `UNIQUE(settlement_id)` **只能保证唯一行**，**不能单独替代 payload conflict semantics**。

**Effects 表必须支持 §6.4（二选一，必须明确）**
- **方案 A**：effects 保存完整 identity：`settlement_id, token_id, outcome, pnl_delta, settlement_ts, applied_at`。
- **方案 B**：effects 不保存 token/outcome，但**事务内**从 settlements 读取 `token_id, outcome, pnl_delta, settlement_ts` 完成 deterministic canonical comparison。
- **不得**只写 UNIQUE 就宣称 conflict semantics 已闭合。

**D2｜WAL 协议（保留，术语与 D1 不混用）**
```
record := [len:u32][crc32:u32][type:u8][json]
append(SETTLE_PENDING, settlement_id, token_id, outcome, settlement_ts)
append(EFFECT_COMMIT, settlement_id, pnl_delta, settlement_ts); fsync()   -- ★ D2 的 durable accounting boundary
replay: 顺序读；遇 len/crc 校验失败 → truncate 自此（半写/无效尾部）；
        C = 所有 EFFECT_COMMIT 的 (id, delta, ts)
重建 daily_pnl = Σ_{C} delta 按 UTC 日（只读，不 append → I3）
```

**运行时顺序（冻结）**：**先 durable COMMIT（journal）→ 再更新 in-memory daily_pnl cache**。若两步间 crash，恢复期由 journal 重建缓存 ⇒ I1 恢复成立。

**与 §5.5 绑定**：A/B/C 的判定即 Q5 表；D/E 由 **I2 + durable conflict algorithm** 处理。

无论哪种实现：`daily_pnl` 必须由 committed effects **可重建**（I1/I3），不以内存为权威。

---

## 6. P0-4：`report_pnl` 幂等契约（冻结）

接口（方向保留）：`RiskEngine::report_pnl(settlement_id, pnl_delta, settlement_ts) -> BreachLevel`（实现期可在服务层/持久层做 effect 幂等，语义一致）。

### 6.1 相同请求
```
X / -10 ; X / -10 ; X / -10
→ daily_pnl 只增加一次 -10
```

### 6.2 冲突请求
```
X / -10  然后  X / -20
→ reject（既不累加 -10+-20，也不覆盖为 -20）
```

### 6.3 Restart Semantics（★）
- 幂等**不能只靠** `HashSet/RwLock/HashMap`；必须由**持久化 effect 身份（`accounting_effect_id`）**支撑：
  - 应用前检查：该 `settlement_id` 是否已有持久化 effect → 有则 no-op；
  - 应用与 effect_id 写入**原子提交**。

> 落地以 **§5.6 的 commit marker** 为准：**先 durable commit，再更新内存投影**（§5.6 Q2-A/Q4）；`daily_pnl` 由 committed effects 可重建（I1/I3），内存非权威。
- `pnl_delta` 本身可选持久化（S4），用于审计与重放校验。

### 6.4 Effect Identity 比较（必改3，冻结；D1 落地见 §5.6 durable conflict algorithm）

- `same settlement_id` + `same canonical pnl_delta` + `same settlement_ts` → **no-op**（幂等）。
- `same settlement_id` + **任一不同**（`pnl_delta` / `settlement_ts` / `token_id` / `outcome`）→ **conflict**：`reject + alert + no PnL mutation`（**不累加、不覆盖、无第二笔 effect**）。
- **canonical 定义**：按持久化字段的**确定性规范序列化**比较（`pnl_delta` 用固定精度/定点字符串，避免浮点歧义）。重算必须 **bit-exact** 复现；否则视为 conflict，禁止“近似相等”放行。
- **语义冻结示例**：
  - Same request：`X / -10 / ts1` 重复三次 → **只产生一个 committed effect**。
  - Conflict：`X / -10 / ts1` 之后 `X / -20 / ts1` → **reject，no overwrite，no addition，no second effect**。
  - Restart：committed effect 存在 + 重启 + 同一请求 → **no-op**。
- **实现约束**：持久落地即 §5.6 的 durable conflict algorithm（事务内 canonical_compare）；`UNIQUE(settlement_id)` 只能保证唯一行，**不得单独**宣称 conflict semantics 已闭合。

---

## 7. 设计约束：Lock ≠ Exactly-Once（显式声明）

> `RwLock / Mutex / atomic claim` **只解决 in-process 并发**，**不保证** crash consistency、restart recovery、跨组件 exactly-once。
> 因此**不得**用“atomic HashSet claim”作为 P0-3 的最终方案；exactly-once 必须由 §5 的持久化 effect 身份 + §11 启动恢复共同保证。

---

## 8. （并入 §3.3 / §9）

---

## 9. P1-2：Settlement Timestamp 语义（冻结）

- `settlement_ts` **唯一来源 = 权威 settlement event**；**不得**用 `now()/received_at/processed_at`。
- 用途：
  - `FillRecord.settled_at`（现为 `now_secs()`，`fill_records.rs:151`，**需改为 settlement_ts**）；
  - `DailyLoss` bucket（§10）。
- `received_at` / `processed_at` **仅用于监控/延迟/freshness/恢复**，**不得**决定 realized PnL 的 accounting day。

---

## 10. P1-3：Late Settlement 行为（冻结 + 授权要求）

示例：
```
settlement_ts = 2026-09-17 23:59:59 UTC
received_at   = 2026-09-18 00:00:05 UTC
→ PnL belongs to 2026-09-17
```
- **归属于 settlement_ts 的 UTC 日**，不因进程晚收而归入次日。
- 历史日期 bucket 处理：现有 `DailyLossLimit` 仅维护单一 `daily_pnl` + `last_reset`（`risk_engine.rs:510-552`），**无法原生支持历史 bucket**。若需支持：
  - **不得**自行改 Risk Model；
  - 必须提出 **Human Acceptance / Risk Model authorization**（HA-005）。
- 在不改 Risk Model 的前提下，备选：迟到结算写对应历史日 bucket，并**不影响当日 gate**；无历史 bucket 能力时，记为 `RETRYABLE/FAILED` 并 alert（保守，不静默错桶入当日）。
- **Gate 行为（必改4，冻结）**：若迟到结算**无法正确归入其 `settlement_ts` 的 UTC 日 bucket**：
  - 状态记 `RETRYABLE`（可重试）或超限后 `FAILED`；
  - **FAIL-CLOSED：`TradingGate` 停新单（`reconcile_complete(false,false)` 或等价），直到该结算被正确入账或人工处置**；
  - **不得只 alert 后静默继续**；**不得**把历史 PnL 错桶进当日 gate；
  - 该 gate 行为涉及 Risk Model 授权（HA-005）；未批准前默认按此最保守执行。

---

## 11. P1-4：Restart Recovery 与 Idempotency 统一（Exactly-Once 四件套）

```
Exactly-Once Settlement Accounting
  = 持久化 Settlement State
  + 持久化 Accounting Effect Identity
  + 幂等 RiskEngine effect
  + 启动 Recovery
```
启动流程（冻结）：
```
① 读取持久化 SettlementRecord / effect 记录
② 无 committed effect（D1：未 COMMIT / 已 rollback；status ∈ {PENDING, ACCOUNTING, RETRYABLE}）
     → 重放 accounting（§5.5 D1 Case A/B；D2 Case B）
③ 有 committed effect：
     D1：status 必已 ACCOUNTED（同一事务）→ no-op（§5.5 D1 Case D）
     D2：业务状态 marker 未完成 → 仅补状态、不重复 effect（§5.5 D2 Case C）
④ 需要时重放/加载 FillLedger 持久化以支撑重算（否则以 SettlementRecord 为权威重放源）
⑤ 恢复后 TradingGate 遵循既有 FAIL-CLOSED
```
- restart recovery 与 idempotency **不可割裂**；四件套缺一不可。

---

## 12. 模块责任边界（不变）
| 模块 | 职责 | 不得 |
|------|------|------|
| FillLedger | fills、结算会计、权威 `pnl_delta`、settlement 幂等记录 | 不接 Risk 阈值 |
| RiskEngine | 消费权威 delta → DailyLoss → BreachLevel → gate；**effect 幂等** | 不重算 accounting |
| RiskStateInternal | projection only | 第二本账 |
| Python | read projection / consumer | 自算 PnL |

---

## 13. Production Call Chain（每环需 code+test+path 证据）
```
UMA/Settlement Authority
 → event ingestion                      (uma_monitor.rs:165；生产接线待定)
 → decoding                             (uma_monitor.rs:261-310；outcome=POC-2)
 → identifier/condition mapping          (POC-1，缺失)
 → token_id mapping                      (POC-1)
 → settlement identity (settlement_id)   (POC-3)
 → FillLedger settlement accounting      (fill_records.rs:401-422)
 → authoritative pnl_delta               (需新增返回，§3.4)
 → RiskEngine effect (幂等, 持久)         (§5 S5 / §6)
 → DailyLossLimit                        (risk_engine.rs:555-594)
 → BreachLevel                           (risk_engine.rs:568-577)
 → check_gates                           (risk_engine.rs:986-1005)
 → RiskStateInternal projection           (service.rs:194-235；需接线)
 → Python risk_gate                       (risk_gate.py:107-114)
```

---

## 14. 测试规格（含新增两例，必须自动化）

| # | 测试 | 期望 |
|---|------|------|
| 14.1 | `test_concurrent_duplicate_settlement` | 同 id 同 outcome 并发 → 恰好一笔 effect |
| 14.2 | `test_conflicting_duplicate_outcome` | 同 id 先 A 后 B → reject，无第二笔 |
| 14.3 | `test_multiple_fills_settle_pnl` | 多 fill 聚合 = 权威（引用 `test_ledger_multi_fill_settle_by_token`） |
| 14.4 | `test_partial_fill_pnl` | requested/filled/unfilled/settled 正确 |
| 14.5 | `test_cross_utc_day_bucket` | 23:59:59 vs 00:00:00 分桶正确（归属 settlement_ts 日） |
| **14.6** | **Test A：`test_crash_after_risk_effect`**（D1） | COMMIT 成功 → crash → restart：**committed effect == 1**、`status == ACCOUNTED`、rebuild、no-op。断言：`effect_count == 1`；`status == ACCOUNTED`；`daily_pnl == expected delta`；replay **不新增 effect** |
| **14.7** | **Test B：`test_crash_before_risk_effect`**（D1） | transaction 尚未 COMMIT → crash → SQLite rollback → restart → retry → **恰好一笔 committed effect** → ACCOUNTED。断言：`effect_count == 1`；`status == ACCOUNTED`；`daily_pnl == expected delta`；第二次 restart = **no-op** |
| 14.8 | `test_restart_duplicate_event` | ACCOUNTED + 重启 + 重复事件 → 额外 PnL=0 |
| 14.9 | `test_daily_loss_circuit_breaker_integration` | 全链 Settlement→…→`check_gates` 拒单 |
| 14.10 | `test_risk_state_projection_daily_pnl` | 引擎/投影/Python 一致，非 0 |
| 14.11 | `test_settlement_recovery_replay_idempotent` | 断言：**I1** `daily_pnl(day) == Σ committed delta`；**I2** 每 `settlement_id` committed effect ≤ 1；**I3** replay N 次 == replay 一次；`effect_count` 不增加；**journal 不发生 append** |
| **14.12** | **`test_sqlite_transaction_atomicity`**（新增） | 直接证明 D1 transaction boundary：**A** crash before COMMIT → `effect_count == 0`、`status != ACCOUNTED`、`daily_pnl` 不变；**B** COMMIT → `effect_count == 1`、`status == ACCOUNTED`、`daily_pnl` 正确；**C** same settlement_id + different canonical payload → reject、`effect_count` 仍为 1、`daily_pnl` 不变 |

鉴别力：短路 effect 幂等 / 去掉持久化恢复 → 14.1/14.6/14.7/14.12 必 FAILED。
`effect_count` 定义：持久化 `AccountingEffect` / WAL `EFFECT_COMMIT` 中该 `settlement_id` 的记录条数（**必须恰为 1**）。
并断言 **I1–I3**：`daily_pnl(day) == Σ committed delta`（I1）、`effect_count == 1`（I2）、`SettlementRecovery` 重放多次 == 一次且不新增 effect、journal 不 append（I3）。

---

## 15. Allowed / Forbidden Scope（完整）

**Allowed（V4 阶段）**：仅 `docs/design/settlement_pnl_risk_wiring.md` + POC 代码/fixture/decoder/mapping 测试。

**Allowed（V4 获批后 implementation，需另行授权）**
- `fast/fill_records.rs`（返回权威 delta；接受 `settlement_ts`；**不改 accounting 口径**）
- `fast/risk_engine.rs`（新增 `report_pnl` 幂等转发 + `daily_pnl` getter + effect 幂等；**不改阈值/分级/窗口**）
- `grpc/service.rs`（settlement 应用入口 + projection 接线）
- event consumer / `bin/server.rs`（UMA 轮询接线）
- 持久化（SettlementRecord / effect journal / WAL）新文件 —— **架构变更，需 HA-006 授权**

**Forbidden**
- 修改 Risk Model / DailyLoss 阈值 / BreachLevel 语义 / 窗口逻辑 / position limit / 集中度 / Gas / 熔断判定
- 修改 strategy / forecast / entry / exit / pricing
- 修改 fund limits / wallet controls / capital allocation / withdrawal
- 重新设计 PnL accounting（除非独立审核明确授权）
- 改变现有 **net** accounting 口径
- POC-1/2/3 完成前实现生产 settlement→PnL
- 用 lock/atomic claim 冒充 exactly-once

---

## 16. Human Acceptance

| ID | 项 | 要求 | 状态 |
|----|----|------|------|
| HA-001 | Settlement Authority（A=链上/UMA，B=应用侧） | POC 后冻结，倾向 A | 待 Human |
| **HA-002** | Net PnL | **确认 G1-001 不改变现有 net（`compute_pnl`）口径**（非重新选择） | 待 Human 确认 |
| HA-003 | Cross-day 归属 | 使用 `settlement_ts` 的 UTC 日 | 待确认 |
| HA-004 | Rust 唯一 Risk Authority | 是 | 待确认 |
| HA-005 | 历史 bucket / 迟到结算 | 是否需历史 bucket（否则记 RETRYABLE/FAILED+alert）；涉及 Risk Model 授权 | 待确认 |
| HA-006 | 持久化方案（WAL/SQLite）+ 新增依赖 | 架构变更授权；**Human 倾向 D1 SQLite**（待复审后正式冻结） | 待冻结 |
| **HA-007** | **提供 UMA 原始事件 fixture / RPC 环境** | 解除 P0-2 POC BLOCKED | **待提供** |

---

## 17. 完成度自评（对照定点整改）

| 项 | 状态 |
|----|------|
| P0-1 PnL 权威/net 冻结 | ✅ |
| P0-2 UMA POC | ❌ **BLOCKED（HA-007）** |
| P0-3 accounting effect 身份 + commit point + Case A–E | ✅ 语义冻结（v7 已统一 D1 单事务模型，消除 Case C 冲突；**待独立复审**） |
| P0-4 report_pnl 幂等（含重启） | ✅ 语义冻结 |
| Lock≠ExactlyOnce 约束 | ✅（§7） |
| P1-1 net 措辞 | ✅（§3.3/HA-002） |
| P1-2 settlement timestamp | ✅（§9） |
| P1-3 late settlement | ✅（§10 + HA-005） |
| P1-4 restart recovery 统一 | ✅（§11） |
| Test A/B 自动化 | ✅ 规格（§14.6/14.7） |
| Allowed/Forbidden | ✅（§15） |

> **P0-2 未关闭 ⇒ G1-001 仍不能进入 implementation。** v7 提交后 **STOP**，等待独立复审；本表不构成自证 / APPROVED。请提供 HA-007 环境以解除 POC BLOCKED。
