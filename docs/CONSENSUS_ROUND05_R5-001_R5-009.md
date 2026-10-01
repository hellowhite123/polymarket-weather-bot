# CONSENSUS_ROUND05_R5-001_R5-009.md

> **来源与诚实声明**：本文件由 OpenCode（实施者）转录**独立审查方 2026-09-18 给出的结论**以归档。
> 审查范围：R5-001（Sell 分支集中度 / TOCTOU）、R5-009（Docker healthcheck / market_discovery 生命周期）。
> 本次整改提交：`c7829d4`（R5-001 测试重做）、`4f77d2e`（R5-009 运行手册）。
> **本文件不是 OpenCode 自证 VERIFIED**；结论归于独立审查。

---

## 一、R5-001 — ✅ CONFIRMED（可关闭）

独立审查确认上一轮「测试证据不足」问题已解决：

- **Test A** `test_sell_concentration_rejected_without_position_limit_breach`：`max_position=5000 / committed=9900 / Sell 200 / future_total=10100` → 失败原因被隔离为 **Position PASS + Global PASS + Concentration FAIL**；并断言错误信息**含「集中度限制」**、**不含「仓位硬限制」/「全局暴露限制」**。
- **Test B** `test_sell_concentration_failure_does_not_release_existing_reserve`：Sell 集中度失败后**既有 Buy 预留不变**。
- **Test C**：Buy 失败释放本次新增预留（既有 rollback 测试保持通过）。
- **Test D（mutation）**：正确实现 `already_reserved = matches!(side, Side::Buy)` → PASS；临时改 `true` → **Test A/B 均 FAILED**；恢复后 PASS。→ 测试具备鉴别力。

**冻结语义（须保持）**：
```
Buy : 创建 reserve；集中度计算不重复加本次 notional；失败则释放本次新增 reserve。
Sell: 不创建本次 reserve；集中度计算必须加本次 notional；失败不得释放既有 Buy reserve。
```

**禁止**：继续修改 R5-001 代码/测试（除非新 Finding）；不得为全库 lint 而去改 RiskConfig / concentration 阈值 / Position Limit / Buy-Sell 语义 / 重构 RiskEngine / 策略 / Kelly / 资金控制。

---

## 二、全库 fmt / clippy 状态 — PRE-EXISTING BASELINE FAILURE

独立审查确认：全库 `cargo fmt --check` 与 `cargo clippy -D warnings` 仍失败，**属预存基线问题，非 R5-001 引入**。

- 涉及文件：`server.rs`、`fill_records.rs`、`market_events.rs`、`mempool.rs`、`nonce_manager.rs`、`order_state.rs`、`state_sync.rs`、`service.rs`、`tests/*`。
- clippy：`non_snake_case` JSON 字段（`apiKeys`/`orderType`/`orderID`/…）、`very complex type`、`Err`-variant 过大等；JSON 字段名**属既有契约，不得为 R5-001 擅自改名**。

**最终报告口径（冻结）**：
```
Global fmt/clippy gate = PRE-EXISTING BASELINE FAILURE
R5-001 authorized file (risk_engine.rs) = clean
```
> **禁止**声称 `cargo clippy -D warnings PASS`；**禁止**为 R5-001 擅自扩大范围清理这些 baseline lint。

---

## 三、R5-009 — ⛔ BLOCKED — ENVIRONMENT（设计通过）

独立审查结论：**设计方向通过**（不是 CHANGES_REQUESTED），**等待真实 Docker Runtime Evidence**。

**冻结语义（须保持；勿改回）**：
```
market_discovery 正常 → container Up → health healthy
market_discovery 异常退出 → entrypoint 不立即退出 → container 继续 Up
                         → healthcheck 检查真实 market_discovery
                         → 连续失败 3 次 → health unhealthy
```
核心原则：**Container Up ≠ Business Healthy**。

**职责划分（保持）**：entrypoint 只负责「保持容器存活 + 记录 subprocess death」；healthcheck 负责「业务健康判断」；Docker 负责 `healthy/unhealthy`。**禁止**改回 `discovery death → entrypoint exit 1 → container restart`；**禁止**在 entrypoint 中自行实现「失败三次」计数。

**不得再改 R5-009 代码**（除非新 Finding）。

解除 BLOCKED 需在**有 Docker 的主机**执行 `docs/DEPLOY_HEALTHCHECK_RUNBOOK.md` §5 的 5 步实机验收并附证据。

---

## 四、状态汇总

| 项 | 状态 | 备注 |
|----|------|------|
| R5-001 | ✅ CONFIRMED（可关闭） | 测试重做 + mutation 通过；不改代码 |
| 全库 fmt/clippy | ⚠️ PRE-EXISTING BASELINE FAILURE | 非 R5-001 引入；勿越权清理 |
| R5-009 | ⛔ BLOCKED — ENVIRONMENT | 设计通过；待 Docker Runtime Evidence |

## 五、下一步
1. R5-001：关闭（无需再动）。
2. R5-009：在真实 Docker 环境执行运行手册 §5 验收，补 Runtime Evidence。
3. 全库 fmt/clippy 基线：若需满足门禁，须**单独授权**（全库 rustfmt 或显式 `#[allow]` 白名单），不得借 R5-001 之名进行。
