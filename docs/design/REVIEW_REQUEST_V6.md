# REVIEW_REQUEST_V6.md — G1-001 复审请求（可直接投喂）

> 本文件为**可直接复制投喂给独立审查 AI / 审查方**的自包含复审请求。
> 相关：[`REVIEW_CHECKLIST_DESIGN_V6.md`](./REVIEW_CHECKLIST_DESIGN_V6.md)、[`CONSENSUS_DESIGN_ROUND_05.md`](./CONSENSUS_DESIGN_ROUND_05.md)。

---

**【复审请求】G1-001 `settlement_pnl_risk_wiring.md` Draft v6（仅 §5.6 重做）**

**背景**：本项目为 Polymarket 天气盘量化系统（Rust 执行层 + Python 策略层），角色为「OpenCode = 工程实施者，独立 AI / Human = 审查」。上一版 v5 的 §5.6 Durable Transaction Boundary 被判 **CHANGES_REQUESTED**（只写“SQLite/WAL 可以做到”，缺 transaction/recovery invariant），自审见 `docs/design/CONSENSUS_DESIGN_ROUND_05.md`。

**审查输入（固定快照）**
- commit：`511dad8`（v6 正文）；`f98d61f`（登记 HA-006 倾向）
- 被审文档：`docs/design/settlement_pnl_risk_wiring.md`（v6，仅重做 §5.6，并联动 §5.5/§6/§14）
- 复审清单：`docs/design/REVIEW_CHECKLIST_DESIGN_V6.md`
- 上游：`docs/design/CONSENSUS_DESIGN_ROUND_05.md`

**请核对（§5.6 v6）**
1. **Q1–Q7 是否逐问给出可执行答案**（不是再次断言“SQLite/WAL 能做到”）：durable authority；`daily_pnl` = A 还是 B；transaction boundary；commit marker；“未生效 / 已生效 / incomplete” 三态判定；唯一恢复模块；如何证明。
2. **I1/I2/I3 是否定义且可判定**：I1 `daily_pnl(day) == Σ committed delta`；I2 每 `settlement_id` committed effect ≤ 1；I3 replay 纯函数、多次 == 一次、只读不新增。
3. **D1 SQLite 协议**是否完整（表 / `UNIQUE` / 事务边界 / 投影重建 SQL / 未提交事务 = 未生效）。
4. **D2 WAL 协议**是否完整（`[len][crc32][type][json]` framing、`EFFECT_COMMIT + fsync` = commit、半写截断、只读重放）。
5. **运行时顺序**是否明确「先 durable commit，再更新内存缓存」，并论证两步间 crash 由 journal 重建 ⇒ I1 成立。
6. **联动**：§5.5 Case A–E 是否指向 §5.6 判定；§6 是否以 commit marker 为准；§14 是否含 I1–I3 断言与 14.11 重放幂等测试。

**已知未决（请据此给结论，不需 OpenCode 自行解决）**
- **P0-2 UMA POC 仍 BLOCKED**（`settlement_id` 未验证）→ 端到端 settlement → PnL 不可实施；
- **HA-006** 持久化选型（Human 倾向 **D1 SQLite**）待正式冻结；**HA-007** 需提供 UMA 原始事件 fixture / RPC 环境。

**约束**：本轮仅设计，未改任何生产代码；请勿要求直接实现。

**预期输出**：在 `docs/design/CONSENSUS_DESIGN_ROUND_06.md` 按 `REVIEW_CHECKLIST_DESIGN_V6.md` §7 模板逐项判 `PASS / CHANGES`，并给出 v6 整体 `APPROVED / CHANGES_REQUESTED / REJECTED`。

---

## 附：结论模板（审查方填）

```markdown
| 项 | 结论 | 位置 | 备注 |
|----|------|------|------|
| Q1 durable authority | PASS / CHANGES | §5.6 | |
| Q2 daily_pnl = A | PASS / CHANGES | §5.6 | |
| Q3 transaction boundary | PASS / CHANGES | §5.6 | |
| Q4 commit marker | PASS / CHANGES | §5.6 | |
| Q5 crash 三态 | PASS / CHANGES | §5.6 | |
| Q6 唯一恢复模块 | PASS / CHANGES | §5.6 | |
| Q7 证明 | PASS / CHANGES | §5.6 / §14 | |
| I1 / I2 / I3 | PASS / CHANGES | §5.6 Q7 / §14 | |
| D1 协议 | PASS / CHANGES | §5.6 | |
| D2 协议 | PASS / CHANGES | §5.6 | |
| 运行时顺序 | PASS / CHANGES | §5.6 | |
| §5.5/§6/§14 联动 | PASS / CHANGES | — | |

整体：settlement_pnl_risk_wiring.md v6 = APPROVED / CHANGES_REQUESTED / REJECTED
残留：P0-2 POC BLOCKED（HA-007）；D1/D2 选型（HA-006，倾向 D1）
下一步：拆分 MASTER_TASKS（Allowed / Forbidden / Required Tests / Acceptance）
```
