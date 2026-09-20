# 设计提案 V6 复审投喂清单（G1-001 §5.6 重做）

> 供**独立审查 AI / Human** 使用。本轮仅复审 `settlement_pnl_risk_wiring.md` 的 **v5 → v6 §5.6 重做**（及 §5.5/§6/§14 联动）。
> 其余文档/章节维持原版本；四个设计文档的横向复审见 `REVIEW_CHECKLIST_DESIGN_V3.md`。
> **OpenCode 为工程实施者，本清单不构成批准；结论不得由实施者自证。**

## 0. 复审输入

| 项 | 值 |
|----|----|
| 快照 commit | `511dad8` |
| 被审文档 | `docs/design/settlement_pnl_risk_wiring.md`（**v6**，§5.6 重做，+65/-21） |
| 上一版 | v5（`0ba8a78`）；其自审见 `CONSENSUS_DESIGN_ROUND_05.md`（必改 1 = CHANGES_REQUESTED） |
| 产出 | `docs/design/CONSENSUS_DESIGN_ROUND_06.md`（建议） |
| 全局状态 | **LIVE = NO-GO**；P0-2 POC **BLOCKED** |

---

## 1. §5.6 的 7 问逐条核对

| 问 | 要求 | v6 位置 | 复核动作 | 通过判据 |
|----|------|---------|----------|----------|
| Q1 | 谁 durable accounting authority | §5.6 Q1 | 是否有唯一权威、且非 RiskEngine/内存 | 明确 `SettlementJournal` 为唯一权威 |
| Q2 | daily_pnl = A(replay) 还是 B(durable txn) | §5.6 Q2 | 是否明确选 A 并解释内存为缓存 | 选 **A**，`daily_pnl(day)=Σ committed delta` |
| Q3 | transaction boundary | §5.6 Q3 | 是否只覆盖持久产物、内存不入边界 | 边界=AccountingEffect+SettlementRecord |
| Q4 | commit marker | §5.6 Q4 | 是否有唯一“effect 已生效”标记 | committed `AccountingEffect`（D1 行/D2 `EFFECT_COMMIT`） |
| Q5 | crash 判定（未生效/已生效/incomplete） | §5.6 Q5 | 三态是否有可观测判据 | 无 commit/有 commit/尾部损坏 三态表 |
| Q6 | 唯一恢复模块 | §5.6 Q6 | 是否唯一且启动先于 Gate Ready | `SettlementRecovery` |
| Q7 | 证明 committed==recovered 且重复 effect | §5.6 Q7 + §14 | I1/I2/I3 是否可判定且有测试 | 见 §2 |

**关键**：v5 被判失败的原因是“只写 SQLite/WAL 可以做到”。请确认 v6 **已给出 invariant 与协议**，而非再次断言。

---

## 2. I1–I3 核对（v5 缺失项）

| Invariant | v6 定义 | 复核动作 |
|-----------|---------|----------|
| **I1 PnL consistency** | `daily_pnl(day) == Σ_{committed e, UTCday(e)==day} e.pnl_delta`，缓存必须=投影 | 是否可判定；恢复后是否强制 |
| **I2 settlement_id uniqueness** | 每 `settlement_id` committed effect ≤ 1 | D1 `UNIQUE` / D2 append-once 是否落实 |
| **I3 replay determinism** | rebuild 为纯函数；多次==一次；重放只读不新增 effect | D2 是否明示“只读不 append”；测试 14.11 |

---

## 3. D1 / D2 协议核对

**D1 SQLite**
- [ ] 表结构含 `effects(settlement_id PK)`、`settlements`；
- [ ] 事务 `BEGIN IMMEDIATE … COMMIT` 覆盖 effect 写入 + status 更新；
- [ ] 重建投影 SQL 存在（`GROUP BY utc_day`）；
- [ ] 未提交事务崩溃 → 自动回滚 = “未生效”（Q5）。

**D2 WAL**
- [ ] 记录 framing `[len][crc32][type][json]`（半写/损坏可检测）；
- [ ] `EFFECT_COMMIT` 后 `fsync` = durability/commit 点；
- [ ] replay：顺序读、校验失败截断、collect commits、**只读不 append**；
- [ ] `daily_pnl` 由 commits 重建。

**运行时顺序（v5 修正点）**
- [ ] 是否明确“**先 durable commit，再更新内存缓存**”；
- [ ] 是否说明“两步间 crash → 恢复由 journal 重建 ⇒ I1 成立”。

---

## 4. 联动章节核对

- [ ] §5.5 Case A–E 是否指向 §5.6 的 Q5/Q6 判定；
- [ ] §6 `report_pnl` 是否以 §5.6 commit marker 为准（先 commit 后更新投影）；
- [ ] §14 是否含 I1–I3 断言 + `test_settlement_recovery_replay_idempotent`(14.11)。

---

## 5. 仍需关闭 / 阻断项

- **P0-2 UMA POC = BLOCKED**（`settlement_id` 未验证 → §4.5 前置链未解除）。解除条件：**HA-007**。
- **HA-006**：D1 vs D2 选型 + 持久化依赖（架构变更）。
- 未解除前：**禁止生产 settlement→PnL 实现**。§5.6 重做不影响该阻断。

---

## 6. Human Acceptance（G1-001，摘要）

HA-001 authority（POC 后）、HA-002 确认不改变现有 net 口径、HA-003 cross-day=`settlement_ts` UTC 日、HA-004 Rust 唯一 authority、HA-005 迟到结算/历史 bucket（gate）、HA-006 持久化选型、**HA-007 提供 UMA fixture/RPC**。

---

## 7. 复审结论模板（`CONSENSUS_DESIGN_ROUND_06.md`）

```markdown
| Q1..Q7 | PASS / CHANGES | §5.6 | 备注 |
| I1/I2/I3 | PASS / CHANGES | §5.6 Q7 / §14 | |
| D1 协议 | PASS / CHANGES | §5.6 | |
| D2 协议 | PASS / CHANGES | §5.6 | |
| 运行时顺序 | PASS / CHANGES | §5.6 | |
| 联动 §5.5/§6/§14 | PASS / CHANGES | | |

整体：settlement_pnl_risk_wiring.md v6 = APPROVED / CHANGES_REQUESTED / REJECTED
残留：P0-2 POC BLOCKED（HA-007）；D1/D2 选型（HA-006）
下一步：拆分 MASTER_TASKS
```

---

## 8. 停止条件

> v6 通过独立复审 + Human 批准（含 HA-006/007）前，**不实施任何生产代码**；P0-2 POC 未解除 BLOCKED 前，**禁止生产 settlement→PnL 实现**。**LIVE = NO-GO。**
