# 设计方案：UpdateSettlementTimes RPC（P0-RISK-002 结算窗口）

- **状态**：**Draft v2** — 已按独立审查意见（CHANGES_REQUESTED）修改，**待第二轮独立审查**。未通过前不得实施。
- **作者**：OpenCode（工程实施者）
- **日期**：2026-09-17
- **关联**：`MASTER_TASKS.md` G1-002、`BUG_TRACKER.md` P0-RISK-002、`audits/round_04/CONSENSUS_ROUND_04.md`
- **依赖接口变更**：是（proto 新增 RPC）。须先审批方案再实施。
- **证据基线**：本文所有 `file:line` 引用基于 commit `5504579`；**未经独立审查逐条确认**，第二轮审查应核对。

---

## 0. 独立审查意见 → 修改对照（v1 → v2）

| 审查要求 | 本文位置 | 处理 |
|----------|----------|------|
| Unknown Settlement Time 必须冻结为风控语义，不得由 OpenCode 决定 | §6 | 设计基线定为 **fail-closed（拒绝新开仓）**，并列为 **Human 必批 Acceptance**；移除 v1 的折中推荐 c |
| `replace_all` 必须 atomic snapshot replacement，禁止 clear+insert | §7 | 改为「构造新 HashMap → 校验 → 整体替换」；如动 `risk_engine.rs` 限最小 Allowed |
| `success / accepted / rejected` 精确语义 | §3.1 | `success` = 请求级执行结果；`accepted/rejected` = 条目级统计。已冻结 |
| 增加 settlement registry freshness | §8 | 新增 Freshness 状态 A/B/C |
| RPC 连续失败 & snapshot 过期行为 | §8 | 明确三态与交易闸门行为 |
| expired/settled token 生命周期与 cleanup 条件 | §9 | 新增五态生命周期，仅 CLEANUP_ELIGIBLE 可删 |
| 增加 concurrent atomic replace 测试 | §10 | 新增 `test_settlement_snapshot_atomic_replace` |
| 明确 `risk_engine.rs` 最小 Allowed | §11 | 仅 snapshot getter + atomic replace + 直接相关最小辅助 |
| 明确禁止修改项 | §11 | settlement window / Risk Model threshold / strategy / position limit / funds-control |

---

## 1. 问题（现状与证据）

结算前「冻结窗口禁开新仓」逻辑**已实现但两端都未接通**：

1. **Rust**：`RiskEngine` → `TimeWindowGuard::pre_check`（`fast/risk_engine.rs:456-483`）逻辑完整；注册入口 `set_settlement`（`:443-448` / `RiskEngine::set_settlement` `:1115-1121`）**无生产调用**。
2. **Rust**：`get_risk_state` 的 `RiskState.settlement_times` 恒为空——`RiskStateInternal::to_proto()` 写死 `settlement_times: Vec::new()`（`grpc/service.rs:224`）。
3. **Python**：`risk_gate.py:80-105` 读 `state.settlement_times`，但仅由 `converter.risk_state_from_proto`（`converter.py:156-165`）从 Rust 填充；Rust 恒空 → Python 闸门同样失效。
4. **缺反向通道**：现有 RPC（`proto/polymarket.proto:243-272`）只有 `GetRiskState`/`StreamRiskStateUpdates`（Rust→Python），无 Python→Rust 推送。
5. **数据源已存在**：`market_discovery/models.py:76` 的 `MarketInfo.end_date_utc`（秒），未结束盘强制要求（`:92-94`）。

**后果**：结算窗口保护完全未生效，且「未知结算时间即放行」为 fail-open，违反 FAIL-CLOSED。

---

## 2. 目标 / 非目标

**目标**
- 建立 Python→Rust 结算时间推送通道（注册表 `end_date_utc` → Rust）。
- Rust 保存权威 settlement registry，供 `TimeWindowGuard` 判定。
- `get_risk_state` 回传 registry 快照（供 Python 展示/辅助校验）。
- 推送失败、数据缺失、快照过期时行为**明确且 fail-closed**。
- 原子、幂等、并发安全。

**非目标**
- 不改 `TimeWindowGuard::pre_check` 判定逻辑与窗口阈值（`DECISIONS.md` C-008）。
- 不做市场自动发现。
- 不处理实际结算/PnL（见 G1-001，**语义严格分离**，见 §12）。

---

## 3. 方案

### 3.1 proto 变更（`proto/polymarket.proto`）

复用 `SettlementTimeEntry`（`:106-109`），新增：

```proto
message UpdateSettlementTimesRequest {
  repeated SettlementTimeEntry settlement_times = 1;
  // true = 以本次为完整快照整体替换；false = 增量 upsert
  bool replace_all = 2;
  // 生产者观测到的快照生成时间（RFC3339，秒级精度）
  string snapshot_taken_at = 3;
}

message UpdateSettlementTimesResponse {
  // success = 请求级执行结果：
  //   true  = RPC 正常处理（可能存在部分条目 rejected，见 accepted/rejected）
  //   false = 请求级失败（如未初始化 / 内部错误），未产生任何状态变更
  bool success = 1;
  // 条目级统计（与 success 正交）
  int32 accepted = 2;
  int32 rejected = 3;
  string message = 4;
  // 服务端当前 registry 版本/时间，便于生产者对齐
  string snapshot_taken_at = 5;
}
```

```proto
service ExecutionService {
  // ... 既有 RPC 不变 ...
  rpc UpdateSettlementTimes(UpdateSettlementTimesRequest)
      returns (UpdateSettlementTimesResponse);
}
```

**冻结语义（写入 Acceptance）**
- `success=true` + `rejected>0` 是合法组合：请求被处理，但部分条目非法被拒。
- `success=false` 时**不得**发生任何 registry 变更（全拒或回滚）。
- `accepted/rejected` 之和等于请求条目数（含重复 token 时按条目计）。
- 向后兼容：纯新增；旧客户端不调用无行为变化。

### 3.2 Rust 变更

1. **registry 存储**：新增 `SettlementRegistry`，持有
   - `map: RwLock<Arc<HashMap<String, u64>>>`（**不可变快照 + Arc 整体替换**，见 §7）
   - `last_success_at: RwLock<Option<Instant>>`（freshness，§8）
   - `snapshot_taken_at: RwLock<Option<SystemTime>>`
2. **handler**（`grpc/service.rs`）：
   - 逐条校验：非空 token_id、RFC3339 可解析 → Unix 秒（容忍带/不带时区，缺失时区按 UTC 并在日志标注）。
   - 非法计入 `rejected`，不写入。
   - `replace_all=true`：构造新 `HashMap`（在当前快照基础上以增量覆盖，或纯替换，见 §3.2.3），校验后**一次性 `Arc` 替换**。
   - 成功后更新 `last_success_at` / `snapshot_taken_at`。
   - 任一条目校验失败**不导致整请求失败**（success=true, rejected>0）；仅内部错误 → success=false。
3. **与 TimeWindowGuard 的接线**：handler 将同一批合法数据同步写入 `RiskEngine::set_settlements`（保持既有判定逻辑不变），**或**让 `TimeWindowGuard` 直接持有 `SettlementRegistry` 的 Arc 快照（优选后者，避免双写不一致）。最终选型见 §13 Q6。
4. **回传**：`get_risk_state` / `stream_risk_state_updates` 组装时从 registry 快照取 `settlement_times`（不放入 `RiskStateInternal` 结构本身，避免耦合）。

**3.2.3 replace_all 语义（冻结）**
- `replace_all=true`：最终 registry = 本次请求的完整合法集合（**整体替换**）。
- `replace_all=false`：在旧快照上 upsert（原子替换新 Arc）。

### 3.3 Python 变更

1. **客户端**（`strategy_engine/grpc_bridge/client.py`）新增
   ```python
   async def update_settlement_times(self, entries: dict[str, datetime],
                                     replace_all: bool = True,
                                     snapshot_taken_at: datetime | None = None) -> dict
   ```
2. **推送来源**：`market_discovery` 扫描/注册表更新后定时推送（`{yes_token_id/no_token_id: end_date_utc}`）。
3. **失败处理**：推送失败**不**本地静默——记录错误并参与 freshness 状态（§8）。

---

## 4. 数据流 / 时序

```
market_discovery registry(end_date_utc)
  │ (Python, 定时/扫描后，带 snapshot_taken_at)
  ▼
GrpcExecutionClient.update_settlement_times(entries, replace_all)
  │ gRPC
  ▼
ExecutionServiceImpl.update_settlement_times
  │ 逐条校验（token/时间格式）
  ├─ 非法 → rejected++ + warn（不写入）
  └─ 合法 → 构造新快照 → 一次性 Arc 替换 SettlementRegistry
                          │ + 更新 freshness
                          ▼
        TimeWindowGuard.pre_check(token_id, side)   ← place_order → check_and_reserve
                          ▲
                          │ get_risk_state 回传 settlement_times
                          └── Python risk_gate（辅助投影）
```

---

## 5. （合并入 §6）

---

## 6. Unknown Settlement Time 的最终处理（BLOCKER，冻结）

**设计基线（必须 Human 批准写入 Acceptance）：**

> 对任何进入 Rust 风控检查的 `token_id`，若 settlement registry 中**不存在**其结算时间，则视为「无法证明不在结算窗口」→ **拒绝新开仓（Buy）**，允许平仓（Sell）。即 **fail-closed**。

理由：P0-RISK-002 的核心缺陷就是缺失即放行；任何「未知→放行」都会复现该缺陷（独立审查 §3.1）。

**配套**
- 该行为由 registry `validity` 状态驱动（§8）：registry 未就绪/过期时的未知 token 同样拒绝。
- 已明确注册且不在窗口内的 token 正常放行。
- 提供可观测性：`get_risk_state` 暴露 registry 有效性 + 未注册 token 数，便于定位误杀。
- **仍需 Human 拍板**：是否对「历史上从未监控的 token」（非本系统交易标的）也一致拒绝——默认**一致拒绝**（更保守）；若确认调用方只会提交已注册标的，可评估放宽。见 §13 Q1。

> v1 的折中选项 c（首次同步后未知放行）**已废弃**。

---

## 7. 并发与原子替换（BLOCKER）

**禁止** `clear()` + 逐条 `insert()`（并发 `pre_check` 可观察到 `{}`/部分集合 → 瞬时 fail-open）。

**要求**：写者构造新的不可变 `HashMap`，完成全部校验后**以单次 `Arc` 交换**发布；读者每次 `pre_check` 读取**单个 Arc 快照**，只能看到「旧完整快照」或「新完整快照」。

```
Old Arc {A,B,C} ──┬── reader → {A,B,C}
                  │
         atomic swap (Arc::store / RwLock<Arc> write)
                  │
New Arc {D,E,F} ──┴── reader → {D,E,F}
```

- 不接受读者看到 `{}` / `{D}` / `{D,E}`。
- 若实现需新增 `risk_engine.rs` 方法，仅限：snapshot getter、atomic replacement、直接相关最小辅助（§11）。

---

## 8. Freshness / 失败模型（新增，缺失项）

registry 明确区分三态，交易闸门行为不同：

| 状态 | 定义 | 交易闸门行为 |
|------|------|--------------|
| **A. NEVER_SYNCED** | 进程启动后从未成功同步 | 未知 token 一律拒绝开仓（fail-closed）；Sell 放行 |
| **B. FRESH** | 有有效快照，且 `now - last_success_at ≤ freshness_window` | 按 registry 正常判定 |
| **C. STALE** | 有历史快照，但 `now - last_success_at > freshness_window` | 保守：未知 token 拒绝；已注册 token 仍按 registry 判定（偏保守），并**显著告警**；是否整体降级 TradingGate 见 §13 Q3 |
| **D. PUSH_FAILED（瞬时）** | 本次推送失败但未过期 | 沿用现有快照（仍 FRESH），记录连续失败计数；连续失败超阈值 → 进入 C 或触发降级 |

新增/暴露：
- `last_success_at`、`snapshot_taken_at`、连续失败计数、`freshness_window`（可配置）。
- `get_risk_state` 中体现 registry 状态（至少布尔 `settlement_registry_valid` + 时间戳）。

**RPC 连续失败**：超过 `max_consecutive_failures` 时是否 `trading_gate.reconcile_complete(false,false)`（停新单）——属风控语义，见 §13 Q3，需 Human 拍板；设计基线为**降级停新单**（最保守）。

---

## 9. Settlement Registry 生命周期与 Cleanup（新增）

**不得**仅凭「预计结算时间已过」删除（预计过 ≠ 已确认结算）。

```
ACTIVE ──► SETTLEMENT_WINDOW ──► EXPECTED_SETTLEMENT_PASSED
                                        │
                                        ▼
                                 ACTUALLY_SETTLED（由 G1-001 实际结算事件驱动）
                                        │
                                        ▼
                                 CLEANUP_ELIGIBLE ──► 删除
```

- 仅 `CLEANUP_ELIGIBLE`（已确认结算 + 宽限 `cleanup_grace` 之后）允许删除。
- `EXPECTED_SETTLEMENT_PASSED` 但仍未确认结算 → **保留**并告警（避免状态丢失）。
- 实际结算确认依赖 G1-001 的 settlement authority；两者**语义分离**（§12），G1-002 不自行推断已结算。
- cleanup 由后台任务执行；无 G1-001 前，已过期但未确认的条目**不删除**。

---

## 10. 测试计划

**Rust**
- `test_update_settlement_times_accepts_valid`：合法条目 → registry 生效（窗口内 Buy 拒、Sell 放行）。
- `test_update_settlement_times_rejects_invalid`：空 token / 非法时间 → `rejected>0`、`success=true`、不写坏数据。
- `test_get_risk_state_returns_registered_settlement_times`：回传非空。
- **`test_settlement_snapshot_atomic_replace`（新增，必做）**：并发读者在 `replace_all` 期间只能读到 `old` 或 `new`；断言**绝不出现空集/部分集**。
- `test_unknown_settlement_fail_closed`：未注册 token 的 Buy 被拒（状态 A / C 下）。
- `test_stale_registry_behavior`：过期后行为符合 §8 基线。
- 鉴别力：短路 atomic swap（改 clear+insert）→ atomic replace 测试必 FAILED。

**Python**
- converter 往返 settlement_times 保真；`client.update_settlement_times` 组装正确（mock server 断言条目数）。
- risk_gate：`settlement_times` 生效时窗口内拒单。

**并发**
- 多线程 `replace_all` × 并发 `pre_check`，无「空集可观测」。

---

## 11. 权限边界（Allowed / Forbidden，最小化）

**Allowed（若最终需动）**
- `proto/polymarket.proto`（新增 message/RPC）
- `grpc/service.rs`（handler + 回传组装）
- `bin/server.rs`（后台 freshness/cleanup 接线）
- `strategy_engine/grpc_bridge/client.py`、`converter.py`、`market_discovery`（推送）
- `fast/risk_engine.rs` —— **仅限**：
  - settlement snapshot getter（只读）；
  - atomic replacement（整体替换）；
  - 与上述直接相关的最小辅助方法。

**Forbidden**
- 修改 `TimeWindowGuard` 判定逻辑 / settlement window 长度；
- 修改 Risk Model threshold；
- 修改 strategy；
- 修改 position limit；
- 修改 funds-control semantics；
- 将 settlement registry 与 actual PnL/结算状态混为一体（见 §12）。

---

## 12. 与 G1-001 的边界（语义分离）

| | G1-002（本方案） | G1-001 |
|---|---|---|
| 数据 | **预计**结算时间 `end_date_utc` | **实际**结算事件 + outcome |
| 用途 | 临近结算禁止开仓 | realized PnL → DailyLoss |
| 共享 | 仅可共享 token 生命周期基础设施 | 同 |

**禁止**把二者合并成单一含义模糊的「settlement 状态」。

---

## 13. 待 Human 拍板的开放问题（第二轮审查一并确认）

1. **Q1**：未注册（从未监控）token 是否一律拒绝开仓？基线=是。
2. **Q2**：`replace_all` 是否要求 atomic snapshot replacement（基线=是，且为此申请 `risk_engine.rs` 最小 Allowed）。
3. **Q3**：RPC 连续失败/快照过期是否降级 TradingGate（停新单）？基线=是。
4. **Q4**：时间格式统一 RFC3339；无时区按 UTC（同意？）。
5. **Q5**：`freshness_window` / `max_consecutive_failures` / `cleanup_grace` 具体取值（需给数值）。
6. **Q6**：registry 归属——由 `TimeWindowGuard` 直接持有 Arc 快照（避免双写），还是 `ExecutionServiceImpl` 持有后同步写入 `RiskEngine::set_settlements`？请选定。
7. **Q7**：回传是否仅含 ACTIVE/SETTLEMENT_WINDOW（避免无界）？基线=是。

---

## 14. 结论

Draft v2 已落实独立审查的 **9 项 Required Changes**（§0 对照）。请进行第二轮独立审查；通过并获 Human 批准前，**不进入实施**。
