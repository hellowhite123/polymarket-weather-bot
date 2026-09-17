# 设计提案独立核对清单（G1-001 / G1-002）

> 供独立审查 AI / Human 使用。对象：`docs/design/` 下两份设计提案。
> 依据 `AGENTS.md`、`docs/CODE_REVIEW_STANDARD.md`。
> **OpenCode 为工程实施者，本清单不构成批准。**

---

## 0. 待审文档

| 文档 | 关联 | 影响 |
|------|------|------|
| `docs/design/update_settlement_times_rpc.md` | G1-002 / P0-RISK-002 | proto 新增 RPC + Python 推送 + Rust 存储/回传 |
| `docs/design/settlement_pnl_risk_wiring.md` | G1-001 / P0-RISK-001 | 结算→PnL→日亏熔断；可能放开 `risk_engine.rs`/`fill_records.rs` 有限 Allowed |

## 1. 通用核对项（两份都查）

- [ ] **证据属实**：文中每个 `file:line` 引用是否与当前 commit（`5504579`）一致，是否确有该问题。
- [ ] **根因覆盖**：方案是否真正解决根因，而非绕开（对照 `CODE_REVIEW_STANDARD.md` §2「模块存在但未接入 = P0」）。
- [ ] **红线合规**：是否改动交易语义 / Risk Model 阈值 / Strategy / 资金控制逻辑？若有，是否在「待审批清单」显式申请。
- [ ] **fail-closed**：数据/网络异常时的行为是否保守、明确，无静默 fail-open 残留。
- [ ] **幂等与并发**：重复推送/重复结算/并发读写是否会重复记账或状态倒退。
- [ ] **兼容性**：proto 变更是否向后兼容；Python pb2 重新生成流程（`DECISIONS.md` C-009）是否覆盖。
- [ ] **可测试性**：测试计划是否含「可证伪」用例（鉴别力），而非只覆盖 happy path。
- [ ] **可回滚**：变更是否可小步上线/回退。

## 2. `update_settlement_times_rpc.md` 专项

- [ ] `set_settlement` 生产零调用的证据是否属实（`fast/risk_engine.rs` grep）。
- [ ] `to_proto` 写死 `settlement_times: Vec::new()` 的证据（`grpc/service.rs:224`）是否属实。
- [ ] Python `risk_gate._check_settlement_window` 因字段恒空失效的推断是否成立（`risk_gate.py:80-105` + `converter.py:156-165`）。
- [ ] 新增 RPC 的命名/字段是否与既有风格一致；返回体设计是否够用（accepted/rejected）。
- [ ] 「回传 settlement_times」是否需要 `risk_engine.rs` 新增只读 getter？若需要，是否已在待批清单申请、且声明不改判定/阈值。
- [ ] **Q1 未知结算时间处置**（a 拒绝 / b 放行 / c 启动宽限）——请给明确选择与理由。
- [ ] **Q2 `replace_all` 语义**是否需原子整体替换；是否值得为它动 `risk_engine.rs`。
- [ ] **Q3 推送失败**的 fail-closed 强度。
- [ ] 是否遗漏：已结束/已结算 token 的清理，避免集合无界增长。

## 3. `settlement_pnl_risk_wiring.md` 专项

- [ ] `post_check`/`report_pnl` 生产零调用的证据是否属实。
- [ ] `FillLedger::settle`/`settle_by_token` 生产零调用、`mark_settled` 无调用者的证据是否属实。
- [ ] `RiskStateInternal.daily_pnl` 无写入、两端日亏熔断失效的推断是否成立。
- [ ] **Q1 结算来源**（UMA / Gamma / CLOB / Python 推送）选择是否合理；`uma_monitor` 的 `PriceSettledEvent` 是否真能映射到 `token_id`。
- [ ] **Q2 日亏口径**：是否含手续费；UTC 日切重置由谁触发；跨日累计风险。
- [ ] **Q3 双端权威**：是否统一以 Rust 为唯一权威，避免双阈值漂移。
- [ ] **重复计 PnL**：结算路径与其它路径是否可能重复 `report_pnl`；幂等键是否足够。
- [ ] `settle_by_token` 不返回 PnL 增量 → 需新增聚合；该改动是否被正确限定为「只读/新增，不改口径」。
- [ ] **Q5 是否同意**将 `fast/risk_engine.rs`、`fast/fill_records.rs` 加入 Allowed（有限范围）。

## 4. 结论记录模板

```markdown
| 文档 | 结论 | 阻塞项 | 需 Human 拍板 |
|------|------|--------|---------------|
| update_settlement_times_rpc.md | APPROVED / CHANGES_REQUESTED / REJECTED | Q1/Q2/Q3 | ... |
| settlement_pnl_risk_wiring.md | APPROVED / CHANGES_REQUESTED / REJECTED | Q1/Q2/Q3/Q5 | ... |

理由与修改要求：
- ...

批准后的下一步：
- 拆分为 MASTER_TASKS 可实施条目（含 Allowed / Forbidden / Required Tests / Acceptance）。
```

> ⚠️ 未经审查与 Human 批准，`docs/design/` 两份提案不进入实施（`AGENTS.md` 红线）。
