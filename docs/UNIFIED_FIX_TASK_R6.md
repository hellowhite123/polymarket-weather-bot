# UNIFIED FIX TASK — AI Weather 整改整合任务（主任务：R6）

> 单一整合任务。一个主任务挂全部待办整改子项，状态机 TODO → IN_PROGRESS → FIXED → VERIFIED。
> VERIFIED 由**非实施者**（伯森先生）判定。红线不变：不改交易语义 / RiskConfig 阈值含义 / 聚合器与 gRPC 契约 / 大规模重构 / cold_wallet 与签名标准实现。
> 执行入口：实施者按 T1→T5 顺序，一次一个子任务，完成后走验收门禁。
> 独立审查证据基线：Python `pytest` **123 passed**；Rust `cargo test` **338+4+18+1=361 passed**（git archive 副本实跑）。

## 状态总览
| 子任务 | 级别 | 内容 | 状态 |
|---|---|---|---|
| T1 | P1 | risk_gate.py Shares vs USDC 单位混淆修复 | ⏳ TODO（指令已备好） |
| T2 | P1 | R5-001 集中度分子口径残留修正 | ⏳ TODO |
| T3 | P2 | R5-002 cap 溢出测试补充 | ⏳ TODO |
| T4 | P1 | `position_notionals` 数据链接线补齐 | ⏳ TODO |
| T5 | P3 | 修正 `04665b7` commit message 记录矛盾 | ⏳ TODO |

---

## T1 — risk_gate.py 单位混淆修复（P1）
- **现状**：`_check_position_limit`(132行) / `_check_global_exposure`(152行) 拿 shares 对比 USD 上限，正常低价单被 BLOCKED（实链 `aggregator.py:338-339`）。经独立审查**实测复现**（1500份@0.10被误拦）。
- **细节**：BUY_NO 用 `target_price` 会高估 4x；存量应取 `position_notionals`（USDC）。
- **交付指令**：`docs/INSTRUCTION_TO_OPENCODE_AGI1_riskgate_units.md`（完整可执行，含 6 测试 + 鉴别力验证）。
- **Allowed Files**：`risk_gate.py`、`models.py`(仅辅助方法)、`test_aggregator.py`。
- **验收锚点**：pytest ≥125 全绿；`_order_notional_usdc` 改回旧公式必 FAILED。

## T2 — R5-001 集中度分子口径残留（P1）
- **现状**：`risk_engine.rs` `check_concentration` 分母已用 `committed_total`（含 reserved），但分子 `new_market` 仍用 `market_position()`（仅 filled，425行 `return e.notional`）→ 同 token 多笔 pending 预留时集中度被低估。
- **改法**：新增 `market_committed(token_id)`（filled+reserved），分子改用之；分母保持 committed，禁止混口径。
- **Required Tests**：同 token「已 reserve A 再下单 B」集中度必须计入 A 的 pending；mutation 鉴别（改回 filled-only 必 FAILED）。既有 `test_concentration_counts_pending_reserves`(1644) / `test_sell_concentration_*`(1679,1718) 全绿。
- **Allowed Files**：`polymarket-rust/.../src/fast/risk_engine.rs`。
- **验收**：cargo test 361 及以上全绿；clippy 无新增告警。

## T3 — R5-002 cap 溢出测试补充（P2）
- **现状**：`_calculate_quantity` 有 `min(advisory_notional, max_position_per_market)`，现有测试 T4 断言 `< base*kelly/cost` 上界，但缺「cap 恰好命中 / 超大 notional 被压回市场上限」的显式断言。
- **改法**：`test_multi_source.py` 增用例：构造 edge→1 且 advisory≫cap，断言 quantity = `max_position_per_market / cost`。
- **Allowed Files**：`test_multi_source.py`。
- **验收**：pytest 全绿。

## T4 — position_notionals 数据链接线补齐（P1）
- **现状**：`models.py:170` 字段定义存在，但**无任何生产写入点** → `get_global_exposure()` 实际回退到 shares 之和（196-202行）。涉及 RiskState 的 USDC 存量权威性，与 T1 强相关。
- **改法**：需设计（谁回写：聚合器 on_fill / Rust 成交回写 / 启动对账对齐）。**保守方案**：T1 采用 FAIL-CLOSED（存量 USDC 不可用即拒单）防低估；本条为根因补齐，须先出方案经伯森先生批准后实施。
- **红线**：触碰 `risk_engine.rs` / 记账语义须显式授权。
- **状态**：⏳ 需设计 → 批准后实施。

## T5 — 修正 commit message 记录矛盾（P3）
- **现状**：`04665b7` message 自述「R5-002 未实施 / pytest 109」，与工作区实际（R5-002 已实施于 `a2dde81`、pytest 123）不符。
- **改法**：修订 message 或补一条说明 commit；不动代码。
- **验收**：`git log` 记录与事实一致。

---

## 通用验收门禁（每个子任务提交前）
- Python：`python -m pytest -q` 全绿（基线 123）。
- Rust（仅 T2）：`cargo test` 全绿（基线 361）+ `cargo clippy --all-targets` 0 error（T2 不得引入新告警）。
- 文件锁提醒：若 `fast/*.rs` 再现独占锁，用 `git archive HEAD | tar -x -C <tmp>` 解副本跑测试。
- 工作区 `git status` 除授权文件外无多余改动。

## 引用文件
- 本任务 T1 完整指令：`docs/INSTRUCTION_TO_OPENCODE_AGI1_riskgate_units.md`
- 审查背景与历史结论：`docs/HANDOFF_r5_audit_and_next.md`