# MASTER_TASKS.md

**唯一工程修改任务来源。** 实施者（Work Buddy）默认只以本文件作为代码修改授权依据。

- 未经本文件明确授权的任务，实施者不得自行改动交易语义 / Risk Model / Strategy / 资金控制逻辑。
- 每个任务一次只做一个 Task Group，完成后走 `docs/CODE_REVIEW_STANDARD.md` 的验收门禁。
- 状态机：`TODO → IN_PROGRESS → FIXED → VERIFIED → CLOSED`（VERIFIED 由非实施者判定）。

---

## 活跃任务

> 当前无未完成的 P0 上线阻塞项。唯一遗留代码项为 **P1-006**（`on_market_trade` 已实现，但 `MarketEventPoller` 尚未接入启动流程），已明确推迟到 Shadow Trading 阶段（需真实 L2 `/data/trades` 数据流驱动）。详见 `BUG_TRACKER.md`。

---

## 已闭环任务（历史）

### P0-02 Docker Secret 与环境变量一致性

- **Status**: ✅ DONE（09-09 修复 + 4 测试全绿 + commit a963231）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `Dockerfile`
  - `docker-compose.yml`
  - `docker-entrypoint.sh`
  - `polymarket-rust/polymarket-rust/src/wallet/keystore.rs`
  - `polymarket-rust/polymarket-rust/src/bin/server.rs`
- **Forbidden**: `strategy_engine/`
- **Required Tests**:
  - `secret_loaded_consistently_in_docker`
- **Acceptance**: Docker 侧 `WALLET_PASSWORD_FILE` 与 Rust 侧 `WALLET_PASSWORD` 读取链路统一，无不一致风险。

---

### P1-010 Order Dedup claim 占位失败清理（P0-004 遗留）

- **Status**: ✅ DONE（09-10 修复 + 4 测试全绿；代码改动待 Human review 后 commit）
- **Owner**: Work Buddy
- **Allowed Files**:
  - `polymarket-rust/polymarket-rust/src/fast/order_state.rs`
  - `polymarket-rust/polymarket-rust/src/grpc/service.rs`
- **Forbidden**: `strategy_engine/`、`polymarket-rust/polymarket-rust/src/fast/risk_engine.rs`
- **Required Tests**:
  - `claim_retry_after_rejected`
  - `failed_place_order_marks_rejected`
- **Acceptance**: `claim()` 占位后任一下单失败路径（非 OPEN / 熔断 / RiskEngine 缺失 / 风控拦截 / fast path 失败 / 未初始化）将占位标记为 `Rejected`；幂等检查对 `Rejected/Expired/Cancelled` 终态允许重新占位下单，进行中/已成交仍去重。

---

## 旧清单对照（P0-01~P0-14 → 新编号，全部闭环）

> 8/25 第一版 P0 清单（14 项）已全部映射到 `BUG_TRACKER.md` 新编号（P0-001~007 + P1-001~010），全部闭环。下表为对应关系，防止旧任务被误重新执行。

| 旧编号 | 内容 | 新编号 | 状态 |
|--------|------|--------|------|
| P0-01 | FastExecutor API Key / Warmup | P0-001 | VERIFIED（09-08） |
| P0-02 | Docker Secret 一致性 | P0-002 | VERIFIED（09-09） |
| P0-03 | RiskEngine pre_check 接入 | P0-003 | VERIFIED |
| P0-04 | FastExecutor 真实执行路径 | 执行层 15 子模块（185 tests）+ P0-001 调用链 | 已实现；真实 CLOB 下单端到端留待实盘/Shadow Trading |
| P0-05 | 幂等保护 | P0-004 | VERIFIED |
| P0-06 | post_check 基础实现 | P1-005 / P1-006 | DONE |
| P0-07 | TradingGate 状态机 | 保持旧编号 | VERIFIED |
| P0-08 | StateSync 仓位接口 | P0-007 | VERIFIED |
| P0-09 | Position 对账逻辑 | P0-007 | VERIFIED |
| P0-10 | full_reconcile 接入启动 | P0-007 | VERIFIED |
| P0-11 | TradingGate 进入 READY | 保持旧编号 | VERIFIED |
| P0-12 | Pending Orders 状态同步 | P1-005 / P1-007 | DONE |
| P0-13 | 成交回报连接 post_check | P1-005 / P1-006 | DONE |
| P0-14 | 后台周期对账循环 | P1-007 | DONE |

---

## P0 → P1 降级记录

> 资金安全相关项降级按协议需 Human 显式确认。以下理由已记录，待伯森先生最终拍板归档。

| 项 | 降级理由 |
|----|---------|
| post_check 成交闭环（旧 P0-06/13 → P1-005/006） | 逻辑已实现并有单测，但真实成交触发依赖 L2 `/data/trades` 轮询，sandbox 无外网无法端到端验证 → 推迟到 Shadow Trading |
| pending orders 同步（旧 P0-12 → P1-005/007） | 状态同步已有单测，对账闭环已在启动 + 后台 30s 循环实现 |
| 后台对账循环（旧 P0-14 → P1-007） | 已实现每 30s full_reconcile + 差异告警；无资金直接暴露，属健壮性增强 |

> ⚠️ 以上 3 项涉及资金/仓位安全的降级，是否同意归档，请伯森先生明示。
