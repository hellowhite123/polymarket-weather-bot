# 设计方案：UpdateSettlementTimes RPC（P0-RISK-002 结算窗口）

- **状态**：Draft v1，**待独立 AI / 伯森先生审批**（未授权实施前不得改 proto / 代码）
- **作者**：OpenCode（工程实施者）
- **日期**：2026-09-17
- **关联**：`MASTER_TASKS.md` G1-002、`BUG_TRACKER.md` P0-RISK-002、`audits/round_04/CONSENSUS_ROUND_04.md`
- **依赖接口变更**：是（proto 新增 RPC）。按 `AGENTS.md`，须先审批方案再实施。

---

## 1. 问题（现状与证据）

结算前「冻结窗口禁开新仓」逻辑**已实现但两端都未接通**：

1. **Rust 侧**：`RiskEngine` → `TimeWindowGuard::pre_check`（`fast/risk_engine.rs:456-483`）逻辑完整，注册入口 `set_settlement`（`risk_engine.rs:443-448` / `RiskEngine::set_settlement` `:1115-1121`）**无任何生产调用**（grep 仅定义 + 测试）。
2. **Rust 侧**：`get_risk_state` 返回的 `RiskState.settlement_times` 恒为空——`RiskStateInternal::to_proto()` 写死 `settlement_times: Vec::new()`（`grpc/service.rs:224`，注释 `TODO: 从市场数据获取`）。
3. **Python 侧**：`risk_gate.py:80-105` 的 `_check_settlement_window` 会读 `state.settlement_times`，但该字段仅由 `converter.risk_state_from_proto`（`converter.py:156-165`）从 Rust 响应填充；Rust 恒空 → Python 的同类闸门**同样失效**。
4. **缺反向通道**：现有 RPC（`proto/polymarket.proto:243-272`）只有 `GetRiskState` / `StreamRiskStateUpdates`（Rust→Python 方向），**没有 Python→Rust 推送结算时间的接口**。
5. **数据源已存在**：`market_discovery/models.py:76` 的 `MarketInfo.end_date_utc`（截止时间戳，秒），且未结束盘强制要求（`:92-94`）——这正是结算时间的权威来源，但从未传给 Rust。

**后果**：结算前 2h（Rust）/ `settlement_lock_hours`（Python）的开仓保护完全未生效；且 P0-RISK-002 的「未知结算时间即放行」为 fail-open，与 FAIL-CLOSED 约束冲突。

---

## 2. 目标 / 非目标

**目标**
- 建立 Python→Rust 的结算时间推送通道（注册表 `end_date_utc` → Rust `set_settlement`）。
- Rust `get_risk_state` 回传已注册的 `settlement_times`（供 Python `risk_gate` 对账/展示）。
- 推送失败/数据非法时行为明确且 fail-closed（策略见 §6 开放问题）。

**非目标**
- 不修改 `TimeWindowGuard::pre_check` 的窗口判定逻辑与阈值（`DECISIONS.md` C-008，属 Risk Model 红线）。
- 不做市场自动发现（复用现有 `market_discovery` 注册表）。
- 不引入结算/PnL 处理（见配套文档 `settlement_pnl_risk_wiring.md`）。

---

## 3. 方案（推荐 Design A）

### 3.1 proto 变更（`proto/polymarket.proto`）

复用既有 `SettlementTimeEntry`（`:106-109`），新增请求/响应与 RPC：

```proto
// 结算时间推送请求 (Python --> Rust)
message UpdateSettlementTimesRequest {
  repeated SettlementTimeEntry settlement_times = 1;
  // 是否“全量替换”语义：true=以本次为准覆盖（用于清理已结束市场）
  bool replace_all = 2;
}

message UpdateSettlementTimesResponse {
  bool success = 1;
  int32 accepted = 2;   // 接受条目数
  int32 rejected = 3;   // 非法/解析失败条目数
  string message = 4;
}
```

```proto
service ExecutionService {
  // ... 既有 RPC 不变 ...
  // 推送/更新市场结算时间（幂等；用于结算窗口风控）
  rpc UpdateSettlementTimes(UpdateSettlementTimesRequest)
      returns (UpdateSettlementTimesResponse);
}
```

**兼容性**：纯新增 message + RPC，向后兼容；旧客户端不调用即无行为变化。proto 重新生成后，Python 侧需按 `DECISIONS.md` C-009 处理包内导入（`from . import polymarket_pb2`）。

### 3.2 Rust 变更

1. **存储**：直接写入 `RiskEngine`（`set_settlement` / `set_settlements`），由 `TimeWindowGuard` 持有（`risk_engine.rs:431` 的 `settlements: RwLock<HashMap<String,u64>>`）。**无需新增独立存储**。
2. **服务端 handler**（`grpc/service.rs`）：新增 `update_settlement_times`：
   - 逐条解析 `settlement_time`（ISO 8601 → Unix 秒；复用 chrono，`DateTime::parse_from_rfc3339`，兼容带时区偏移）。
   - 非法/空 token_id/解析失败 → 计入 `rejected` 并 `tracing::warn!`，**不写入**。
   - 合法 → `risk_engine.set_settlement(token_id, ts)`；`accepted += 1`。
   - `replace_all=true`：先清空再写入（需 `TimeWindowGuard` 增加 `clear_settlements` getter，**属新增方法**，须一并批；或改为「只增不删」避免动 RiskEngine——见开放问题 Q2）。
   - 返回 `UpdateSettlementTimesResponse`。
3. **回传**（`RiskStateInternal::to_proto`，`service.rs:222-229`）：把 `settlement_times` 填充为已注册集合。
   - 需要 `RiskEngine::settlement_snapshot() -> Vec<(String,u64)>`（`TimeWindowGuard::upcoming_settlements` 已有类似能力 `:486-489`，可扩展为全量快照）。
   - `RiskStateInternal` 不持有结算时间，`to_proto` 需能访问 `RiskEngine`；建议由 `get_risk_state`/`stream_risk_state_updates` 在组装时合并（而非 `RiskStateInternal` 内），降低耦合。

**改动文件预估**：`grpc/service.rs`（handler + 回传组装）、`fast/risk_engine.rs`（**仅新增只读快照/getter，不改判定逻辑**，需在 `MASTER_TASKS` 显式列入 Allowed 并申请）、`bin/server.rs`（无，除错误处理）。

### 3.3 Python 变更

1. **客户端方法**（`strategy_engine/grpc_bridge/client.py`）：新增
   ```python
   async def update_settlement_times(self, entries: dict[str, datetime], replace_all: bool = False) -> dict
   ```
   内部用 `pb.SettlementTimeEntry` 组装，调用新 RPC。
2. **定时推送**：在 `market_discovery` 扫描/注册表更新后（或独立轻量循环，10min）：
   - 从 `MarketRegistry` 读取活跃市场 → `{yes_token_id/no_token_id: end_date_utc}`。
   - 推送 `UpdateSettlementTimes(replace_all=true)`（以注册表为权威全量同步）。
   - 推送失败 → **FAIL-CLOSED**：记录错误并使本地 `RiskState` 进入保守态（见 Q3）。
3. **本地 RiskState 同步**：`pipeline`/`bridge` 维护的 `RiskState.settlement_times` 与注册表同源，避免只依赖 Rust 回传。

---

## 4. 数据流 / 时序（文字）

```
market_discovery registry(end_date_utc)
        │ (Python, 定时/扫描后)
        ▼
GrpcExecutionClient.update_settlement_times(entries, replace_all=true)
        │  gRPC
        ▼
ExecutionServiceImpl.update_settlement_times
        │  逐条校验 ISO8601 / token_id
        ├─ 非法 → rejected++ + warn（不写入）
        └─ 合法 → RiskEngine.set_settlement(token_id, unix_ts)
                               │
                               ▼
             TimeWindowGuard.pre_check(token_id, side)
             （place_order → check_and_reserve → check_gates）
        ▲
        │ get_risk_state 回传 settlement_times
        └── Python risk_gate._check_settlement_window（二次校验/展示）
```

---

## 5. 幂等 / 并发 / 一致性

- **幂等**：`set_settlement` 为 `HashMap::insert`，同 token 重复推送覆盖为最新值，天然幂等。
- **并发**：写为 `RwLock` 写锁，读为 `pre_check` 读锁；无锁跨 await（同现有模式）。
- **一致性**：`replace_all` 若采用「先清后写」，在清空瞬间存在读到空集的窗口（短暂 fail-open）。建议改为**构建新 HashMap 后整体替换**（`*guard = new_map`）以保证原子性——这需要 `TimeWindowGuard` 暴露原子替换方法（新增，非改判定逻辑）。见 Q2。

---

## 6. 校验与 fail-closed（需审批的开放问题）

| # | 问题 | 选项 |
|---|------|------|
| Q1 | **未知结算时间的 token 是否拒绝开仓？** | (a) 拒绝（严格 fail-closed，但若注册表推送滞后会误杀正常下单）；(b) 放行 + 显著告警（现状）；(c) 折中：**启动后首次成功推送前**对「未知 token」拒绝，推送成功后未知 token 视为「本就不在监控集合」放行（推荐？） |
| Q2 | `replace_all` 语义 | (a) 只增不删（最简单，无需动 `risk_engine.rs`）；(b) 原子整体替换（需新增 getter/方法）。已结束市场残留会长期占用内存但无害 |
| Q3 | Python 推送失败时 | (a) 仅告警继续（Rust 用旧值，偏保守）；(b) 触发 TradingGate 降级（最强 fail-closed，涉及 G2 已有的降级通道） |
| Q4 | 时间格式 | 统一 RFC3339（含时区）；秒级精度。是否允许无时区（按 UTC 解释）需定 |
| Q5 | 回传 `settlement_times` 是否含历史/已结算 | 建议仅回传「未来结算」集合，避免无界增长 |

> **需伯森先生明确 Q1 的处置口径**（原 `MASTER_TASKS` G1 Acceptance 已标注此点）。

---

## 7. 测试计划

**Rust（`grpc/service.rs` tests）**
- `test_update_settlement_times_accepts_valid`：合法 ISO8601 → `accepted>0`，`RiskEngine` 生效（随后 `check_and_reserve` 在窗口内 Buy 被拒、Sell 放行）。
- `test_update_settlement_times_rejects_invalid`：空 token / 非法时间 → `rejected>0`，不写坏数据。
- `test_get_risk_state_returns_registered_settlement_times`：注册后 `to_proto` 回传非空。
- 鉴别力：临时短路写入 → 上述至少 2 个必 FAILED。

**Python（`test_grpc.py` 或新 pytest 用例）**
- converter 往返：`risk_state_to_proto` / `from_proto` 的 `settlement_times` 保真。
- `client.update_settlement_times` 组装正确（可用 mock server 断言收到条目数）。

**集成**
- mock server + 客户端：推送 → `GetRiskState` 可见 → Python `risk_gate` 窗口判定生效。

---

## 8. 影响面与风险

| 风险 | 说明 | 缓解 |
|------|------|------|
| proto 变更回归 | 需重新生成 py/rs，版本对齐（C-009） | CI 跑 `cargo test` + `pytest` |
| 动 `risk_engine.rs` | G1 Allowed Files 当前 **Forbidden** 含 risk_engine.rs | 本方案若需新增只读 getter，须在 `MASTER_TASKS` 显式追加 Allowed 并说明「不改判定/阈值」 |
| fail-closed 误杀 | Q1 选严格拒绝且推送滞后 → 正常单被拒 | 需 Q1 拍板；可加「启动宽限 + 推送成功前保守」 |
| 安全 | RPC 无鉴权（本地 gRPC 环回）——与现有一致 | 生产 UDS + 网络隔离 |

---

## 9. 待审批清单（请逐条回复）

1. 是否批准本方案（proto 新增 RPC + Rust handler + Python 推送）？
2. Q1 未知结算时间处置口径（a/b/c）？
3. Q2 `replace_all` 语义（是否允许为原子替换新增 `risk_engine.rs` 只读/替换方法）？
4. Q3 推送失败的 fail-closed 强度？
5. 是否同意将 `fast/risk_engine.rs` 加入本任务 Allowed Files（**仅新增 getter/原子替换，不改判定与阈值**）？

审批通过前，本方案不进入实施。
